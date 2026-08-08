//!
//! Partially Signed Kaspa Transaction (PSKT)
//!

use kaspa_bip32::{DerivationPath, KeyFingerprint, secp256k1};
use kaspa_consensus_core::{
    Hash,
    hashing::sighash::{SigHashReusedValuesUnsync, calc_ecdsa_signature_hash, calc_schnorr_signature_hash},
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::{
    collections::BTreeMap,
    fmt::Display,
    fmt::Formatter,
    future::Future,
    marker::PhantomData,
    ops::{Add, Deref},
};

pub use crate::error::Error;
pub use crate::global::{Global, GlobalBuilder};
pub use crate::input::{Input, InputBuilder};
pub use crate::output::{Output, OutputBuilder};
pub use crate::role::{Combiner, Constructor, Creator, Extractor, Finalizer, Signer, Updater};
use kaspa_consensus_core::config::params::Params;
use kaspa_consensus_core::constants::TX_VERSION_TOCCATA;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::{
    hashing::sighash_type::SigHashType,
    subnets::SUBNETWORK_ID_NATIVE,
    tx::{
        ComputeCommit, MutableTransaction, SignableTransaction, Transaction, TransactionId, TransactionInput, TransactionOutput,
        VerifiableTransaction,
    },
};
use kaspa_txscript::{EngineCtx, TxScriptEngine, caches::Cache};
pub use kaspa_wallet_keys::signature::SignatureScheme;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Inner {
    /// The global map.
    pub global: Global,
    /// The corresponding key-value map for each input in the unsigned transaction.
    pub inputs: Vec<Input>,
    /// The corresponding key-value map for each output in the unsigned transaction.
    pub outputs: Vec<Output>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Version {
    #[default]
    Zero = 0,
    One = 1,
    Two = 2,
}

impl Display for Version {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Version::Zero => write!(f, "{}", Version::Zero as u8),
            Version::One => write!(f, "{}", Version::One as u8),
            Version::Two => write!(f, "{}", Version::Two as u8),
        }
    }
}

/// Full information on the used extended public key: fingerprint of the
/// master extended public key and a derivation path from it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeySource {
    #[serde(with = "kaspa_utils::serde_bytes_fixed")]
    pub key_fingerprint: KeyFingerprint,
    pub derivation_path: DerivationPath,
}

impl KeySource {
    pub fn new(key_fingerprint: KeyFingerprint, derivation_path: DerivationPath) -> Self {
        Self { key_fingerprint, derivation_path }
    }
}

pub type PartialSigs = BTreeMap<secp256k1::PublicKey, Signature>;

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Copy, Clone)]
#[serde(rename_all = "camelCase")]
pub enum Signature {
    ECDSA(secp256k1::ecdsa::Signature),
    Schnorr(secp256k1::schnorr::Signature),
}

impl Signature {
    pub fn into_bytes(self) -> [u8; 64] {
        match self {
            Signature::ECDSA(s) => s.serialize_compact(),
            Signature::Schnorr(s) => s.serialize(),
        }
    }

    pub fn verify(&self, message: &secp256k1::Message, public_key: &secp256k1::PublicKey) -> Result<(), secp256k1::Error> {
        match self {
            Signature::ECDSA(signature) => secp256k1::SECP256K1.verify_ecdsa(message, signature, public_key),
            Signature::Schnorr(signature) => signature.verify(message, &public_key.x_only_public_key().0),
        }
    }
}

///
/// A Partially Signed Kaspa Transaction (PSKT) is a standardized format
/// that allows multiple participants to collaborate in creating and signing
/// a Kaspa transaction. PSKT enables the exchange of incomplete transaction
/// data between different wallets or entities, allowing each participant
/// to add their signature or inputs in stages. This facilitates more complex
/// transaction workflows, such as multi-signature setups or hardware wallet
/// interactions, by ensuring that sensitive data remains secure while
/// enabling cooperation across different devices or platforms without
/// exposing private keys.
///
/// Please note that due to transaction mass limits and potential of
/// a wallet aggregating large UTXO sets, the PSKT [`Bundle`](crate::bundle::Bundle) primitive
/// is used to represent a collection of PSKTs and should be used for
/// PSKT serialization and transport. PSKT is an internal implementation
/// primitive that represents each transaction in the bundle.
///
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PSKT<ROLE> {
    #[serde(flatten)]
    inner_pskt: Inner,
    #[serde(skip_serializing, default)]
    role: PhantomData<ROLE>,
}

impl<ROLE> From<Inner> for PSKT<ROLE> {
    fn from(inner_pskt: Inner) -> Self {
        PSKT { inner_pskt, role: Default::default() }
    }
}

impl<ROLE> Clone for PSKT<ROLE> {
    fn clone(&self) -> Self {
        PSKT { inner_pskt: self.inner_pskt.clone(), role: Default::default() }
    }
}

impl<ROLE> Deref for PSKT<ROLE> {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner_pskt
    }
}

impl<R> PSKT<R> {
    fn unsigned_tx(&self) -> SignableTransaction {
        let tx = Transaction::new(
            self.global.tx_version,
            self.inputs
                .iter()
                .map(|Input { previous_outpoint, sequence, sig_op_count, .. }| TransactionInput {
                    previous_outpoint: *previous_outpoint,
                    signature_script: vec![],
                    sequence: sequence.unwrap_or(u64::MAX),
                    compute_commit: ComputeCommit::SigopCount(sig_op_count.unwrap_or(0).into()), // TODO: Add support for v1 transactions with ComputeCommit::ComputeBudget
                })
                .collect(),
            self.outputs
                .iter()
                .map(|Output { amount, script_public_key, covenant, .. }: &Output| TransactionOutput {
                    value: *amount,
                    script_public_key: script_public_key.clone(),
                    covenant: *covenant,
                })
                .collect(),
            self.determine_lock_time(),
            SUBNETWORK_ID_NATIVE,
            0,
            // Only include payload if version supports it (Version::One or higher)
            if self.global.version >= Version::One { self.global.payload.clone().unwrap_or_default() } else { vec![] },
        );

        let mut tx = SignableTransaction::new(tx);
        // it is allowed to have missing utxo entries at that stage, this function can be executed by any roles
        tx.entries.iter_mut().zip(self.inputs.iter()).for_each(|(entry, input)| {
            *entry = input.utxo_entry.clone();
        });
        tx
    }

    fn calculate_id_internal(&self) -> TransactionId {
        self.unsigned_tx().tx.id()
    }

    fn determine_lock_time(&self) -> u64 {
        self.inputs.iter().map(|input: &Input| input.min_time).max().unwrap_or(self.global.fallback_lock_time).unwrap_or(0)
    }

    pub fn to_hex(&self) -> Result<String, Error> {
        Ok(format!("PSKT{}", hex::encode(serde_json::to_string(self)?)))
    }

    pub fn from_hex(hex_data: &str) -> Result<Self, Error> {
        if let Some(hex_data) = hex_data.strip_prefix("PSKT") {
            Ok(serde_json::from_slice(hex::decode(hex_data)?.as_slice())?)
        } else {
            Err(Error::PsktPrefixError)
        }
    }
}

impl Default for PSKT<Creator> {
    fn default() -> Self {
        PSKT { inner_pskt: Default::default(), role: Default::default() }
    }
}

impl PSKT<Creator> {
    /// Sets the fallback lock time.
    pub fn fallback_lock_time(mut self, fallback: u64) -> Self {
        self.inner_pskt.global.fallback_lock_time = Some(fallback);
        self
    }

    /// Sets the PSKT version.
    pub fn set_version(mut self, version: Version) -> Self {
        self.inner_pskt.global.version = version;
        self
    }

    // todo generic const
    /// Sets the inputs modifiable bit in the transaction modifiable flags.
    pub fn inputs_modifiable(mut self) -> Self {
        self.inner_pskt.global.inputs_modifiable = true;
        self
    }
    // todo generic const
    /// Sets the outputs modifiable bit in the transaction modifiable flags.
    pub fn outputs_modifiable(mut self) -> Self {
        self.inner_pskt.global.outputs_modifiable = true;
        self
    }

    pub fn constructor(self) -> PSKT<Constructor> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }
}

impl PSKT<Constructor> {
    // todo generic const
    /// Marks that the `PSKT` can not have any more inputs added to it.
    pub fn no_more_inputs(mut self) -> Self {
        self.inner_pskt.global.inputs_modifiable = false;
        self
    }
    // todo generic const
    /// Marks that the `PSKT` can not have any more outputs added to it.
    pub fn no_more_outputs(mut self) -> Self {
        self.inner_pskt.global.outputs_modifiable = false;
        self
    }

    /// Adds an input to the PSKT.
    pub fn input(mut self, input: Input) -> Self {
        self.inner_pskt.inputs.push(input);
        self.inner_pskt.global.input_count += 1;
        self
    }

    /// Adds an output to the PSKT.
    pub fn output(mut self, output: Output) -> Result<Self, Error> {
        if output.covenant.is_some()
            && (self.inner_pskt.global.version < Version::Two || self.inner_pskt.global.tx_version < TX_VERSION_TOCCATA)
        {
            return Err(Error::Covenant);
        }
        self.inner_pskt.outputs.push(output);
        self.inner_pskt.global.output_count += 1;
        Ok(self)
    }

    pub fn payload(mut self, payload: Option<Vec<u8>>) -> Result<Self, Error> {
        // Only allow setting payload if version is One or greater
        if payload.is_some() && self.inner_pskt.global.version < Version::One {
            return Err(Error::PayloadRequiresVersion1(self.inner_pskt.global.version));
        }
        self.inner_pskt.global.payload = payload;
        Ok(self)
    }

    /// Returns a PSKT [`Updater`] once construction is completed.
    pub fn updater(self) -> PSKT<Updater> {
        let pskt = self.no_more_inputs().no_more_outputs();
        PSKT { inner_pskt: pskt.inner_pskt, role: Default::default() }
    }

    pub fn signer(self) -> PSKT<Signer> {
        self.updater().signer()
    }

    pub fn combiner(self) -> PSKT<Combiner> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }

    pub fn set_tx_version(mut self, tx_version: u16) -> Self {
        self.inner_pskt.global.tx_version = tx_version;
        self
    }
}

impl PSKT<Updater> {
    pub fn set_sequence(mut self, n: u64, input_index: usize) -> Result<Self, Error> {
        self.inner_pskt.inputs.get_mut(input_index).ok_or(Error::OutOfBounds)?.sequence = Some(n);
        Ok(self)
    }

    pub fn set_input_redeem_script(mut self, input_index: usize, redeem_script: Vec<u8>) -> Result<Self, Error> {
        let input = self.inner_pskt.inputs.get_mut(input_index).ok_or(Error::OutOfBounds)?;
        match input.redeem_script.as_ref() {
            Some(existing) if existing != &redeem_script => Err(Error::Custom("Conflicting redeem script for PSKT input".to_string())),
            Some(_) => Ok(self),
            None => {
                input.redeem_script = Some(redeem_script);
                Ok(self)
            }
        }
    }

    pub fn set_input_sig_op_count(mut self, input_index: usize, sig_op_count: u8) -> Result<Self, Error> {
        let input = self.inner_pskt.inputs.get_mut(input_index).ok_or(Error::OutOfBounds)?;
        match input.sig_op_count {
            Some(existing) if existing != sig_op_count => Err(Error::Custom("Conflicting sig op count for PSKT input".to_string())),
            Some(_) => Ok(self),
            None => {
                input.sig_op_count = Some(sig_op_count);
                Ok(self)
            }
        }
    }

    pub fn add_input_bip32_derivation(
        mut self,
        input_index: usize,
        public_key: secp256k1::PublicKey,
        key_source: Option<KeySource>,
    ) -> Result<Self, Error> {
        let input = self.inner_pskt.inputs.get_mut(input_index).ok_or(Error::OutOfBounds)?;
        insert_bip32_derivation(input, public_key, key_source)?;
        Ok(self)
    }

    pub fn signer(self) -> PSKT<Signer> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }

    pub fn combiner(self) -> PSKT<Combiner> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }
}

impl PSKT<Signer> {
    // todo use iterator instead of vector
    pub fn pass_signature_sync<SignFn, E>(mut self, sign_fn: SignFn) -> Result<Self, Error>
    where
        E: Display,
        SignFn: FnOnce(SignableTransaction, Vec<SigHashType>) -> Result<Vec<SignInputOk>, E>,
    {
        let unsigned_tx = self.unsigned_tx();
        let sighashes = self.inputs.iter().map(|input| input.sighash_type).collect();
        let signatures = sign_fn(unsigned_tx, sighashes).map_err(|error| Error::Custom(error.to_string()))?;
        if signatures.len() != self.inputs.len() {
            return Err(Error::SignatureCountMismatch { expected: self.inputs.len(), actual: signatures.len() });
        }

        for (input_index, signature) in signatures.into_iter().enumerate() {
            self.add_partial_signature(input_index, signature)?;
        }

        Ok(self)
    }
    // todo use iterator instead of vector
    pub async fn pass_signature<SignFn, Fut, E>(mut self, sign_fn: SignFn) -> Result<Self, Error>
    where
        E: Display,
        Fut: Future<Output = Result<Vec<SignInputOk>, E>>,
        SignFn: FnOnce(SignableTransaction, Vec<SigHashType>) -> Fut,
    {
        let unsigned_tx = self.unsigned_tx();
        let sighashes = self.inputs.iter().map(|input| input.sighash_type).collect();
        let signatures = sign_fn(unsigned_tx, sighashes).await.map_err(|error| Error::Custom(error.to_string()))?;
        if signatures.len() != self.inputs.len() {
            return Err(Error::SignatureCountMismatch { expected: self.inputs.len(), actual: signatures.len() });
        }

        for (input_index, signature) in signatures.into_iter().enumerate() {
            self.add_partial_signature(input_index, signature)?;
        }
        Ok(self)
    }

    pub fn pass_partial_signatures(mut self, signatures: impl IntoIterator<Item = (usize, SignInputOk)>) -> Result<Self, Error> {
        for (input_index, signature) in signatures {
            self.add_partial_signature(input_index, signature)?;
        }
        Ok(self)
    }

    fn add_partial_signature(&mut self, input_index: usize, sign_input: SignInputOk) -> Result<(), Error> {
        let input = self.inner_pskt.inputs.get_mut(input_index).ok_or(Error::OutOfBounds)?;
        let SignInputOk { signature, pub_key, key_source } = sign_input;

        insert_bip32_derivation(input, pub_key, key_source)?;

        match input.partial_sigs.entry(pub_key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(signature);
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(entry) if *entry.get() == signature => Ok(()),
            std::collections::btree_map::Entry::Occupied(entry) => Err(Error::ConflictingPartialSignature(*entry.key())),
        }
    }

    pub fn calculate_id(&self) -> TransactionId {
        self.calculate_id_internal()
    }

    pub fn signature_hashes(&self, scheme: SignatureScheme) -> Result<Vec<Hash>, Error> {
        if self.inputs.iter().any(|input| input.utxo_entry.is_none()) {
            return Err(Error::MissingUtxoEntry);
        }

        let tx = self.unsigned_tx();
        let verifiable_tx = tx.as_verifiable();
        let reused_values = SigHashReusedValuesUnsync::new();
        Ok(self
            .inputs
            .iter()
            .enumerate()
            .map(|(input_index, input)| match scheme {
                SignatureScheme::ECDSA => calc_ecdsa_signature_hash(&verifiable_tx, input_index, input.sighash_type, &reused_values),
                SignatureScheme::Schnorr => {
                    calc_schnorr_signature_hash(&verifiable_tx, input_index, input.sighash_type, &reused_values)
                }
            })
            .collect())
    }

    pub fn finalizer(self) -> PSKT<Finalizer> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }

    pub fn combiner(self) -> PSKT<Combiner> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }

    // Unorphan batch transaction UTXO.
    pub fn set_input_prev_transaction_id(self, transaction_id: Hash) -> PSKT<Signer> {
        let mut new_inputs = self.inner_pskt.inputs.clone();

        new_inputs.iter_mut().for_each(|input| {
            input.previous_outpoint.transaction_id = transaction_id;
        });

        let mut updated_inner = self.inner_pskt.clone();
        updated_inner.inputs = new_inputs;

        PSKT { inner_pskt: updated_inner, role: Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInputOk {
    pub signature: Signature,
    pub pub_key: secp256k1::PublicKey,
    pub key_source: Option<KeySource>,
}

impl PSKT<Combiner> {
    pub fn combine<R>(mut self, mut rhs: PSKT<R>) -> Result<Self, CombineError> {
        // If both PSKTs describe the same number of inputs and outputs,
        // they must represent the same unsigned transaction.
        // Differing shapes are allowed here to support construction-stage extension.
        if self.has_same_transaction_shape(&rhs) && self.calculate_id_internal() != rhs.calculate_id_internal() {
            return Err(CombineError::TransactionMismatch);
        }

        self.inner_pskt.global = (self.inner_pskt.global + rhs.inner_pskt.global)?;
        macro_rules! combine {
            ($left:expr, $right:expr, $err: ty) => {
                if $left.len() > $right.len() {
                    $left.iter_mut().zip($right.iter_mut()).try_for_each(|(left, right)| -> Result<(), $err> {
                        *left = (std::mem::take(left) + std::mem::take(right))?;
                        Ok(())
                    })?;
                    $left
                } else {
                    $right.iter_mut().zip($left.iter_mut()).try_for_each(|(left, right)| -> Result<(), $err> {
                        *left = (std::mem::take(left) + std::mem::take(right))?;
                        Ok(())
                    })?;
                    $right
                }
            };
        }
        // todo add sort to build deterministic combination
        // comment: depends on sighash type, otherwise it can break previous signature (outside of construct)
        self.inner_pskt.inputs = combine!(self.inner_pskt.inputs, rhs.inner_pskt.inputs, crate::input::CombineError);
        self.inner_pskt.outputs = combine!(self.inner_pskt.outputs, rhs.inner_pskt.outputs, crate::output::CombineError);
        if self.outputs.iter().any(|output| output.covenant.is_some())
            && (self.global.version < Version::Two || self.global.tx_version < TX_VERSION_TOCCATA)
        {
            return Err(CombineError::Covenant);
        }
        Ok(self)
    }

    fn has_same_transaction_shape<R>(&self, rhs: &PSKT<R>) -> bool {
        self.inputs.len() == rhs.inputs.len() && self.outputs.len() == rhs.outputs.len()
    }

    pub fn signer(self) -> PSKT<Signer> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }

    pub fn finalizer(self) -> PSKT<Finalizer> {
        PSKT { inner_pskt: self.inner_pskt, role: Default::default() }
    }
}

impl<R> Add<PSKT<R>> for PSKT<Combiner> {
    type Output = Result<Self, CombineError>;

    fn add(self, rhs: PSKT<R>) -> Self::Output {
        self.combine(rhs)
    }
}

impl PSKT<Finalizer> {
    pub fn finalize_sync<E: Display>(
        self,
        final_sig_fn: impl FnOnce(&Inner) -> Result<Vec<Vec<u8>>, E>,
    ) -> Result<Self, FinalizeError<E>> {
        let sigs = final_sig_fn(&self);
        self.finalize_internal(sigs)
    }

    pub async fn finalize<F, Fut, E>(self, final_sig_fn: F) -> Result<Self, FinalizeError<E>>
    where
        E: Display,
        F: FnOnce(&Inner) -> Fut,
        Fut: Future<Output = Result<Vec<Vec<u8>>, E>>,
    {
        let sigs = final_sig_fn(&self).await;
        self.finalize_internal(sigs)
    }

    pub fn id(&self) -> Option<TransactionId> {
        self.global.id
    }

    pub fn extractor(self) -> Result<PSKT<Extractor>, TxNotFinalized> {
        if self.global.id.is_none() {
            Err(TxNotFinalized {})
        } else {
            Ok(PSKT { inner_pskt: self.inner_pskt, role: Default::default() })
        }
    }

    fn finalize_internal<E: Display>(mut self, sigs: Result<Vec<Vec<u8>>, E>) -> Result<Self, FinalizeError<E>> {
        let sigs = sigs?;
        if sigs.len() != self.inputs.len() {
            return Err(FinalizeError::WrongFinalizedSigsCount { expected: self.inputs.len(), actual: sigs.len() });
        }
        self.inner_pskt.inputs.iter_mut().enumerate().zip(sigs).try_for_each(|((idx, input), sig)| {
            if sig.is_empty() {
                return Err(FinalizeError::EmptySignature(idx));
            }
            input.sequence = Some(input.sequence.unwrap_or(u64::MAX)); // todo discussable
            input.final_script_sig = Some(sig);
            input.partial_sigs.clear();
            input.redeem_script = None;
            input.bip32_derivations.clear();
            Ok(())
        })?;
        self.inner_pskt.global.id = Some(self.calculate_id_internal());
        Ok(self)
    }
}

impl PSKT<Extractor> {
    pub fn extract_tx_unchecked(self, params: &Params) -> Result<MutableTransaction<Transaction>, TxNotFinalized> {
        let tx = self.unsigned_tx();
        let entries = tx.entries;
        let mut tx = tx.tx;
        tx.inputs.iter_mut().zip(self.inner_pskt.inputs).try_for_each(|(dest, src)| {
            dest.signature_script = src.final_script_sig.ok_or(TxNotFinalized {})?;
            Ok(())
        })?;
        let tx = MutableTransaction { tx, entries, calculated_fee: None, calculated_non_contextual_masses: None };
        let calculator = MassCalculator::new_with_consensus_params(params);
        let storage_mass = calculator.calc_contextual_masses(&tx.as_verifiable()).map(|mass| mass.storage_mass).unwrap_or_default();
        let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(&tx.tx);
        let mass = storage_mass.max(compute_mass).max(transient_mass);
        tx.tx.set_storage_mass(mass);
        Ok(tx)
    }

    pub fn extract_tx(self, params: &Params) -> Result<MutableTransaction<Transaction>, ExtractError> {
        let tx = self.extract_tx_unchecked(params)?;
        {
            let tx = tx.as_verifiable();
            let cache = Cache::new(10_000);
            let reused_values = SigHashReusedValuesUnsync::new();
            let ctx = EngineCtx::new(&cache).with_reused(&reused_values);

            tx.populated_inputs().enumerate().try_for_each(|(idx, (input, entry))| {
                TxScriptEngine::from_transaction_input(&tx, input, idx, entry, ctx, Default::default()).execute()?;
                <Result<(), ExtractError>>::Ok(())
            })?;
        }
        Ok(tx)
    }
}

fn insert_bip32_derivation(input: &mut Input, public_key: secp256k1::PublicKey, key_source: Option<KeySource>) -> Result<(), Error> {
    match input.bip32_derivations.entry(public_key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(key_source);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry) if *entry.get() == key_source => Ok(()),
        std::collections::btree_map::Entry::Occupied(_) => {
            Err(Error::Custom("Conflicting bip32 derivation for PSKT input".to_string()))
        }
    }
}

/// Error combining pskt.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CombineError {
    #[error("PSKT unsigned transactions do not match")]
    TransactionMismatch,
    #[error(transparent)]
    Global(#[from] crate::global::CombineError),
    #[error(transparent)]
    Inputs(#[from] crate::input::CombineError),
    #[error(transparent)]
    Outputs(#[from] crate::output::CombineError),
    #[error("Outputs not allowed to contain covenant due to pskt or tx versions mismatch")]
    Covenant,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum FinalizeError<E> {
    #[error("Signatures count mismatch")]
    WrongFinalizedSigsCount { expected: usize, actual: usize },
    #[error("Signatures at index: {0} is empty")]
    EmptySignature(usize),
    #[error(transparent)]
    FinalizeCb(#[from] E),
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    #[error(transparent)]
    TxScriptError(#[from] kaspa_txscript_errors::TxScriptError),
    #[error(transparent)]
    TxNotFinalized(#[from] TxNotFinalized),
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("Transaction is not finalized")]
pub struct TxNotFinalized {}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::tx::{TransactionOutpoint, UtxoEntry};
    use kaspa_txscript::{multisig_redeem_script, pay_to_script_hash_script};
    use secp256k1::{Keypair, rand::thread_rng};
    use std::str::FromStr;

    #[test]
    fn signature_hashes_require_utxo_entries() {
        let inner = Inner { inputs: vec![Input::default()], outputs: vec![], ..Default::default() };
        let pskt = PSKT::<Signer>::from(inner);

        assert!(matches!(pskt.signature_hashes(SignatureScheme::Schnorr), Err(Error::MissingUtxoEntry)));
    }

    #[test]
    fn combine_allows_construction_extension() {
        let left = PSKT::<Combiner>::from(Inner::default());
        let right = PSKT::<Constructor>::from(Inner { inputs: vec![Input::default()], ..Default::default() });

        let combined = left.combine(right).unwrap();

        assert_eq!(combined.inputs.len(), 1);
    }

    #[test]
    fn combine_rejects_same_shape_transaction_mismatch() {
        let utxo_entry = UtxoEntry::default();
        let left = PSKT::<Combiner>::from(Inner {
            inputs: vec![Input { utxo_entry: Some(utxo_entry.clone()), ..Default::default() }],
            ..Default::default()
        });
        let right = PSKT::<Constructor>::from(Inner {
            inputs: vec![Input {
                utxo_entry: Some(utxo_entry),
                previous_outpoint: TransactionOutpoint { index: 1, ..Default::default() },
                ..Default::default()
            }],
            ..Default::default()
        });

        assert!(matches!(left.combine(right), Err(CombineError::TransactionMismatch)));
    }

    fn multisig_signer_context() -> ([Keypair; 2], PSKT<Signer>) {
        let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
        let redeem_script = multisig_redeem_script(kps.iter().map(|kp| kp.x_only_public_key().0.serialize()), 2).unwrap();
        let input = InputBuilder::default()
            .utxo_entry(UtxoEntry {
                amount: 10,
                script_public_key: pay_to_script_hash_script(&redeem_script),
                block_daa_score: 1,
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

    #[test]
    fn pass_signature_sync_rejects_wrong_signature_count() {
        let (_, pskt) = multisig_signer_context();

        let result = pskt.pass_signature_sync(|_, _| -> Result<Vec<SignInputOk>, String> { Ok(vec![]) });

        assert!(matches!(result, Err(Error::SignatureCountMismatch { expected: 1, actual: 0 })));
    }
}
