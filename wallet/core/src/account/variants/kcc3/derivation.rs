//! KCC-3 derivation and the lifetime of its network-bound address chains.

use crate::derivation::{AddressDerivationManagerTrait, AddressDerivationMeta, AddressManagerTrait, Inner};
use crate::imports::*;
use kaspa_bip32::ExtendedPrivateKey;
use kaspa_wallet_keys::derivation::gen2::{AccountPublicKey, MAX_INDEX, SignatureScheme};
use kaspa_wallet_keys::derivation::traits::PubkeyDerivationManagerTrait;

pub(super) struct Kcc3DerivationManager {
    key: AccountPublicKey,
    receive: Arc<Kcc3AddressManager>,
    change: Arc<Kcc3AddressManager>,
}

impl Kcc3DerivationManager {
    pub(super) fn new(wallet: &Arc<Wallet>, key: AccountPublicKey, indexes: AddressDerivationMeta) -> Result<Arc<Self>> {
        let network = wallet.network_id()?;
        if key.derivation().network() != network.network_type {
            return Err(Error::custom("KCC-3 origin network mismatch"));
        }
        let generation = wallet.context_generation();
        let ecdsa = key.derivation().signature_scheme() == SignatureScheme::Ecdsa;
        let address_manager = |change, index| -> Result<Arc<Kcc3AddressManager>> {
            key.derive_public_key(change, index)?;
            Ok(Arc::new(Kcc3AddressManager {
                wallet: wallet.clone(),
                network,
                generation,
                ecdsa,
                pubkey_manager: Arc::new(Kcc3PubkeyManager { key: key.clone(), change, index: Mutex::new(index) }),
                inner: Mutex::new(Inner { index, address_to_index_map: HashMap::new() }),
            }))
        };
        let receive = address_manager(0, indexes.receive())?;
        let change = address_manager(1, indexes.change())?;
        Ok(Arc::new(Self { key, receive, change }))
    }
}

#[async_trait]
impl AddressDerivationManagerTrait for Kcc3DerivationManager {
    fn receive_address_manager(&self) -> Arc<dyn AddressManagerTrait> {
        self.receive.clone()
    }

    fn change_address_manager(&self) -> Arc<dyn AddressManagerTrait> {
        self.change.clone()
    }

    async fn get_range_with_keys(
        &self,
        change_address: bool,
        indexes: std::ops::Range<u32>,
        update_indexes: bool,
        xkey: &ExtendedPrivateKey<secp256k1::SecretKey>,
    ) -> Result<Vec<(Address, secp256k1::SecretKey)>> {
        let manager = if change_address { &self.change } else { &self.receive };
        manager.ensure_active_context()?;
        if indexes.start > indexes.end || indexes.end > MAX_INDEX + 1 {
            return Err(Error::custom("Invalid KCC-3 address range"));
        }
        if self.key.derivation().derive_account(xkey)?.public_key() != *self.key.xpub() {
            return Err(Error::custom("KCC-3 private key does not match account xpub"));
        }
        let mut result = Vec::new();
        for index in indexes.clone() {
            let public = self.key.derive_public_key(change_address as u32, index)?;
            let address = manager.create_address(public.public_key())?;
            let private = self.key.derivation().derive_private_key(xkey, change_address as u32, index)?;
            result.push((address, private));
        }
        if update_indexes {
            let mut inner = manager.inner();
            for (index, (address, _)) in indexes.zip(&result) {
                inner.address_to_index_map.insert(address.clone(), index);
            }
        }
        Ok(result)
    }
}

struct Kcc3AddressManager {
    wallet: Arc<Wallet>,
    network: NetworkId,
    generation: u64,
    ecdsa: bool,
    pubkey_manager: Arc<dyn PubkeyDerivationManagerTrait>,
    inner: Mutex<Inner>,
}

impl Kcc3AddressManager {
    fn create_address(&self, key: &secp256k1::PublicKey) -> Result<Address> {
        self.ensure_active_context()?;
        Ok(kaspa_wallet_keys::derivation::gen1::PubkeyDerivationManager::create_address(key, self.network.into(), self.ecdsa)?)
    }
}

impl AddressManagerTrait for Kcc3AddressManager {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn ensure_active_context(&self) -> Result<()> {
        if self.wallet.network_id()? != self.network || self.wallet.context_generation() != self.generation {
            return Err(Error::custom("Stale KCC-3 network context"));
        }
        Ok(())
    }

    fn new_address(&self) -> Result<Address> {
        self.ensure_active_context()?;
        let mut inner = self.inner();
        let index = inner.index.checked_add(1).ok_or_else(|| Error::custom("Address index exhausted"))?;
        if index > MAX_INDEX {
            return Err(Error::custom("KCC-3 address index exhausted"));
        }
        let key = self
            .pubkey_manager
            .get_range(index..index + 1)?
            .first()
            .copied()
            .ok_or_else(|| kaspa_wallet_keys::error::Error::custom("Missing derived key"))?;
        let address = self.create_address(&key)?;
        self.pubkey_manager.set_index(index)?;
        inner.index = index;
        inner.address_to_index_map.insert(address.clone(), index);
        Ok(address)
    }

    fn current_address(&self) -> Result<Address> {
        self.ensure_active_context()?;
        let address = self.create_address(&self.pubkey_manager.current_pubkey()?)?;
        let mut inner = self.inner();
        let index = inner.index;
        inner.address_to_index_map.insert(address.clone(), index);
        Ok(address)
    }

    fn set_index(&self, index: u32) -> Result<()> {
        self.ensure_active_context()?;
        if index > MAX_INDEX {
            return Err(Error::custom("Invalid KCC-3 address index"));
        }
        self.pubkey_manager.set_index(index)?;
        self.inner().index = index;
        Ok(())
    }

    fn get_range_with_args(&self, indexes: std::ops::Range<u32>, update_indexes: bool) -> Result<Vec<Address>> {
        self.ensure_active_context()?;
        if indexes.start > indexes.end || indexes.end > MAX_INDEX + 1 {
            return Err(Error::custom("Invalid KCC-3 address range"));
        }
        let addresses =
            self.pubkey_manager.get_range(indexes.clone())?.iter().map(|key| self.create_address(key)).collect::<Result<Vec<_>>>()?;
        if update_indexes {
            let mut inner = self.inner();
            for (index, address) in indexes.zip(&addresses) {
                inner.address_to_index_map.insert(address.clone(), index);
            }
        }
        Ok(addresses)
    }
}

struct Kcc3PubkeyManager {
    key: AccountPublicKey,
    change: u32,
    index: Mutex<u32>,
}

#[async_trait]
impl PubkeyDerivationManagerTrait for Kcc3PubkeyManager {
    fn new_pubkey(&self) -> kaspa_wallet_keys::result::Result<secp256k1::PublicKey> {
        let mut index = self.index.lock()?;
        let next = index.checked_add(1).ok_or_else(|| kaspa_wallet_keys::error::Error::custom("KCC-3 address index exhausted"))?;
        let key = *self.key.derive_public_key(self.change, next)?.public_key();
        *index = next;
        Ok(key)
    }

    fn current_pubkey(&self) -> kaspa_wallet_keys::result::Result<secp256k1::PublicKey> {
        Ok(*self.key.derive_public_key(self.change, *self.index.lock()?)?.public_key())
    }

    fn index(&self) -> kaspa_wallet_keys::result::Result<u32> {
        Ok(*self.index.lock()?)
    }

    fn set_index(&self, index: u32) -> kaspa_wallet_keys::result::Result<()> {
        self.key.derive_public_key(self.change, index)?;
        *self.index.lock()? = index;
        Ok(())
    }

    fn get_range(&self, range: std::ops::Range<u32>) -> kaspa_wallet_keys::result::Result<Vec<secp256k1::PublicKey>> {
        if range.start > range.end || range.end > MAX_INDEX + 1 {
            return Err(kaspa_wallet_keys::error::Error::custom("Invalid KCC-3 address range"));
        }
        range.map(|index| Ok(*self.key.derive_public_key(self.change, index)?.public_key())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingPubkeyManager {
        public_key: secp256k1::PublicKey,
        index: Mutex<u32>,
        requested: Mutex<Vec<u32>>,
    }

    #[async_trait]
    impl PubkeyDerivationManagerTrait for FailingPubkeyManager {
        fn new_pubkey(&self) -> kaspa_wallet_keys::result::Result<secp256k1::PublicKey> {
            let index = self.index()? + 1;
            let key = self.get_range(index..index + 1)?[0];
            self.set_index(index)?;
            Ok(key)
        }

        fn current_pubkey(&self) -> kaspa_wallet_keys::result::Result<secp256k1::PublicKey> {
            let index = self.index()?;
            Ok(self.get_range(index..index + 1)?[0])
        }

        fn index(&self) -> kaspa_wallet_keys::result::Result<u32> {
            Ok(*self.index.lock()?)
        }

        fn set_index(&self, index: u32) -> kaspa_wallet_keys::result::Result<()> {
            if index == 1 {
                return Err(kaspa_wallet_keys::error::Error::InvalidChild { depth: 6, index });
            }
            *self.index.lock()? = index;
            Ok(())
        }

        fn get_range(&self, range: std::ops::Range<u32>) -> kaspa_wallet_keys::result::Result<Vec<secp256k1::PublicKey>> {
            range
                .map(|index| {
                    self.requested.lock()?.push(index);
                    if index == 1 {
                        Err(kaspa_wallet_keys::error::Error::InvalidChild { depth: 6, index })
                    } else {
                        Ok(self.public_key)
                    }
                })
                .collect()
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn kcc3_invalid_child_does_not_skip_or_advance() -> Result<()> {
        let wallet = Arc::new(Wallet::try_with_rpc(None, Wallet::resident_store()?, Some(NetworkId::new(NetworkType::Mainnet)))?);
        let key = secp256k1::SecretKey::from_slice(&[11; 32])?;
        let public = Arc::new(FailingPubkeyManager {
            public_key: key.public_key(secp256k1::SECP256K1),
            index: Mutex::new(0),
            requested: Mutex::new(Vec::new()),
        });
        let manager = Kcc3AddressManager {
            network: wallet.network_id()?,
            generation: wallet.context_generation(),
            wallet,
            ecdsa: false,
            pubkey_manager: public.clone(),
            inner: Mutex::new(Inner { index: 0, address_to_index_map: HashMap::new() }),
        };
        assert!(matches!(
            manager.new_address(),
            Err(Error::WalletKeys(kaspa_wallet_keys::error::Error::InvalidChild { depth: 6, index: 1 }))
        ));
        assert_eq!(manager.index(), 0);
        assert_eq!(public.index()?, 0);
        assert_eq!(*public.requested.lock()?, vec![1]);
        assert!(manager.inner().address_to_index_map.is_empty());
        assert!(manager.get_range(0..3).is_err());
        assert_eq!(*public.requested.lock()?, vec![1, 0, 1]);
        assert!(manager.inner().address_to_index_map.is_empty());
        assert!(manager.set_index(1).is_err());
        assert_eq!(manager.index(), 0);
        assert_eq!(public.index()?, 0);
        Ok(())
    }
}
