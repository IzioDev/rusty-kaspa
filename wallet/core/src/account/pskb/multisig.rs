use crate::account::multisig::MultiSig;
use crate::account::pskb::pskt_to_pending_transaction;
use crate::account::{Account, DerivationCapableAccount, SignatureSchemeCapableAccount};
use crate::derivation::{AddressDerivationConfig, MultisigDerivationScheme, build_derivate_path};
use crate::error::Error;
use crate::result::Result;
use crate::tx::{Fees, Generator, GeneratorSettings, PaymentDestination};
use kaspa_bip32::{AddressType, ChildNumber, DerivationPath, PrivateKey};
use kaspa_consensus_core::tx::TransactionId;
use kaspa_txscript::MultisigRedeemScriptContext;
use kaspa_txscript::{
    extract_script_pub_key_address, multisig_redeem_script, multisig_redeem_script_ecdsa, opcodes::codes::OpData65,
    parse_multisig_redeem_script, pay_to_script_hash_script, script_builder::ScriptBuilder,
};
use kaspa_wallet_keys::derivation::gen1::WalletDerivationManager;
use kaspa_wallet_keys::derivation::traits::{PubkeyDerivationManagerTrait, WalletDerivationManagerTrait};
use kaspa_wallet_keys::secret::Secret;
use kaspa_wallet_pskt::prelude::{
    Bundle, Error as PsktError, FinalizeError, Finalizer, KeySource, PSKT, Signature, SignatureScheme, Signer, Updater,
};
use secp256k1::{Message, PublicKey};
use std::{
    collections::{BTreeMap, BTreeSet},
    iter,
    ops::Deref,
    sync::Arc,
};
use workflow_core::abortable::Abortable;

/// True when a partial signature's curve matches the multisig account scheme:
/// an ECDSA account carries `Signature::ECDSA`, a Schnorr account `Signature::Schnorr`.
fn signature_matches_scheme(signature: &Signature, is_ecdsa: bool) -> bool {
    match signature {
        Signature::ECDSA(_) => is_ecdsa,
        Signature::Schnorr(_) => !is_ecdsa,
    }
}

/// Assemble a P2SH multisig script-sig: each selected signature pushed as
/// `OpData65 || sig(64) || sighash_type(1)` in redeem-key order, followed by the redeem
/// script push. The framing is curve-agnostic: each signature serializes to a fixed 64 bytes
/// (ECDSA compact r||s, never DER; Schnorr native 64-byte), so every push is exactly 64
/// signature bytes plus one sighash-type byte and the consensus multisig verifier consumes a
/// fixed-width element.
fn assemble_script_sig(selected: &[Signature], redeem_script: &[u8], sighash_type: u8) -> Result<Vec<u8>> {
    let mut script = Vec::new();
    for signature in selected {
        script.extend(iter::once(OpData65).chain(signature.into_bytes()).chain([sighash_type]));
    }
    script.extend(ScriptBuilder::new().add_data(redeem_script)?.drain());
    Ok(script)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MultisigInputAddressDerivation {
    input_index: usize,
    derivation_scheme: MultisigDerivationScheme,
    address_type: AddressType,
    address_index: u32,
}

impl MultisigInputAddressDerivation {
    fn new(input_index: usize, derivation_scheme: MultisigDerivationScheme, address_type: AddressType, address_index: u32) -> Self {
        Self { input_index, derivation_scheme, address_type, address_index }
    }

    fn receive(input_index: usize, derivation_scheme: MultisigDerivationScheme, address_index: u32) -> Self {
        Self::new(input_index, derivation_scheme, AddressType::Receive, address_index)
    }

    fn change(input_index: usize, derivation_scheme: MultisigDerivationScheme, address_index: u32) -> Self {
        Self::new(input_index, derivation_scheme, AddressType::Change, address_index)
    }

    fn derivation_path(&self) -> Result<DerivationPath> {
        let mut path = DerivationPath::default();
        if let MultisigDerivationScheme::Kaspa45 { cosigner_index } = self.derivation_scheme {
            path.push(ChildNumber::new(cosigner_index.unwrap_or(0), false)?);
        }
        path.push(ChildNumber::new(self.address_type.into(), false)?);
        path.push(ChildNumber::new(self.address_index, false)?);
        Ok(path)
    }

    fn from_path(account_scheme: MultisigDerivationScheme, input_index: usize, path: &DerivationPath) -> Result<Self> {
        let children = path.iter().collect::<Vec<_>>();
        if children.iter().any(ChildNumber::is_hardened) {
            return Err(Error::InvalidMultisigDerivationPath(path.to_string()));
        }

        let (derivation_scheme, keychain, address_index) = match account_scheme {
            MultisigDerivationScheme::Bip87 => {
                let [keychain, address_index] = children.as_slice() else {
                    return Err(Error::InvalidMultisigDerivationPath(path.to_string()));
                };
                (MultisigDerivationScheme::Bip87, keychain.index(), address_index.index())
            }
            MultisigDerivationScheme::Kaspa45 { .. } => {
                let [branch, keychain, address_index] = children.as_slice() else {
                    return Err(Error::InvalidMultisigDerivationPath(path.to_string()));
                };
                (MultisigDerivationScheme::kaspa45(Some(branch.index())), keychain.index(), address_index.index())
            }
        };

        let address_type = AddressType::try_from(keychain)?;

        Ok(Self::new(input_index, derivation_scheme, address_type, address_index))
    }

    fn from_bip32_derivations(
        account_scheme: MultisigDerivationScheme,
        pskt_index: usize,
        input_index: usize,
        bip32_derivations: &BTreeMap<PublicKey, Option<KeySource>>,
        expected_derivation_count: usize,
    ) -> Result<Self> {
        if bip32_derivations.len() != expected_derivation_count {
            return Err(Error::InvalidMultisigDerivationCount {
                pskt_index,
                input_index,
                expected: expected_derivation_count,
                actual: bip32_derivations.len(),
            });
        }

        let mut path = None;
        for key_source in bip32_derivations.values() {
            let key_source = key_source.as_ref().ok_or(Error::MissingMultisigKeySource { pskt_index, input_index })?;
            match &path {
                Some(existing) if existing != &key_source.derivation_path => {
                    return Err(Error::ConflictingMultisigDerivationPath { pskt_index, input_index });
                }
                Some(_) => {}
                None => path = Some(key_source.derivation_path.clone()),
            }
        }

        let path = path.ok_or(Error::MissingMultisigKeySource { pskt_index, input_index })?;
        Self::from_path(account_scheme, input_index, &path)
    }
}

#[derive(Clone)]
struct MultisigInputSignerKey {
    public_key: PublicKey,
    key_source: KeySource,
}

#[derive(Clone)]
struct MultisigInputSpendPlan {
    input_index: usize,
    address_derivation: MultisigInputAddressDerivation,
    signer_keys: BTreeMap<[u8; 32], MultisigInputSignerKey>,
    redeem_script: Vec<u8>,
}

impl MultisigInputSpendPlan {
    fn x_only_key_id(public_key: &PublicKey) -> [u8; 32] {
        public_key.x_only_public_key().0.serialize()
    }

    fn signer_key(&self, public_key: &PublicKey) -> Option<&MultisigInputSignerKey> {
        self.signer_keys.get(&Self::x_only_key_id(public_key))
    }

    fn verify_bip32_derivations(&self, pskt_index: usize, bip32_derivations: &BTreeMap<PublicKey, Option<KeySource>>) -> Result<()> {
        if bip32_derivations.len() != self.signer_keys.len() {
            return Err(Error::InvalidMultisigDerivationCount {
                pskt_index,
                input_index: self.input_index,
                expected: self.signer_keys.len(),
                actual: bip32_derivations.len(),
            });
        }

        for key in self.signer_keys.values() {
            match bip32_derivations.get(&key.public_key) {
                Some(Some(key_source)) if key_source == &key.key_source => {}
                Some(Some(_)) => {
                    return Err(Error::ConflictingMultisigDerivationPath { pskt_index, input_index: self.input_index });
                }
                Some(None) | None => {
                    return Err(Error::MissingMultisigKeySource { pskt_index, input_index: self.input_index });
                }
            }
        }

        Ok(())
    }
}

type PubkeyManagers = Vec<Arc<dyn PubkeyDerivationManagerTrait>>;

impl MultiSig {
    pub async fn pskb_from_multisig(
        self: Arc<Self>,
        destination: PaymentDestination,
        fee_rate: Option<f64>,
        priority_fee_sompi: Fees,
        payload: Option<Vec<u8>>,
        abortable: &Abortable,
    ) -> Result<Bundle> {
        let (receive_pubkey_managers, _) = self.pubkey_managers_for_derivation(self.derivation_scheme())?;
        let sig_op_count = receive_pubkey_managers.len().try_into().map_err(|_| Error::InvalidMultisigSignerCount)?;

        let settings =
            GeneratorSettings::try_new_with_account(self.clone().as_dyn_arc(), destination, fee_rate, priority_fee_sompi, payload)?;
        let generator = Generator::try_new(settings, None, Some(abortable))?;
        let raw_bundle = generator.try_into_pskt_bundle().await?;

        let mut multisig_bundle = Bundle::new();
        for inner in raw_bundle.iter().cloned() {
            let mut pskt: PSKT<Updater> = PSKT::from(inner);
            let mut input_address_derivations = BTreeMap::new();

            for (input_index, input) in pskt.inputs.iter().enumerate().filter(|(_, input)| input.final_script_sig.is_none()) {
                let utxo_entry = input.utxo_entry.as_ref().ok_or(PsktError::MissingUtxoEntry)?;
                let address = extract_script_pub_key_address(&utxo_entry.script_public_key, self.wallet().address_prefix()?)?;
                let derivation_scheme = self.derivation_scheme();
                let (receive, change) = self.derivation().addresses_indexes(&[&address])?;

                let address_derivation = if let Some((_, address_index)) = receive.first() {
                    MultisigInputAddressDerivation::receive(input_index, derivation_scheme, *address_index)
                } else if let Some((_, address_index)) = change.first() {
                    MultisigInputAddressDerivation::change(input_index, derivation_scheme, *address_index)
                } else {
                    unreachable!("addresses_indexes returns receive or change index for a known address")
                };

                input_address_derivations.insert(input_index, address_derivation);
            }
            let spend_plans = self.build_input_spend_plans(&pskt, PlanningMode::Annotate, &input_address_derivations)?;

            for spend_plan in &spend_plans {
                pskt = pskt.set_input_redeem_script(spend_plan.input_index, spend_plan.redeem_script.clone())?;
                pskt = pskt.set_input_sig_op_count(spend_plan.input_index, sig_op_count)?;
                for signer_key in spend_plan.signer_keys.values() {
                    pskt = pskt.add_input_bip32_derivation(
                        spend_plan.input_index,
                        signer_key.public_key,
                        Some(signer_key.key_source.clone()),
                    )?;
                }
            }

            multisig_bundle.add_pskt(pskt.signer());
        }

        Ok(multisig_bundle)
    }

    pub async fn sign_multisig_pskb(
        self: Arc<Self>,
        bundle: &Bundle,
        wallet_secret: Secret,
        payment_secret: Option<Secret>,
    ) -> Result<Bundle> {
        let key_data = self.clone().as_dyn_arc().prv_key_data_list(wallet_secret).await?;
        if key_data.is_empty() {
            return Err(Error::NoMatchingLocalSigner);
        }
        let xpub_count = self.xpub_keys().len();

        let mut signed_bundle = Bundle::new();
        let mut matched_any = false;

        for (pskt_index, inner) in bundle.iter().cloned().enumerate() {
            let pskt: PSKT<Signer> = PSKT::from(inner);
            let mut input_address_derivations = BTreeMap::new();
            for (input_index, input) in pskt.inputs.iter().enumerate().filter(|(_, input)| input.final_script_sig.is_none()) {
                let address_derivation = MultisigInputAddressDerivation::from_bip32_derivations(
                    self.derivation_scheme(),
                    pskt_index,
                    input_index,
                    &input.bip32_derivations,
                    xpub_count,
                )?;
                input_address_derivations.insert(input_index, address_derivation);
            }

            let spend_plans = self.build_input_spend_plans(&pskt, PlanningMode::Verify, &input_address_derivations)?;
            let signature_scheme = if self.is_ecdsa() { SignatureScheme::ECDSA } else { SignatureScheme::Schnorr };
            let signature_hashes = pskt.signature_hashes(signature_scheme)?;
            let mut signatures = Vec::new();
            let mut signed_keys = pskt
                .inputs
                .iter()
                .enumerate()
                .flat_map(|(input_index, input)| input.partial_sigs.keys().map(move |public_key| (input_index, *public_key)))
                .collect::<BTreeSet<_>>();

            for spend_plan in &spend_plans {
                let message = Message::from_digest_slice(signature_hashes[spend_plan.input_index].as_bytes().as_slice())?;

                for (signature_public_key, signature) in pskt.inputs[spend_plan.input_index].partial_sigs.iter() {
                    if spend_plan.signer_key(signature_public_key).is_none() {
                        return Err(Error::InvalidPartialSignature);
                    }
                    if !signature_matches_scheme(signature, self.is_ecdsa()) {
                        return Err(Error::UnsupportedSignatureType);
                    }
                    signature.verify(&message, signature_public_key).map_err(|_| Error::InvalidPartialSignature)?;
                }
                spend_plan.verify_bip32_derivations(pskt_index, &pskt.inputs[spend_plan.input_index].bip32_derivations)?;

                for key_data in &key_data {
                    let xkey = key_data.get_xprv(payment_secret.as_ref())?;
                    let config = AddressDerivationConfig::multisig(0, spend_plan.address_derivation.derivation_scheme);
                    let path = build_derivate_path(config, spend_plan.address_derivation.address_type)?;
                    let account_child = xkey.clone().derive_path(&path)?;
                    let private_key = account_child
                        .derive_child(ChildNumber::new(spend_plan.address_derivation.address_index, false)?)?
                        .private_key()
                        .to_bytes();
                    let keypair = secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, &private_key)?;
                    let public_key = keypair.public_key();
                    let Some(signer_key) = spend_plan.signer_key(&public_key) else {
                        continue;
                    };
                    matched_any = true;
                    if !signed_keys.insert((spend_plan.input_index, public_key)) {
                        continue;
                    }
                    let signature = if self.is_ecdsa() {
                        Signature::ECDSA(keypair.secret_key().sign_ecdsa(message))
                    } else {
                        Signature::Schnorr(keypair.sign_schnorr(message))
                    };
                    signature.verify(&message, &public_key).map_err(|_| Error::InvalidPartialSignature)?;
                    signatures.push((
                        spend_plan.input_index,
                        kaspa_wallet_pskt::pskt::SignInputOk {
                            signature,
                            pub_key: public_key,
                            key_source: Some(signer_key.key_source.clone()),
                        },
                    ));
                }
            }

            signed_bundle.add_pskt(pskt.pass_partial_signatures(signatures)?);
        }

        if !matched_any {
            return Err(Error::NoMatchingLocalSigner);
        }

        Ok(signed_bundle)
    }

    pub async fn finalize_and_submit_multisig_pskb(self: Arc<Self>, bundle: &Bundle) -> Result<Vec<TransactionId>> {
        let mut ids = Vec::new();
        let finalized_bundle = self.finalize_bundle(bundle)?;

        for inner in finalized_bundle.iter().cloned() {
            let finalized: PSKT<Finalizer> = PSKT::from(inner);
            let transaction = pskt_to_pending_transaction(
                finalized,
                self.wallet().network_id()?,
                self.change_address()?,
                self.utxo_context().clone().into(),
            )?;
            ids.push(transaction.try_submit(&self.wallet().rpc_api()).await?);
        }

        Ok(ids)
    }

    fn pubkey_managers_for_derivation(&self, derivation_scheme: MultisigDerivationScheme) -> Result<(PubkeyManagers, PubkeyManagers)> {
        let xpub_keys = self.xpub_keys();
        if xpub_keys.is_empty() {
            return Err(Error::MultisigXpubRequired);
        }
        if self.minimum_signatures() == 0 {
            return Err(Error::InvalidMultisigThreshold);
        }
        if self.minimum_signatures() as usize > xpub_keys.len() {
            return Err(Error::InvalidMultisigThreshold);
        }

        let xpub_child_branch = derivation_scheme.xpub_child_branch();
        let mut receive_pubkey_managers = Vec::with_capacity(xpub_keys.len());
        let mut change_pubkey_managers = Vec::with_capacity(xpub_keys.len());

        for xpub in xpub_keys.iter() {
            let derivation = WalletDerivationManager::from_extended_public_key(xpub.clone(), xpub_child_branch)?;
            receive_pubkey_managers.push(WalletDerivationManagerTrait::receive_pubkey_manager(&derivation));
            change_pubkey_managers.push(WalletDerivationManagerTrait::change_pubkey_manager(&derivation));
        }

        Ok((receive_pubkey_managers, change_pubkey_managers))
    }

    fn finalize_bundle(&self, bundle: &Bundle) -> Result<Bundle> {
        let mut finalized_bundle = Bundle::new();

        for inner in bundle.iter().cloned() {
            let finalizer: PSKT<Finalizer> = PSKT::<Signer>::from(inner).finalizer();
            finalized_bundle.add_pskt(Self::finalize_pskt(finalizer)?);
        }

        Ok(finalized_bundle)
    }

    fn build_input_spend_plans<R>(
        &self,
        pskt: &PSKT<R>,
        mode: PlanningMode,
        input_address_derivations: &BTreeMap<usize, MultisigInputAddressDerivation>,
    ) -> Result<Vec<MultisigInputSpendPlan>> {
        pskt.inputs
            .iter()
            .enumerate()
            .filter(|(_, input)| input.final_script_sig.is_none())
            .map(|(input_index, input)| {
                let address_derivation =
                    input_address_derivations.get(&input_index).copied().ok_or(Error::MissingMultisigInputDerivation(input_index))?;

                match (self.derivation_scheme(), address_derivation.derivation_scheme) {
                    (MultisigDerivationScheme::Bip87, MultisigDerivationScheme::Bip87)
                    | (MultisigDerivationScheme::Kaspa45 { .. }, MultisigDerivationScheme::Kaspa45 { .. }) => {}
                    _ => return Err(Error::InvalidMultisigDerivationScheme),
                }

                let utxo_entry = input.utxo_entry.as_ref().ok_or(PsktError::MissingUtxoEntry)?;
                let public_keys = {
                    let (receive_pubkey_managers, change_pubkey_managers) =
                        self.pubkey_managers_for_derivation(address_derivation.derivation_scheme)?;
                    let managers = match address_derivation.address_type {
                        AddressType::Receive => receive_pubkey_managers.as_slice(),
                        AddressType::Change => change_pubkey_managers.as_slice(),
                    };
                    managers
                        .iter()
                        .map(|manager| {
                            manager
                                .get_range(address_derivation.address_index..address_derivation.address_index + 1)?
                                .into_iter()
                                .next()
                                .ok_or_else(|| Error::MissingMultisigPublicKey(address_derivation.address_index))
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                let derivation_path = address_derivation.derivation_path()?;
                let signer_keys = public_keys
                    .iter()
                    .copied()
                    .zip(self.xpub_keys().iter().map(|xpub| KeySource::new(xpub.fingerprint(), derivation_path.clone())))
                    .map(|(public_key, key_source)| {
                        (MultisigInputSpendPlan::x_only_key_id(&public_key), MultisigInputSignerKey { public_key, key_source })
                    })
                    .collect();
                let required = self.minimum_signatures() as usize;
                let redeem_script = if self.is_ecdsa() {
                    multisig_redeem_script_ecdsa(public_keys.iter().map(|public_key| public_key.serialize()), required)?
                } else {
                    multisig_redeem_script(
                        public_keys.iter().map(|public_key| public_key.x_only_public_key().0.serialize()),
                        required,
                    )?
                };

                if pay_to_script_hash_script(&redeem_script) != utxo_entry.script_public_key {
                    return Err(Error::InvalidMultisigRedeemScript);
                }

                let spend_plan = MultisigInputSpendPlan {
                    input_index: address_derivation.input_index,
                    address_derivation,
                    signer_keys,
                    redeem_script,
                };

                if matches!(mode, PlanningMode::Verify) {
                    let input_redeem_script = input.redeem_script.as_ref().ok_or(Error::MissingRedeemScript)?;
                    if input_redeem_script != &spend_plan.redeem_script {
                        return Err(Error::InvalidMultisigRedeemScript);
                    }
                }

                Ok(spend_plan)
            })
            .collect()
    }

    fn finalize_pskt(pskt: PSKT<Finalizer>) -> Result<PSKT<Finalizer>> {
        // finalize_pskt has no account handle, so the finalize-side sighash domain is read
        // from each input's parsed redeem context: OpCheckMultiSigECDSA verifies under the
        // ECDSA sighash domain, so an ECDSA input must be finalized against the ECDSA digest
        // and a Schnorr input against the Schnorr digest. Both digest sets are computed up
        // front and selected per input below.
        let signer = PSKT::<Signer>::from(pskt.deref().clone());
        let schnorr_hashes = signer.signature_hashes(SignatureScheme::Schnorr)?;
        let ecdsa_hashes = signer.signature_hashes(SignatureScheme::ECDSA)?;

        match pskt.finalize_sync(|inner| {
            if schnorr_hashes.len() != inner.inputs.len() {
                return Err(Error::Custom(format!(
                    "Signature hash count mismatch: expected {}, actual {}",
                    inner.inputs.len(),
                    schnorr_hashes.len()
                )));
            }

            if ecdsa_hashes.len() != inner.inputs.len() {
                return Err(Error::Custom(format!(
                    "Signature hash count mismatch: expected {}, actual {}",
                    inner.inputs.len(),
                    ecdsa_hashes.len()
                )));
            }

            inner
                .inputs
                .iter()
                .enumerate()
                .map(|(input_index, input)| {
                    let redeem_script = input.redeem_script.as_ref().ok_or(Error::MissingRedeemScript)?;
                    let context = parse_multisig_redeem_script(redeem_script).map_err(|_| Error::InvalidMultisigRedeemScript)?;

                    let is_ecdsa = matches!(context, MultisigRedeemScriptContext::Ecdsa { .. });
                    let signature_hash = if is_ecdsa { &ecdsa_hashes[input_index] } else { &schnorr_hashes[input_index] };
                    let message = Message::from_digest_slice(signature_hash.as_bytes().as_slice())?;

                    let script = match context {
                        MultisigRedeemScriptContext::Schnorr { required, public_keys } => {
                            for (signature_public_key, signature) in input.partial_sigs.iter() {
                                if !public_keys.iter().any(|public_key| *public_key == signature_public_key.x_only_public_key().0) {
                                    return Err(Error::InvalidPartialSignature);
                                }
                                if !signature_matches_scheme(signature, is_ecdsa) {
                                    return Err(Error::UnsupportedSignatureType);
                                }
                                signature.verify(&message, signature_public_key).map_err(|_| Error::InvalidPartialSignature)?;
                            }

                            let mut selected = Vec::with_capacity(required);
                            for public_key in public_keys.iter() {
                                if let Some((_, signature)) = input
                                    .partial_sigs
                                    .iter()
                                    .find(|(signature_public_key, _)| signature_public_key.x_only_public_key().0 == *public_key)
                                {
                                    selected.push(*signature);
                                }
                                if selected.len() == required {
                                    break;
                                }
                            }

                            if selected.len() < required {
                                return Err(Error::InsufficientSignatures { required, present: selected.len() });
                            }

                            assemble_script_sig(&selected, redeem_script, input.sighash_type.to_u8())?
                        }
                        MultisigRedeemScriptContext::Ecdsa { required, public_keys } => {
                            for (signature_public_key, signature) in input.partial_sigs.iter() {
                                if !public_keys.iter().any(|public_key| public_key == signature_public_key) {
                                    return Err(Error::InvalidPartialSignature);
                                }
                                if !signature_matches_scheme(signature, is_ecdsa) {
                                    return Err(Error::UnsupportedSignatureType);
                                }
                                signature.verify(&message, signature_public_key).map_err(|_| Error::InvalidPartialSignature)?;
                            }

                            let mut selected = Vec::with_capacity(required);
                            for public_key in public_keys.iter() {
                                if let Some((_, signature)) =
                                    input.partial_sigs.iter().find(|(signature_public_key, _)| *signature_public_key == public_key)
                                {
                                    selected.push(*signature);
                                }
                                if selected.len() == required {
                                    break;
                                }
                            }

                            if selected.len() < required {
                                return Err(Error::InsufficientSignatures { required, present: selected.len() });
                            }

                            assemble_script_sig(&selected, redeem_script, input.sighash_type.to_u8())?
                        }
                    };
                    Ok(script)
                })
                .collect()
        }) {
            Ok(finalized) => Ok(finalized),
            Err(FinalizeError::FinalizeCb(error)) => Err(error),
            Err(error) => Err(Error::Custom(error.to_string())),
        }
    }
}

#[derive(Clone, Copy)]
enum PlanningMode {
    Annotate,
    Verify,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::hashing::sighash::{SigHashReusedValuesUnsync, calc_ecdsa_signature_hash, calc_schnorr_signature_hash};
    use kaspa_consensus_core::tx::{TransactionOutpoint, UtxoEntry};
    use kaspa_wallet_pskt::prelude::{Inner, InputBuilder};
    use kaspa_wallet_pskt::pskt::SignInputOk;
    use secp256k1::{Keypair, rand::thread_rng};
    use std::str::FromStr;

    fn multisig_signer_context() -> ([Keypair; 2], PSKT<Signer>) {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let redeem_script = multisig_redeem_script(kps.iter().map(|kp| kp.x_only_public_key().0.serialize()), 2).unwrap();
        let input = InputBuilder::default()
            .utxo_entry(UtxoEntry {
                amount: 12_793_000_000_000,
                script_public_key: pay_to_script_hash_script(&redeem_script),
                block_daa_score: 36_151_168,
                is_coinbase: false,
                covenant_id: None,
            })
            .previous_outpoint(TransactionOutpoint {
                transaction_id: TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap(),
                index: 0,
            })
            .sig_op_count(2)
            .redeem_script(redeem_script)
            .build()
            .unwrap();

        (kps, PSKT::<Signer>::from(Inner { inputs: vec![input], outputs: vec![], ..Default::default() }))
    }

    fn sign_with(pskt: PSKT<Signer>, kp: &Keypair) -> PSKT<Signer> {
        let reused_values = SigHashReusedValuesUnsync::new();
        pskt.pass_signature_sync(|tx, sighash| -> std::result::Result<Vec<SignInputOk>, String> {
            tx.tx
                .inputs
                .iter()
                .enumerate()
                .map(|(input_index, _)| {
                    let hash = calc_schnorr_signature_hash(&tx.as_verifiable(), input_index, sighash[input_index], &reused_values);
                    let message =
                        secp256k1::Message::from_digest_slice(hash.as_bytes().as_slice()).map_err(|error| error.to_string())?;
                    Ok(SignInputOk {
                        signature: Signature::Schnorr(kp.sign_schnorr(message)),
                        pub_key: kp.public_key(),
                        key_source: None,
                    })
                })
                .collect()
        })
        .unwrap()
    }

    #[test]
    fn multisig_input_address_derivation_path_round_trips_kaspa45() {
        let derivation = MultisigInputAddressDerivation::change(1, MultisigDerivationScheme::kaspa45(Some(2)), 42);

        let path = derivation.derivation_path().unwrap();
        let parsed = MultisigInputAddressDerivation::from_path(MultisigDerivationScheme::kaspa45(Some(0)), 1, &path).unwrap();

        assert_eq!(path.to_string(), "m/2/1/42");
        assert_eq!(parsed, derivation);
    }

    #[test]
    fn multisig_input_address_derivation_path_round_trips_bip87() {
        let derivation = MultisigInputAddressDerivation::receive(1, MultisigDerivationScheme::Bip87, 42);

        let path = derivation.derivation_path().unwrap();
        let parsed = MultisigInputAddressDerivation::from_path(MultisigDerivationScheme::Bip87, 1, &path).unwrap();

        assert_eq!(path.to_string(), "m/0/42");
        assert_eq!(parsed, derivation);
    }

    #[test]
    fn multisig_input_address_derivation_reads_complete_bip32_metadata() {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let path = DerivationPath::from_str("m/2/0/42").unwrap();
        let derivations = BTreeMap::from([
            (kps[0].public_key(), Some(KeySource::new([1, 2, 3, 4], path.clone()))),
            (kps[1].public_key(), Some(KeySource::new([5, 6, 7, 8], path))),
        ]);

        let derivation =
            MultisigInputAddressDerivation::from_bip32_derivations(MultisigDerivationScheme::kaspa45(Some(0)), 0, 1, &derivations, 2)
                .unwrap();

        assert_eq!(derivation, MultisigInputAddressDerivation::receive(1, MultisigDerivationScheme::kaspa45(Some(2)), 42));
    }

    #[test]
    fn multisig_input_address_derivation_rejects_incomplete_bip32_metadata() {
        let kp = Keypair::new(secp256k1::SECP256K1, &mut thread_rng());
        let path = DerivationPath::from_str("m/2/0/42").unwrap();
        let derivations = BTreeMap::from([(kp.public_key(), Some(KeySource::new([1, 2, 3, 4], path)))]);

        let result =
            MultisigInputAddressDerivation::from_bip32_derivations(MultisigDerivationScheme::kaspa45(Some(0)), 0, 1, &derivations, 2);

        assert!(matches!(
            result,
            Err(Error::InvalidMultisigDerivationCount { pskt_index: 0, input_index: 1, expected: 2, actual: 1 })
        ));
    }

    #[test]
    fn multisig_input_address_derivation_rejects_missing_key_source() {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let path = DerivationPath::from_str("m/2/0/42").unwrap();
        let derivations =
            BTreeMap::from([(kps[0].public_key(), Some(KeySource::new([1, 2, 3, 4], path))), (kps[1].public_key(), None)]);

        let result =
            MultisigInputAddressDerivation::from_bip32_derivations(MultisigDerivationScheme::kaspa45(Some(0)), 0, 1, &derivations, 2);

        assert!(matches!(result, Err(Error::MissingMultisigKeySource { pskt_index: 0, input_index: 1 })));
    }

    #[test]
    fn multisig_input_address_derivation_rejects_conflicting_paths() {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let derivations = BTreeMap::from([
            (kps[0].public_key(), Some(KeySource::new([1, 2, 3, 4], DerivationPath::from_str("m/2/0/42").unwrap()))),
            (kps[1].public_key(), Some(KeySource::new([5, 6, 7, 8], DerivationPath::from_str("m/2/1/42").unwrap()))),
        ]);

        let result =
            MultisigInputAddressDerivation::from_bip32_derivations(MultisigDerivationScheme::kaspa45(Some(0)), 0, 1, &derivations, 2);

        assert!(matches!(result, Err(Error::ConflictingMultisigDerivationPath { pskt_index: 0, input_index: 1 })));
    }

    #[test]
    fn multisig_input_spend_plan_verifies_exact_bip32_metadata_keys() {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let path = DerivationPath::from_str("m/2/0/42").unwrap();
        let key_source_0 = KeySource::new([1, 2, 3, 4], path.clone());
        let key_source_1 = KeySource::new([5, 6, 7, 8], path);
        let spend_plan = MultisigInputSpendPlan {
            input_index: 1,
            address_derivation: MultisigInputAddressDerivation::receive(1, MultisigDerivationScheme::kaspa45(Some(2)), 42),
            signer_keys: BTreeMap::from([
                (
                    MultisigInputSpendPlan::x_only_key_id(&kps[0].public_key()),
                    MultisigInputSignerKey { public_key: kps[0].public_key(), key_source: key_source_0.clone() },
                ),
                (
                    MultisigInputSpendPlan::x_only_key_id(&kps[1].public_key()),
                    MultisigInputSignerKey { public_key: kps[1].public_key(), key_source: key_source_1.clone() },
                ),
            ]),
            redeem_script: vec![],
        };
        let bip32_derivations = BTreeMap::from([
            (kps[0].public_key(), Some(key_source_0)),
            (Keypair::new(secp256k1::SECP256K1, &mut thread_rng()).public_key(), Some(key_source_1)),
        ]);

        assert!(matches!(
            spend_plan.verify_bip32_derivations(0, &bip32_derivations),
            Err(Error::MissingMultisigKeySource { pskt_index: 0, input_index: 1 })
        ));
    }

    #[test]
    fn finalize_pskt_orders_and_clears_partial_signatures() {
        let (kps, pskt) = multisig_signer_context();
        let signed_0 = sign_with(pskt.clone(), &kps[0]);
        let signed_1 = sign_with(pskt.clone(), &kps[1]);
        let combined = pskt.combiner().combine(signed_1).unwrap().combine(signed_0).unwrap();

        let finalized = MultiSig::finalize_pskt(combined.finalizer()).unwrap();

        let input = &finalized.inputs[0];
        assert!(input.final_script_sig.as_ref().is_some_and(|script| !script.is_empty()));
        assert!(input.partial_sigs.is_empty());
        assert!(input.redeem_script.is_none());
        assert!(input.bip32_derivations.is_empty());
    }

    #[test]
    fn finalize_pskt_rejects_insufficient_signatures() {
        let (kps, pskt) = multisig_signer_context();
        let signed = sign_with(pskt, &kps[0]);

        let result = MultiSig::finalize_pskt(signed.finalizer());

        assert!(matches!(result, Err(Error::InsufficientSignatures { required: 2, present: 1 })));
    }

    fn multisig_signer_context_ecdsa() -> ([Keypair; 2], PSKT<Signer>) {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let redeem_script = multisig_redeem_script_ecdsa(kps.iter().map(|kp| kp.public_key().serialize()), 2).unwrap();
        let input = InputBuilder::default()
            .utxo_entry(UtxoEntry {
                amount: 12_793_000_000_000,
                script_public_key: pay_to_script_hash_script(&redeem_script),
                block_daa_score: 36_151_168,
                is_coinbase: false,
                covenant_id: None,
            })
            .previous_outpoint(TransactionOutpoint {
                transaction_id: TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap(),
                index: 0,
            })
            .sig_op_count(2)
            .redeem_script(redeem_script)
            .build()
            .unwrap();

        (kps, PSKT::<Signer>::from(Inner { inputs: vec![input], outputs: vec![], ..Default::default() }))
    }

    fn sign_with_ecdsa(pskt: PSKT<Signer>, kp: &Keypair) -> PSKT<Signer> {
        let reused_values = SigHashReusedValuesUnsync::new();
        pskt.pass_signature_sync(|tx, sighash| -> std::result::Result<Vec<SignInputOk>, String> {
            tx.tx
                .inputs
                .iter()
                .enumerate()
                .map(|(input_index, _)| {
                    let hash = calc_ecdsa_signature_hash(&tx.as_verifiable(), input_index, sighash[input_index], &reused_values);
                    let message =
                        secp256k1::Message::from_digest_slice(hash.as_bytes().as_slice()).map_err(|error| error.to_string())?;
                    Ok(SignInputOk {
                        signature: Signature::ECDSA(kp.secret_key().sign_ecdsa(message)),
                        pub_key: kp.public_key(),
                        key_source: None,
                    })
                })
                .collect()
        })
        .unwrap()
    }

    #[test]
    fn finalize_pskt_ecdsa_orders_and_clears_partial_signatures() {
        let (kps, pskt) = multisig_signer_context_ecdsa();
        let signed_0 = sign_with_ecdsa(pskt.clone(), &kps[0]);
        let signed_1 = sign_with_ecdsa(pskt.clone(), &kps[1]);
        let combined = pskt.combiner().combine(signed_1).unwrap().combine(signed_0).unwrap();

        let finalized = MultiSig::finalize_pskt(combined.finalizer()).unwrap();

        let input = &finalized.inputs[0];
        assert!(input.final_script_sig.as_ref().is_some_and(|script| !script.is_empty()));
        assert!(input.partial_sigs.is_empty());
        assert!(input.redeem_script.is_none());
        assert!(input.bip32_derivations.is_empty());
    }

    #[test]
    fn finalize_pskt_ecdsa_rejects_insufficient_signatures() {
        let (kps, pskt) = multisig_signer_context_ecdsa();
        let signed = sign_with_ecdsa(pskt, &kps[0]);

        let result = MultiSig::finalize_pskt(signed.finalizer());

        assert!(matches!(result, Err(Error::InsufficientSignatures { required: 2, present: 1 })));
    }

    #[test]
    fn finalize_pskt_ecdsa_rejects_schnorr_partial_signature() {
        // Cross-scheme rejection: an ECDSA redeem context must reject a Schnorr partial sig.
        // Both signers are redeem-script members so membership passes, isolating the scheme
        // check - the ECDSA finalize arm rejects the Schnorr signature.
        let (kps, pskt) = multisig_signer_context_ecdsa();
        let ecdsa_signed = sign_with_ecdsa(pskt.clone(), &kps[0]);
        let schnorr_signed = sign_with(pskt.clone(), &kps[1]);
        let combined = pskt.combiner().combine(ecdsa_signed).unwrap().combine(schnorr_signed).unwrap();

        let result = MultiSig::finalize_pskt(combined.finalizer());

        assert!(matches!(result, Err(Error::UnsupportedSignatureType)));
    }

    #[test]
    fn finalize_pskt_schnorr_rejects_ecdsa_partial_signature() {
        // Cross-scheme rejection in the other direction: a Schnorr redeem context rejects an
        // ECDSA partial sig at the Schnorr finalize arm.
        let (kps, pskt) = multisig_signer_context();
        let schnorr_signed = sign_with(pskt.clone(), &kps[0]);
        let ecdsa_signed = sign_with_ecdsa(pskt.clone(), &kps[1]);
        let combined = pskt.combiner().combine(schnorr_signed).unwrap().combine(ecdsa_signed).unwrap();

        let result = MultiSig::finalize_pskt(combined.finalizer());

        assert!(matches!(result, Err(Error::UnsupportedSignatureType)));
    }

    #[test]
    fn signature_matches_scheme_enforces_account_curve() {
        let kp = Keypair::new(secp256k1::SECP256K1, &mut thread_rng());
        let message = Message::from_digest_slice(&[7u8; 32]).unwrap();
        let schnorr = Signature::Schnorr(kp.sign_schnorr(message));
        let ecdsa = Signature::ECDSA(kp.secret_key().sign_ecdsa(message));

        assert!(signature_matches_scheme(&ecdsa, true));
        assert!(!signature_matches_scheme(&schnorr, true));
        assert!(signature_matches_scheme(&schnorr, false));
        assert!(!signature_matches_scheme(&ecdsa, false));
    }

    #[test]
    fn finalize_pskt_rejects_non_member_partial_signature() {
        // Membership gate (Schnorr arm): a partial sig from a key absent from the redeem script
        // is rejected before the scheme and sufficiency checks. The signer's scheme matches the
        // context, so the only failing gate is membership -> InvalidPartialSignature.
        let (_members, pskt) = multisig_signer_context();
        let non_member = Keypair::new(secp256k1::SECP256K1, &mut thread_rng());
        let signed = sign_with(pskt, &non_member);

        let result = MultiSig::finalize_pskt(signed.finalizer());

        assert!(matches!(result, Err(Error::InvalidPartialSignature)));
    }

    #[test]
    fn finalize_pskt_ecdsa_rejects_non_member_partial_signature() {
        // Membership gate (ECDSA arm): same isolation as the Schnorr case with full-key compare.
        let (_members, pskt) = multisig_signer_context_ecdsa();
        let non_member = Keypair::new(secp256k1::SECP256K1, &mut thread_rng());
        let signed = sign_with_ecdsa(pskt, &non_member);

        let result = MultiSig::finalize_pskt(signed.finalizer());

        assert!(matches!(result, Err(Error::InvalidPartialSignature)));
    }

    fn multisig_signer_context_mixed() -> ([Keypair; 2], PSKT<Signer>) {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let schnorr_redeem = multisig_redeem_script(kps.iter().map(|kp| kp.x_only_public_key().0.serialize()), 2).unwrap();
        let ecdsa_redeem = multisig_redeem_script_ecdsa(kps.iter().map(|kp| kp.public_key().serialize()), 2).unwrap();
        let schnorr_input = InputBuilder::default()
            .utxo_entry(UtxoEntry {
                amount: 12_793_000_000_000,
                script_public_key: pay_to_script_hash_script(&schnorr_redeem),
                block_daa_score: 36_151_168,
                is_coinbase: false,
                covenant_id: None,
            })
            .previous_outpoint(TransactionOutpoint {
                transaction_id: TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap(),
                index: 0,
            })
            .sig_op_count(2)
            .redeem_script(schnorr_redeem)
            .build()
            .unwrap();
        let ecdsa_input = InputBuilder::default()
            .utxo_entry(UtxoEntry {
                amount: 12_793_000_000_000,
                script_public_key: pay_to_script_hash_script(&ecdsa_redeem),
                block_daa_score: 36_151_168,
                is_coinbase: false,
                covenant_id: None,
            })
            .previous_outpoint(TransactionOutpoint {
                transaction_id: TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap(),
                index: 1,
            })
            .sig_op_count(2)
            .redeem_script(ecdsa_redeem)
            .build()
            .unwrap();

        (kps, PSKT::<Signer>::from(Inner { inputs: vec![schnorr_input, ecdsa_input], outputs: vec![], ..Default::default() }))
    }

    fn sign_mixed_with(pskt: PSKT<Signer>, kp: &Keypair) -> PSKT<Signer> {
        let reused_values = SigHashReusedValuesUnsync::new();
        pskt.pass_signature_sync(|tx, sighash| -> std::result::Result<Vec<SignInputOk>, String> {
            tx.tx
                .inputs
                .iter()
                .enumerate()
                .map(|(input_index, _)| {
                    // Input 0 is the Schnorr redeem, input 1 the ECDSA redeem: select the digest
                    // and signing curve by input index so one PSKT carries both schemes.
                    let signature = if input_index == 0 {
                        let hash = calc_schnorr_signature_hash(&tx.as_verifiable(), input_index, sighash[input_index], &reused_values);
                        let message =
                            secp256k1::Message::from_digest_slice(hash.as_bytes().as_slice()).map_err(|error| error.to_string())?;
                        Signature::Schnorr(kp.sign_schnorr(message))
                    } else {
                        let hash = calc_ecdsa_signature_hash(&tx.as_verifiable(), input_index, sighash[input_index], &reused_values);
                        let message =
                            secp256k1::Message::from_digest_slice(hash.as_bytes().as_slice()).map_err(|error| error.to_string())?;
                        Signature::ECDSA(kp.secret_key().sign_ecdsa(message))
                    };
                    Ok(SignInputOk { signature, pub_key: kp.public_key(), key_source: None })
                })
                .collect()
        })
        .unwrap()
    }

    #[test]
    fn finalize_pskt_mixed_schemes_orders_and_clears_partial_signatures() {
        // One PSKT with a Schnorr input and an ECDSA input proves per-input scheme detection and
        // per-input digest selection in finalize: both inputs finalize and clear their state.
        let (kps, pskt) = multisig_signer_context_mixed();
        let signed_0 = sign_mixed_with(pskt.clone(), &kps[0]);
        let signed_1 = sign_mixed_with(pskt.clone(), &kps[1]);
        let combined = pskt.combiner().combine(signed_1).unwrap().combine(signed_0).unwrap();

        let finalized = MultiSig::finalize_pskt(combined.finalizer()).unwrap();

        for input in finalized.inputs.iter() {
            assert!(input.final_script_sig.as_ref().is_some_and(|script| !script.is_empty()));
            assert!(input.partial_sigs.is_empty());
            assert!(input.redeem_script.is_none());
            assert!(input.bip32_derivations.is_empty());
        }
    }
}
