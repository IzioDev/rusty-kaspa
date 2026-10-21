//!
//! Transaction signing trait and generic signer implementations..
//!

use crate::imports::*;
use kaspa_bip32::PrivateKey;
use kaspa_consensus_core::{sign::sign_with_multiple_v2, tx::SignableTransaction};

pub trait SignerT: Send + Sync + 'static {
    fn try_sign(&self, transaction: SignableTransaction, addresses: &[Address]) -> Result<SignableTransaction>;
}

/// P2PK signer bound to the account's immutable network view.
pub struct Kcc3Signer {
    account: Arc<crate::account::kcc3::Kcc3>,
    keydata: PrvKeyData,
    payment_secret: Option<Secret>,
}

impl Kcc3Signer {
    pub fn new(account: Arc<crate::account::kcc3::Kcc3>, keydata: PrvKeyData, payment_secret: Option<Secret>) -> Self {
        Self { account, keydata, payment_secret }
    }
}

impl SignerT for Kcc3Signer {
    fn try_sign(&self, mut transaction: SignableTransaction, addresses: &[Address]) -> Result<SignableTransaction> {
        use kaspa_consensus_core::hashing::{
            sighash::{SigHashReusedValuesUnsync, calc_ecdsa_signature_hash, calc_schnorr_signature_hash},
            sighash_type::SIG_HASH_ALL,
        };
        use kaspa_wallet_keys::derivation::gen2::SignatureScheme;
        self.account.ensure_active_context()?;
        if addresses.len() != transaction.tx.inputs.len() || transaction.entries.len() != addresses.len() {
            return Err(Error::custom("KCC-3 input/address count mismatch"));
        }
        let refs = addresses.iter().collect::<Vec<_>>();
        let keys = self.account.clone().create_address_private_keys(&self.keydata, &self.payment_secret, &refs)?;
        let scheme = self.account.public_description()?.key.derivation().signature_scheme();
        let reused = SigHashReusedValuesUnsync::new();
        for (index, address) in addresses.iter().enumerate() {
            let key = &keys.iter().find(|(a, _)| *a == address).ok_or_else(|| Error::custom("Missing KCC-3 input key"))?.1;
            let public = key.public_key(secp256k1::SECP256K1);
            let expected = kaspa_wallet_keys::derivation::gen1::PubkeyDerivationManager::create_address(
                &public,
                self.account.wallet().address_prefix()?,
                scheme == SignatureScheme::Ecdsa,
            )?;
            let entry = transaction.entries[index].as_ref().ok_or_else(|| Error::custom("KCC-3 input has no UTXO"))?;
            if expected != *address || entry.script_public_key != kaspa_txscript::pay_to_address_script(&expected) {
                return Err(Error::custom("KCC-3 input script does not match derived key"));
            }
            let hash = match scheme {
                SignatureScheme::Schnorr => calc_schnorr_signature_hash(&transaction.as_verifiable(), index, SIG_HASH_ALL, &reused),
                SignatureScheme::Ecdsa => calc_ecdsa_signature_hash(&transaction.as_verifiable(), index, SIG_HASH_ALL, &reused),
            };
            let message = secp256k1::Message::from_digest_slice(&hash.as_bytes())?;
            let signature = match scheme {
                SignatureScheme::Schnorr => key.keypair(secp256k1::SECP256K1).sign_schnorr(message).serialize(),
                SignatureScheme::Ecdsa => secp256k1::SECP256K1.sign_ecdsa(&message, key).serialize_compact(),
            };
            transaction.tx.inputs[index].signature_script =
                std::iter::once(65).chain(signature).chain([SIG_HASH_ALL.to_u8()]).collect();
        }
        self.account.ensure_active_context()?;
        Ok(transaction)
    }
}

struct Inner {
    keydata: PrvKeyData,
    account: Arc<dyn Account>,
    payment_secret: Option<Secret>,
    keys: Mutex<AHashMap<Address, [u8; 32]>>,
}

pub struct Signer {
    inner: Arc<Inner>,
}

impl Signer {
    pub fn new(account: Arc<dyn Account>, keydata: PrvKeyData, payment_secret: Option<Secret>) -> Self {
        Self { inner: Arc::new(Inner { keydata, account, payment_secret, keys: Mutex::new(AHashMap::new()) }) }
    }

    fn ingest(&self, addresses: &[Address]) -> Result<()> {
        let mut keys = self.inner.keys.lock().unwrap();
        // skip address that are already present in the key map
        let addresses = addresses.iter().filter(|a| !keys.contains_key(a)).collect::<Vec<_>>();
        if !addresses.is_empty() {
            // let account = self.inner.account.clone().as_derivation_capable().expect("expecting derivation capable account");
            // let (receive, change) = account.derivation().addresses_indexes(&addresses)?;
            // let private_keys = account.create_private_keys(&self.inner.keydata, &self.inner.payment_secret, &receive, &change)?;
            let private_keys = self.inner.account.clone().create_address_private_keys(
                &self.inner.keydata,
                &self.inner.payment_secret,
                addresses.as_slice(),
            )?;
            for (address, private_key) in private_keys {
                keys.insert(address.clone(), private_key.to_bytes());
            }
        }

        Ok(())
    }
}

impl SignerT for Signer {
    fn try_sign(&self, mutable_tx: SignableTransaction, addresses: &[Address]) -> Result<SignableTransaction> {
        self.ingest(addresses)?;

        let keys = self.inner.keys.lock().unwrap();
        let mut keys_for_signing = addresses.iter().map(|address| *keys.get(address).unwrap()).collect::<Vec<_>>();
        // TODO - refactor for multisig
        let signable_tx = sign_with_multiple_v2(mutable_tx, &keys_for_signing).fully_signed()?;
        keys_for_signing.zeroize();
        Ok(signable_tx)
    }
}

// ---

struct KeydataSignerInner {
    keys: HashMap<Address, [u8; 32]>,
}

pub struct KeydataSigner {
    inner: Arc<KeydataSignerInner>,
}

impl KeydataSigner {
    pub fn new(keydata: Vec<(Address, secp256k1::SecretKey)>) -> Self {
        let keys = keydata.into_iter().map(|(address, key)| (address, key.to_bytes())).collect();
        Self { inner: Arc::new(KeydataSignerInner { keys }) }
    }
}

impl SignerT for KeydataSigner {
    fn try_sign(&self, mutable_tx: SignableTransaction, addresses: &[Address]) -> Result<SignableTransaction> {
        let mut keys_for_signing = addresses.iter().map(|address| *self.inner.keys.get(address).unwrap()).collect::<Vec<_>>();
        // TODO - refactor for multisig
        let signable_tx = sign_with_multiple_v2(mutable_tx, &keys_for_signing).fully_signed()?;
        keys_for_signing.zeroize();
        Ok(signable_tx)
    }
}
