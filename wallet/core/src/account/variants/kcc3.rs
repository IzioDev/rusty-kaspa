//! Seed-backed and watch-only KCC-3 P2PK accounts.

use crate::account::Inner;
use crate::derivation::{AddressDerivationManagerTrait, AddressDerivationMeta};
use crate::imports::*;
use derivation::Kcc3DerivationManager;
use kaspa_bip32::{Language, Mnemonic};
use kaspa_wallet_keys::derivation::gen2::{AccountDerivation, AccountPublicKey, MAX_INDEX, SignatureScheme};
use std::collections::BTreeMap;

mod derivation;

pub const KCC3_ACCOUNT_KIND: &str = "kaspa-kcc3-standard";
pub const KCC3_WATCH_ACCOUNT_KIND: &str = "kaspa-kcc3-watch-standard";

pub struct Ctor;

#[async_trait]
impl Factory for Ctor {
    fn name(&self) -> String {
        "kcc3".into()
    }
    fn description(&self) -> String {
        "KCC-3 P2PK account".into()
    }
    async fn try_load(
        &self,
        wallet: &Arc<Wallet>,
        storage: &AccountStorage,
        meta: Option<Arc<AccountMetadata>>,
    ) -> Result<Arc<dyn Account>> {
        Ok(Arc::new(Kcc3::try_load(wallet, storage, meta)?))
    }
}

/// Public exchange format: a single network's account key and claimed origin.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicAccountDescription {
    pub version: u32,
    pub convention: String,
    pub kind: String,
    pub network: NetworkType,
    pub signature_scheme: SignatureScheme,
    pub account_index: u32,
    pub key: AccountPublicKey,
}

impl PublicAccountDescription {
    /// Import an account xpub on the selected network with a user-supplied scheme.
    /// The xpub encodes its account index, but cannot prove its hardened ancestry.
    pub fn try_from_xpub(network: NetworkType, signature_scheme: SignatureScheme, xpub: ExtendedPublicKeySecp256k1) -> Result<Self> {
        let account_index = xpub.attrs().child_number.index();
        let origin = AccountDerivation::try_new(network, signature_scheme, account_index)?;
        let key = AccountPublicKey::try_new(origin, None, xpub)?;
        Ok(Self { version: 1, convention: "kcc3".into(), kind: "kcc3-watch".into(), network, signature_scheme, account_index, key })
    }

    pub fn try_key(self) -> Result<AccountPublicKey> {
        if self.version != 1 || self.convention != "kcc3" || !matches!(self.kind.as_str(), "kcc3" | "kcc3-watch") {
            return Err(Error::custom("Unsupported KCC-3 public account description"));
        }
        if self.network != self.key.derivation().network()
            || self.signature_scheme != self.key.derivation().signature_scheme()
            || self.account_index != self.key.derivation().account_index()
        {
            return Err(Error::custom("KCC-3 public description does not match its origin"));
        }
        Ok(self.key)
    }
}

#[derive(Clone, Debug)]
pub struct Payload {
    pub anchor: AccountPublicKey,
    pub contexts: BTreeMap<NetworkType, AccountPublicKey>,
}

impl Payload {
    pub fn try_load(storage: &AccountStorage) -> Result<Self> {
        let payload = Self::try_from_slice(&storage.serialized)?;
        if payload.contexts.get(&payload.anchor.derivation().network()) != Some(&payload.anchor)
            || payload.contexts.iter().any(|(network, key)| {
                *network != key.derivation().network()
                    || key.derivation().signature_scheme() != payload.anchor.derivation().signature_scheme()
            })
        {
            return Err(Error::custom("Inconsistent KCC-3 network contexts"));
        }
        Ok(payload)
    }

    fn identity(&self, kind: AccountKind) -> Result<(AccountId, AccountStorageKey)> {
        // A separate versioned domain; existing deterministic account layouts are immutable.
        // Fingerprint annotations and additional contexts do not participate in identity.
        let anchor = &self.anchor;
        let bytes = borsh::to_vec(&("kaspa-kcc3-account", 1u32, kind, anchor.derivation(), anchor.xpub()))?;
        let storage_hash = crate::encryption::sha256_hash(&bytes);
        let id_hash = crate::encryption::sha256_hash(storage_hash.as_ref());
        Ok((
            AccountId(kaspa_hashes::Hash::from_slice(id_hash.as_ref())),
            AccountStorageKey(kaspa_hashes::Hash::from_slice(storage_hash.as_ref())),
        ))
    }
}

impl Storable for Payload {
    const STORAGE_MAGIC: u32 = 0x3343434b;
    const STORAGE_VERSION: u32 = 1;
}

impl AccountStorable for Payload {}
impl BorshSerialize for Payload {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> IoResult<()> {
        BorshSerialize::serialize(&(Self::STORAGE_MAGIC, Self::STORAGE_VERSION, &self.anchor, &self.contexts), writer)
    }
}
impl BorshDeserialize for Payload {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> IoResult<Self> {
        let magic = u32::deserialize_reader(reader)?;
        let version = u32::deserialize_reader(reader)?;
        let anchor = AccountPublicKey::deserialize_reader(reader)?;
        let contexts = BTreeMap::deserialize_reader(reader)?;
        if magic != Self::STORAGE_MAGIC || version != Self::STORAGE_VERSION {
            return Err(std::io::Error::other("Unsupported KCC-3 account payload"));
        }
        Ok(Self { anchor, contexts })
    }
}

pub struct Kcc3 {
    inner: Arc<Inner>,
    prv_key_data_id: Option<PrvKeyDataId>,
    payload: Payload,
    metadata: AccountMetadata,
    network: NetworkId,
    generation: u64,
    xpub_keys: ExtendedPublicKeys,
    derivation: Option<Arc<Kcc3DerivationManager>>,
}

impl Drop for Kcc3 {
    fn drop(&mut self) {
        self.wallet().release_context_view();
    }
}

impl Kcc3 {
    pub(crate) async fn try_new_from_prv_key_data(
        wallet: &Arc<Wallet>,
        name: Option<String>,
        keydata: &PrvKeyData,
        payment_secret: Option<&Secret>,
        network: NetworkType,
        scheme: SignatureScheme,
        requested_index: Option<u32>,
    ) -> Result<Self> {
        if network != wallet.network_id()?.network_type {
            return Err(Error::custom("KCC-3 creation network mismatch"));
        }
        let index = Self::next_account_index(wallet, &keydata.id, network, scheme, requested_index).await?;
        let origin = AccountDerivation::try_new(network, scheme, index)?;
        let key = keydata.create_kcc3_xpub(payment_secret, &origin)?;
        Self::try_new(wallet, name, Some(keydata.id), key)
    }

    async fn next_account_index(
        wallet: &Arc<Wallet>,
        seed: &PrvKeyDataId,
        network: NetworkType,
        scheme: SignatureScheme,
        requested: Option<u32>,
    ) -> Result<u32> {
        let mut next = 0;
        let mut stored = wallet.store().as_account_store()?.iter(Some(*seed)).await?;
        while let Some(entry) = stored.next().await {
            let (account, _) = entry?;
            if account.kind == KCC3_ACCOUNT_KIND {
                let payload = Payload::try_load(&account)?;
                if let Some(key) = payload.contexts.get(&network)
                    && key.derivation().signature_scheme() == scheme
                {
                    let index = key.derivation().account_index();
                    if requested == Some(index) {
                        return Err(Error::AccountAlreadyExists(account.id));
                    }
                    next = next.max(index + 1);
                }
            }
        }
        let index = requested.unwrap_or(next);
        AccountDerivation::try_new(network, scheme, index)?;
        Ok(index)
    }

    /// Attach the selected network's public context, deriving it only with explicit credentials.
    pub async fn provision_context(
        wallet: &Arc<Wallet>,
        wallet_secret: &Secret,
        id: &AccountId,
        payment_secret: Option<&Secret>,
        watch_key: Option<AccountPublicKey>,
    ) -> Result<()> {
        let guard = wallet.guard();
        let _guard = guard.lock().await;
        let context_guard = wallet.context_guard();
        let _context_guard = context_guard.lock().await;
        if wallet.is_connected() {
            return Err(Error::NetworkTypeConnected);
        }
        let store = wallet.store().as_account_store()?;
        let (storage, meta) = store.load_single(id).await?.ok_or(Error::AccountNotFound(*id))?;
        if !matches!(storage.kind.as_ref(), KCC3_ACCOUNT_KIND | KCC3_WATCH_ACCOUNT_KIND) {
            return Err(Error::InvalidAccountKind);
        }
        let mut payload = Payload::try_load(&storage)?;
        let network = wallet.network_id()?.network_type;
        if payload.contexts.contains_key(&network) {
            return Ok(());
        }
        let key = if storage.kind == KCC3_ACCOUNT_KIND {
            if watch_key.is_some() {
                return Err(Error::custom("Seed-backed KCC-3 contexts must be verified against the seed"));
            }
            let seed: PrvKeyDataId = storage.prv_key_data_ids.clone().try_into()?;
            let keydata = wallet.get_prv_key_data(wallet_secret, &seed).await?.ok_or(Error::PrivateKeyNotFound(seed))?;
            let scheme = payload.anchor.derivation().signature_scheme();
            let index = Self::next_account_index(wallet, &seed, network, scheme, None).await?;
            let origin = AccountDerivation::try_new(network, scheme, index)?;
            keydata.create_kcc3_xpub(payment_secret, &origin)?
        } else {
            watch_key.ok_or_else(|| Error::custom("Watch context requires the destination network's xpub and origin"))?
        };
        if key.derivation().network() != network
            || key.derivation().signature_scheme() != payload.anchor.derivation().signature_scheme()
        {
            return Err(Error::custom("KCC-3 context network/scheme mismatch"));
        }
        payload.contexts.insert(network, key);
        let updated = AccountStorage::try_new(
            storage.kind,
            &storage.id,
            &storage.storage_key,
            storage.prv_key_data_ids.clone(),
            storage.settings.clone(),
            payload,
        )?;
        store.store_single(&updated, meta.as_deref()).await?;
        wallet.store().commit(wallet_secret).await?;
        wallet.notify(Events::AccountUpdate { account_descriptor: Self::try_load(wallet, &updated, meta)?.descriptor()? }).await?;
        Ok(())
    }

    pub(crate) async fn scan_accounts(
        wallet: &Arc<Wallet>,
        mnemonic: Secret,
        passphrase: Option<Secret>,
        address_extent: u32,
        account_extent: u32,
        scheme: SignatureScheme,
    ) -> Result<u32> {
        if address_extent > MAX_INDEX + 1 || account_extent > MAX_INDEX {
            return Err(Error::custom("KCC-3 scan extent exceeds the index range"));
        }
        let mnemonic = Mnemonic::new(mnemonic.as_str()?, Language::English)?;
        let keydata = storage::PrvKeyData::try_new_from_mnemonic(mnemonic, passphrase.as_ref(), EncryptionKind::XChaCha20Poly1305)?;
        let network = wallet.network_id()?;
        let generation = wallet.context_generation();
        let mut last = 0u32;
        let mut index = 0u32;
        while index < last.checked_add(account_extent).ok_or_else(|| Error::custom("KCC-3 account scan exhausted"))? {
            let origin = AccountDerivation::try_new(network.network_type, scheme, index)?;
            let key = keydata.create_kcc3_xpub(passphrase.as_ref(), &origin)?;
            let manager = Kcc3DerivationManager::new(wallet, key, Default::default())?;
            let mut addresses = manager.receive_address_manager().get_range_with_args(0..address_extent, false)?;
            addresses.extend(manager.change_address_manager().get_range_with_args(0..address_extent, false)?);
            let funded = !wallet.rpc_api().get_utxos_by_addresses(addresses).await?.is_empty();
            if wallet.network_id()? != network || wallet.context_generation() != generation {
                return Err(Error::custom("Stale KCC-3 discovery context"));
            }
            if funded {
                last = index;
            }
            index = index.checked_add(1).ok_or_else(|| Error::custom("KCC-3 account index exhausted"))?;
        }
        Ok(last)
    }

    /// Resolve an owned address to its complete origin, without exposing a private key.
    pub fn key_source(&self, address: &Address) -> Result<(secp256k1::PublicKey, Option<kaspa_wallet_pskt::prelude::KeySource>)> {
        let manager = self.derivation()?;
        let (receive, change) = manager.addresses_indexes(&[address])?;
        let (chain, index) = if let Some((_, index)) = receive.first() {
            (0, *index)
        } else if let Some((_, index)) = change.first() {
            (1, *index)
        } else {
            return Err(Error::AddressNotFound);
        };
        let key = self.active_key()?;
        let public = *key.derive_public_key(chain, index)?.public_key();
        let source = key
            .master_fingerprint()
            .map(|key_fingerprint| -> Result<_> {
                Ok(kaspa_wallet_pskt::prelude::KeySource {
                    key_fingerprint,
                    derivation_path: key.derivation().address_path(chain, index)?,
                })
            })
            .transpose()?;
        Ok((public, source))
    }

    fn annotate_bundle(&self, bundle: &mut kaspa_wallet_pskt::bundle::Bundle) -> Result<()> {
        self.ensure_active_context()?;
        // Cache the current change address before looking up output origins.
        self.change_address()?;
        for inner in &mut bundle.0 {
            for input in &mut inner.inputs {
                let utxo = input.utxo_entry.as_ref().ok_or_else(|| Error::custom("KCC-3 bundle input has no UTXO"))?;
                let address = kaspa_txscript::extract_script_pub_key_address(&utxo.script_public_key, self.network.into())?;
                let (public, source) = self.key_source(&address)?;
                input.bip32_derivations.insert(public, source);
            }
            for output in &mut inner.outputs {
                if let Ok(address) = kaspa_txscript::extract_script_pub_key_address(&output.script_public_key, self.network.into())
                    && let Ok((public, source)) = self.key_source(&address)
                {
                    output.bip32_derivations.insert(public, source);
                }
            }
        }
        Ok(())
    }

    pub fn try_new(
        wallet: &Arc<Wallet>,
        name: Option<String>,
        prv_key_data_id: Option<PrvKeyDataId>,
        key: AccountPublicKey,
    ) -> Result<Self> {
        if key.derivation().network() != wallet.network_id()?.network_type {
            return Err(Error::custom("KCC-3 origin network mismatch"));
        }
        let payload = Payload { anchor: key.clone(), contexts: BTreeMap::from([(key.derivation().network(), key)]) };
        let kind = if prv_key_data_id.is_some() { KCC3_ACCOUNT_KIND } else { KCC3_WATCH_ACCOUNT_KIND };
        let (id, storage_key) = payload.identity(kind.into())?;
        let settings = AccountSettings { name, ..Default::default() };
        let storage = AccountStorage::try_new(
            kind.into(),
            &id,
            &storage_key,
            prv_key_data_id.map(Into::into).unwrap_or(AssocPrvKeyDataIds::None),
            settings,
            payload,
        )?;
        Self::try_load(wallet, &storage, None)
    }

    pub fn try_load(wallet: &Arc<Wallet>, storage: &AccountStorage, meta: Option<Arc<AccountMetadata>>) -> Result<Self> {
        let payload = Payload::try_load(storage)?;
        let prv_key_data_id = match (storage.kind.as_ref(), &storage.prv_key_data_ids) {
            (KCC3_ACCOUNT_KIND, AssocPrvKeyDataIds::Single(id)) => Some(*id),
            (KCC3_WATCH_ACCOUNT_KIND, AssocPrvKeyDataIds::None) => None,
            _ => return Err(Error::custom("KCC-3 account kind/key association mismatch")),
        };
        let (id, storage_key) = payload.identity(storage.kind)?;
        if id != storage.id || storage_key != storage.storage_key {
            return Err(Error::custom("KCC-3 account identity mismatch"));
        }
        let network = wallet.network_id()?;
        let metadata = meta.map(|m| (*m).clone()).unwrap_or_else(|| AccountMetadata::new_kcc3(id));
        if metadata.id != id || metadata.indexes.is_some() || metadata.kcc3_indexes.is_none() {
            return Err(Error::custom("Invalid KCC-3 metadata"));
        }
        let key = payload.contexts.get(&network.network_type);
        let xpub_keys = Arc::new(key.map(|k| vec![k.xpub().clone()]).unwrap_or_default());
        let derivation =
            key.map(|key| Kcc3DerivationManager::new(wallet, key.clone(), metadata.kcc3_indexes(network.network_type))).transpose()?;
        wallet.retain_context_view();
        Ok(Self {
            inner: Arc::new(Inner::from_storage(wallet, storage)),
            prv_key_data_id,
            payload,
            metadata,
            network,
            generation: wallet.context_generation(),
            xpub_keys,
            derivation,
        })
    }

    pub fn public_description(&self) -> Result<PublicAccountDescription> {
        self.ensure_active_context()?;
        Ok(PublicAccountDescription {
            version: 1,
            convention: "kcc3".into(),
            kind: if self.prv_key_data_id.is_some() { "kcc3" } else { "kcc3-watch" }.into(),
            network: self.active_key()?.derivation().network(),
            signature_scheme: self.active_key()?.derivation().signature_scheme(),
            account_index: self.active_key()?.derivation().account_index(),
            key: self.active_key()?.clone(),
        })
    }

    fn active_key(&self) -> Result<&AccountPublicKey> {
        self.payload
            .contexts
            .get(&self.network.network_type)
            .ok_or_else(|| Error::custom("KCC-3 requires an account xpub and origin for this network"))
    }
}

#[async_trait]
impl Account for Kcc3 {
    fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }
    fn account_kind(&self) -> AccountKind {
        if self.prv_key_data_id.is_some() { KCC3_ACCOUNT_KIND.into() } else { KCC3_WATCH_ACCOUNT_KIND.into() }
    }
    fn as_dyn_arc(self: Arc<Self>) -> Arc<dyn Account> {
        self
    }
    fn xpub_keys(&self) -> Option<&ExtendedPublicKeys> {
        Some(&self.xpub_keys)
    }
    fn prv_key_data_id(&self) -> Result<&PrvKeyDataId> {
        self.prv_key_data_id.as_ref().ok_or_else(|| Error::custom("KCC-3 watch account cannot sign"))
    }
    fn ensure_active_context(&self) -> Result<()> {
        if self.wallet().network_id()? != self.network || self.wallet().context_generation() != self.generation {
            return Err(Error::custom("Stale KCC-3 network context"));
        }
        if self.wallet().active_accounts().get(self.id()).is_some_and(|installed| !Arc::ptr_eq(installed.inner(), self.inner())) {
            return Err(Error::custom("Replaced KCC-3 runtime view"));
        }
        self.active_key()?;
        Ok(())
    }
    fn balance(&self) -> Option<Balance> {
        self.ensure_active_context().ok().and_then(|_| self.utxo_context().balance())
    }
    fn sig_op_count(&self) -> u8 {
        1
    }
    fn minimum_signatures(&self) -> u16 {
        1
    }
    fn receive_address(&self) -> Result<Address> {
        self.derivation()?.receive_address_manager().current_address()
    }
    fn change_address(&self) -> Result<Address> {
        self.derivation()?.change_address_manager().current_address()
    }
    fn default_address(&self) -> Result<Address> {
        self.derivation()?.receive_address_manager().get_range_with_args(0..1, false)?.first().cloned().ok_or(Error::AddressNotFound)
    }
    fn account_addresses(&self) -> Result<Vec<Address>> {
        let manager = self.derivation()?;
        let receive = manager.receive_address_manager();
        let change = manager.change_address_manager();
        let mut addresses = receive.get_range_with_args(0..receive.index() + 1, false)?;
        addresses.extend(change.get_range_with_args(0..change.index() + 1, false)?);
        Ok(addresses)
    }
    fn to_storage(&self) -> Result<AccountStorage> {
        self.ensure_active_context()?;
        AccountStorage::try_new(
            self.account_kind(),
            self.id(),
            self.storage_key(),
            self.prv_key_data_id.map(Into::into).unwrap_or(AssocPrvKeyDataIds::None),
            self.context().settings.clone(),
            self.payload.clone(),
        )
    }
    fn metadata(&self) -> Result<Option<AccountMetadata>> {
        self.ensure_active_context()?;
        let mut metadata = self.metadata.clone();
        if let Some(manager) = &self.derivation {
            metadata.set_kcc3_indexes(
                self.network.network_type,
                AddressDerivationMeta::new(manager.receive_address_manager().index(), manager.change_address_manager().index()),
            )?;
        }
        Ok(Some(metadata))
    }
    fn descriptor(&self) -> Result<AccountDescriptor> {
        let available = self.ensure_active_context().is_ok();
        let mut descriptor = AccountDescriptor::new(
            self.account_kind(),
            *self.id(),
            self.name(),
            if available { self.balance() } else { None },
            self.prv_key_data_id.map(Into::into).unwrap_or(AssocPrvKeyDataIds::None),
            self.receive_address().ok(),
            self.change_address().ok(),
            None,
        )
        .with_property(AccountDescriptorProperty::Other("available".into()), available.into());
        if available {
            descriptor = descriptor
                .with_property(AccountDescriptorProperty::AccountIndex, self.account_index().into())
                .with_property(AccountDescriptorProperty::XpubKeys, self.xpub_keys.clone().into())
                .with_property(
                    AccountDescriptorProperty::Ecdsa,
                    (self.active_key()?.derivation().signature_scheme() == SignatureScheme::Ecdsa).into(),
                )
                .with_property(
                    AccountDescriptorProperty::Other("kcc3".into()),
                    crate::account::descriptor::AccountDescriptorValue::Json(serde_json::to_string(&self.public_description()?)?),
                );
        } else {
            descriptor = descriptor.with_property(
                AccountDescriptorProperty::Other("unavailableReason".into()),
                "Import or provision the destination network's KCC-3 account key".to_string().into(),
            );
        }
        Ok(descriptor)
    }
    fn as_derivation_capable(self: Arc<Self>) -> Result<Arc<dyn DerivationCapableAccount>> {
        Ok(self)
    }
    async fn create_signer(
        self: Arc<Self>,
        wallet_secret: Secret,
        payment_secret: Option<Secret>,
    ) -> Result<Arc<dyn crate::tx::SignerT>> {
        self.ensure_active_context()?;
        let keydata = self.prv_key_data(wallet_secret).await?;
        self.ensure_active_context()?;
        Ok(Arc::new(crate::tx::generator::signer::Kcc3Signer::new(self, keydata, payment_secret)))
    }

    async fn pskb_from_send_generator(
        self: Arc<Self>,
        destination: crate::tx::PaymentDestination,
        fee_rate: Option<f64>,
        priority_fee_sompi: crate::tx::Fees,
        payload: Option<Vec<u8>>,
        _wallet_secret: Secret,
        _payment_secret: Option<Secret>,
        abortable: &Abortable,
    ) -> Result<kaspa_wallet_pskt::bundle::Bundle> {
        self.ensure_active_context()?;
        let settings =
            crate::tx::GeneratorSettings::try_new_with_account(self.clone(), destination, fee_rate, priority_fee_sompi, payload)?;
        let generator = crate::tx::Generator::try_new(settings, None, Some(abortable))?;
        let mut bundle = crate::account::pskb::bundle_from_pskt_generator(crate::account::pskb::PSKTGenerator::new_unsigned(
            generator,
            self.network.into(),
        ))
        .await?;
        self.annotate_bundle(&mut bundle)?;
        Ok(bundle)
    }

    async fn pskb_sign(
        self: Arc<Self>,
        bundle: &kaspa_wallet_pskt::bundle::Bundle,
        wallet_secret: Secret,
        payment_secret: Option<Secret>,
        sign_for_address: Option<&Address>,
    ) -> Result<kaspa_wallet_pskt::bundle::Bundle> {
        self.ensure_active_context()?;
        self.prv_key_data_id()?;
        let keydata = self.prv_key_data(wallet_secret).await?;
        self.ensure_active_context()?;
        let signer = Arc::new(crate::account::pskb::PSKBSigner::new(self.clone(), keydata, payment_secret));
        let mut bundle =
            crate::account::pskb::pskb_signer_for_address(bundle, signer, self.network, sign_for_address, None, None).await?;
        self.annotate_bundle(&mut bundle)?;
        Ok(bundle)
    }
}

#[async_trait]
impl DerivationCapableAccount for Kcc3 {
    fn derivation(&self) -> Result<Arc<dyn AddressDerivationManagerTrait>> {
        self.ensure_active_context()?;
        Ok(self.derivation.clone().ok_or_else(|| Error::custom("Unavailable KCC-3 network context"))?)
    }
    fn account_index(&self) -> u64 {
        self.active_key().map(|k| k.derivation().account_index() as u64).unwrap_or(0)
    }
    fn create_private_keys<'l>(
        &self,
        key_data: &PrvKeyData,
        payment_secret: &Option<Secret>,
        receive: &[(&'l Address, u32)],
        change: &[(&'l Address, u32)],
    ) -> Result<Vec<(&'l Address, secp256k1::SecretKey)>> {
        self.ensure_active_context()?;
        self.prv_key_data_id()?;
        let master = key_data.get_xprv(payment_secret.as_ref())?;
        let key = self.active_key()?;
        if key.derivation().derive_account(&master)?.public_key() != *key.xpub() {
            return Err(Error::custom("KCC-3 private key does not match account xpub"));
        }
        receive
            .iter()
            .map(|(address, index)| Ok((*address, key.derivation().derive_private_key(&master, 0, *index)?)))
            .chain(change.iter().map(|(address, index)| Ok((*address, key.derivation().derive_private_key(&master, 1, *index)?))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_wallet_keys::derivation::gen2::{AccountDerivation, MAX_INDEX};

    struct Fixture {
        wallet: Arc<Wallet>,
        secret: Secret,
        keydata: PrvKeyData,
        account: Arc<dyn Account>,
    }

    impl Fixture {
        async fn new(scheme: SignatureScheme) -> Result<Self> {
            let wallet = Arc::new(Wallet::try_with_rpc(None, Wallet::resident_store()?, Some(NetworkId::new(NetworkType::Mainnet)))?);
            let secret = Secret::from("kcc3-public-test-password");
            wallet.create_wallet(&secret, WalletCreateArgs::new(None, None, EncryptionKind::XChaCha20Poly1305, None, false)).await?;
            let mnemonic = kaspa_bip32::Mnemonic::new(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                kaspa_bip32::Language::English,
            )?;
            let keydata = PrvKeyData::try_new_from_mnemonic(mnemonic, None, EncryptionKind::XChaCha20Poly1305)?;
            wallet.store().as_prv_key_data_store()?.store(&secret, keydata.clone()).await?;
            let guard = wallet.guard();
            let guard = guard.lock().await;
            let account = wallet
                .create_account(
                    &secret,
                    AccountCreateArgs::Kcc3 {
                        prv_key_data_args: PrvKeyDataArgs::new(keydata.id, None),
                        account_name: None,
                        network: NetworkType::Mainnet,
                        signature_scheme: scheme,
                        account_index: None,
                    },
                    false,
                    &guard,
                )
                .await?;
            Ok(Self { wallet, secret, keydata, account })
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn network_contexts_counters_and_stale_handles() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        f.wallet.select(Some(&f.account)).await?;
        let derivation = f.account.clone().as_derivation_capable()?.derivation()?;
        let receive = derivation.receive_address_manager();
        let change = derivation.change_address_manager();
        let initial = f.account.receive_address()?;
        let advanced = f.account.clone().as_derivation_capable()?.new_receive_address().await?;
        assert_ne!(initial, advanced);
        let testnet: NetworkId = "testnet-10".parse()?;
        assert!(f.wallet.set_network_id(&testnet).is_err());
        f.wallet.change_network_context(&testnet).await?;
        assert!(f.account.receive_address().is_err());
        assert!(derivation.receive_address_manager().new_address().is_err());
        for manager in [&receive, &change] {
            let index = manager.index();
            let cached = manager.inner().address_to_index_map.clone();
            assert!(manager.ensure_active_context().is_err());
            assert!(manager.current_address().is_err());
            assert!(manager.set_index(8).is_err());
            assert!(manager.get_range(8..16).is_err());
            assert!(manager.get_range_with_args(8..16, false).is_err());
            assert_eq!(manager.index(), index);
            assert_eq!(manager.inner().address_to_index_map, cached);
        }
        let master = f.keydata.get_xprv(None)?;
        assert!(derivation.get_range_with_keys(false, 0..1, true, &master).await.is_err());
        assert!(derivation.get_range_with_keys(true, 0..1, true, &master).await.is_err());
        assert!(f.account.metadata().is_err());
        let guard = f.wallet.guard();
        let locked = guard.lock().await;
        let missing = f.wallet.get_account_by_id(f.account.id(), &locked).await?.unwrap();
        assert!(missing.descriptor()?.receive_address.is_none());
        assert!(missing.ensure_active_context().is_err());
        drop(locked);
        Kcc3::provision_context(&f.wallet, &f.secret, f.account.id(), None, None).await?;
        let locked = guard.lock().await;
        let test_account = f.wallet.get_account_by_id(f.account.id(), &locked).await?.unwrap();
        drop(locked);
        f.wallet.select(Some(&test_account)).await?;
        let test_address = test_account.clone().as_derivation_capable()?.new_receive_address().await?;
        assert_ne!(initial.payload, test_address.payload);
        f.wallet.change_network_context(&"testnet-11".parse()?).await?;
        let next_test = f.wallet.account()?;
        assert_eq!(next_test.receive_address()?, test_address);
        assert!(test_account.receive_address().is_err());
        f.wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
        let restored = f.wallet.account()?;
        assert_eq!(restored.id(), f.account.id());
        assert_eq!(restored.receive_address()?, advanced);
        assert!(f.account.receive_address().is_err());
        let current = restored.clone().as_derivation_capable()?.derivation()?;
        current.receive_address_manager().set_index(MAX_INDEX)?;
        assert!(current.receive_address_manager().new_address().is_err());
        assert_eq!(current.receive_address_manager().index(), MAX_INDEX);
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn watch_ranges_identity_and_serialization() -> Result<()> {
        for scheme in [SignatureScheme::Schnorr, SignatureScheme::Ecdsa] {
            let f = Fixture::new(scheme).await?;
            let source = f.account.downcast_ref::<Kcc3>().unwrap();
            let description = source.public_description()?;
            let json = serde_json::to_string(&description)?;
            let key = serde_json::from_str::<PublicAccountDescription>(&json)?.try_key()?;
            let imported = PublicAccountDescription::try_from_xpub(description.network, scheme, key.xpub().to_string(None).parse()?)?;
            assert_eq!(imported.key.derivation(), key.derivation());
            let watch = Arc::new(Kcc3::try_new(&f.wallet, Some("watch".into()), None, imported.try_key()?)?);
            assert_ne!(watch.id(), source.id());
            assert!(watch.prv_key_data_id().is_err());
            assert!(watch.clone().create_signer(f.secret.clone(), None).await.is_err());
            assert_eq!(watch.receive_address()?, source.receive_address()?);
            let manager = watch.derivation()?;
            let seed_manager = source.derivation()?;
            for range in [0..8, 8..16, 100..109] {
                assert_eq!(
                    manager.receive_address_manager().get_range(range.clone())?,
                    seed_manager.receive_address_manager().get_range(range.clone())?
                );
                assert_eq!(
                    manager.change_address_manager().get_range(range.clone())?,
                    seed_manager.change_address_manager().get_range(range)?
                );
            }
            let stored = source.to_storage()?;
            let payload = Payload::try_load(&stored)?;
            let mut updated = payload.clone();
            let other = AccountDerivation::try_new(NetworkType::Testnet, scheme, 3)?;
            updated.contexts.insert(NetworkType::Testnet, f.keydata.create_kcc3_xpub(None, &other)?);
            assert_eq!(payload.identity(KCC3_ACCOUNT_KIND.into())?, updated.identity(KCC3_ACCOUNT_KIND.into())?);
            updated.anchor = AccountPublicKey::try_new(payload.anchor.derivation().clone(), None, payload.anchor.xpub().clone())?;
            assert_eq!(payload.identity(KCC3_ACCOUNT_KIND.into())?, updated.identity(KCC3_ACCOUNT_KIND.into())?);
            let reloaded = Kcc3::try_load(&f.wallet, &stored, source.metadata()?.map(Arc::new))?;
            assert_eq!(reloaded.receive_address()?, source.receive_address()?);
            let mut wrong = stored.clone();
            wrong.kind = KCC3_WATCH_ACCOUNT_KIND.into();
            assert!(Kcc3::try_load(&f.wallet, &wrong, None).is_err());
            let master = f.keydata.get_xprv(None)?;
            let keyed = seed_manager.get_range_with_keys(false, 8..16, false, &master).await?;
            for (address, key) in keyed {
                assert_eq!(
                    address,
                    kaspa_wallet_keys::derivation::gen1::PubkeyDerivationManager::create_address(
                        &key.public_key(secp256k1::SECP256K1),
                        Prefix::Mainnet,
                        scheme == SignatureScheme::Ecdsa
                    )?
                );
            }
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn public_xpub_import_preserves_account_origin() -> Result<()> {
        let master = kaspa_bip32::ExtendedPrivateKey::<secp256k1::SecretKey>::new([19; 32])?;
        for network in [NetworkType::Mainnet, NetworkType::Testnet, NetworkType::Simnet, NetworkType::Devnet] {
            for scheme in [SignatureScheme::Schnorr, SignatureScheme::Ecdsa] {
                for index in [0, 7, MAX_INDEX] {
                    let origin = AccountDerivation::try_new(network, scheme, index)?;
                    let xpub = origin.derive_account(&master)?.public_key();
                    for prefix in [kaspa_bip32::Prefix::XPUB, kaspa_bip32::Prefix::KPUB, kaspa_bip32::Prefix::KTUB] {
                        let imported =
                            PublicAccountDescription::try_from_xpub(network, scheme, xpub.to_string(Some(prefix)).parse()?)?;
                        assert_eq!(imported.account_index, index);
                        let key = imported.try_key()?;
                        assert_eq!(key.derivation(), &origin);
                        assert_eq!(key.master_fingerprint(), None);
                        for change in [0, 1] {
                            assert_eq!(
                                key.derive_public_key(change, 3)?.public_key(),
                                &origin.derive_private_key(&master, change, 3)?.public_key(secp256k1::SECP256K1)
                            );
                        }
                    }
                    let mut invalid = xpub.clone();
                    invalid.attrs.child_number = kaspa_bip32::ChildNumber::new(index, false)?;
                    assert!(PublicAccountDescription::try_from_xpub(network, scheme, invalid).is_err());
                    for depth in [0, 3, 5] {
                        let mut invalid = xpub.clone();
                        invalid.attrs.depth = depth;
                        assert!(PublicAccountDescription::try_from_xpub(network, scheme, invalid).is_err());
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn derivation_range_boundaries_and_private_key_cache() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        let derivation = f.account.clone().as_derivation_capable()?.derivation()?;
        let master = f.keydata.get_xprv(None)?;
        let wrong_master = kaspa_bip32::ExtendedPrivateKey::<secp256k1::SecretKey>::new([23; 32])?;
        for change in [false, true] {
            let manager = if change { derivation.change_address_manager() } else { derivation.receive_address_manager() };
            let cached = manager.inner().address_to_index_map.clone();
            for range in [std::ops::Range { start: 2, end: 1 }, MAX_INDEX..MAX_INDEX + 2] {
                assert_eq!(manager.get_range(range.clone()).unwrap_err().to_string(), "Invalid KCC-3 address range");
                assert_eq!(
                    derivation.get_range_with_keys(change, range, true, &master).await.unwrap_err().to_string(),
                    "Invalid KCC-3 address range"
                );
            }
            assert_eq!(
                derivation.get_range_with_keys(change, 8..10, true, &wrong_master).await.unwrap_err().to_string(),
                "KCC-3 private key does not match account xpub"
            );
            assert!(manager.get_range(MAX_INDEX + 1..MAX_INDEX + 1)?.is_empty());
            assert!(derivation.get_range_with_keys(change, MAX_INDEX + 1..MAX_INDEX + 1, true, &master).await?.is_empty());
            assert_eq!(manager.set_index(MAX_INDEX + 1).unwrap_err().to_string(), "Invalid KCC-3 address index");
            assert_eq!(manager.index(), 0);
            assert_eq!(manager.inner().address_to_index_map, cached);

            let keyed = derivation.get_range_with_keys(change, 8..10, false, &master).await?;
            assert_eq!(manager.inner().address_to_index_map, cached);
            assert_eq!(keyed, derivation.get_range_with_keys(change, 8..10, true, &master).await?);
            let addresses = keyed.iter().map(|(address, _)| address).collect::<Vec<_>>();
            let (receive, change_indexes) = derivation.addresses_indexes(&addresses)?;
            let expected = vec![(addresses[0], 8), (addresses[1], 9)];
            if change {
                assert!(receive.is_empty());
                assert_eq!(change_indexes, expected);
            } else {
                assert_eq!(receive, expected);
                assert!(change_indexes.is_empty());
            }
            assert_eq!(manager.index(), 0);
            manager.set_index(MAX_INDEX)?;
            assert_eq!(manager.new_address().unwrap_err().to_string(), "KCC-3 address index exhausted");
            assert_eq!(manager.index(), MAX_INDEX);
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn watch_unsigned_bundle_and_both_scheme_signatures() -> Result<()> {
        use kaspa_consensus_core::tx::{TransactionOutpoint, UtxoEntry};
        for scheme in [SignatureScheme::Schnorr, SignatureScheme::Ecdsa] {
            let f = Fixture::new(scheme).await?;
            let source = f.account.downcast_ref::<Kcc3>().unwrap();
            let description = source.public_description()?;
            let imported = PublicAccountDescription::try_from_xpub(description.network, scheme, description.key.xpub().clone())?;
            let watch = Arc::new(Kcc3::try_new(&f.wallet, None, None, imported.try_key()?)?);
            let receive = watch.receive_address()?;
            let utxo = kaspa_consensus_client::UtxoEntry {
                address: Some(receive.clone()),
                outpoint: TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([17; 32]), 0).into(),
                amount: 100_000_000,
                script_public_key: kaspa_txscript::pay_to_address_script(&receive),
                block_daa_score: 0,
                is_coinbase: false,
                covenant_id: None,
            };
            watch.utxo_context().extend_from_scan(vec![utxo.into()], 1_000_000).await?;
            let destination =
                crate::tx::PaymentDestination::from(crate::tx::PaymentOutput::new(f.account.change_address()?, 50_000_000));
            let bundle = watch
                .clone()
                .pskb_from_send_generator(
                    destination,
                    None,
                    crate::tx::Fees::SenderPays(0),
                    None,
                    Secret::from("unused"),
                    None,
                    &Abortable::new(),
                )
                .await?;
            assert!(!bundle.0.is_empty());
            assert!(bundle.0.iter().all(|inner| inner.inputs.iter().all(|input| input.partial_sigs.is_empty())));
            assert!(bundle.0.iter().any(|inner| inner.outputs.iter().any(|output| !output.bip32_derivations.is_empty())));
            for input in bundle.0.iter().flat_map(|inner| &inner.inputs) {
                assert!(!input.bip32_derivations.is_empty());
                assert!(input.bip32_derivations.values().all(Option::is_none));
            }
            f.account.receive_address()?;
            let signed = f.account.clone().pskb_sign(&bundle, f.secret.clone(), None, None).await?;
            for input in signed.0.iter().flat_map(|inner| &inner.inputs) {
                let source = input.bip32_derivations.values().next().unwrap().as_ref().unwrap();
                assert_eq!(source.derivation_path.iter().count(), 6);
                assert_eq!(source.key_fingerprint, f.keydata.get_xprv(None)?.public_key().fingerprint());
            }
            let signature = signed.0[0].inputs[0].partial_sigs.values().next().unwrap();
            assert!(matches!(
                (scheme, signature),
                (SignatureScheme::Schnorr, kaspa_wallet_pskt::pskt::Signature::Schnorr(_))
                    | (SignatureScheme::Ecdsa, kaspa_wallet_pskt::pskt::Signature::ECDSA(_))
            ));
            let mut stream = crate::account::pskb::bundle_to_finalizer_stream(&signed);
            while let Some(finalized) = stream.next().await {
                let finalized = finalized?;
                let extracted = finalized.extractor().unwrap().extract_tx(&NetworkType::Mainnet.into()).unwrap();
                assert!(extracted.entries.iter().all(Option::is_some));
                assert!(extracted.entries.iter().flatten().all(|entry: &UtxoEntry| entry.amount > 0));
            }
            assert!(watch.pskb_sign(&bundle, f.secret.clone(), None, None).await.is_err());
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn backup_coexistence_allocation_and_missing_context_return() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        let mnemonic = f.keydata.as_mnemonic(None)?.unwrap();
        let old = f.wallet.import_with_mnemonic(&f.secret, None, mnemonic.clone(), BIP32_ACCOUNT_KIND.into()).await?;
        assert_ne!(old.receive_address()?, f.account.receive_address()?);
        assert!(f.wallet.import_with_mnemonic(&f.secret, None, mnemonic, KCC3_ACCOUNT_KIND.into()).await.is_err());
        let guard = f.wallet.guard();
        let locked = guard.lock().await;
        let next = f
            .wallet
            .create_account(
                &f.secret,
                AccountCreateArgs::Kcc3 {
                    prv_key_data_args: PrvKeyDataArgs::new(f.keydata.id, None),
                    account_name: None,
                    network: NetworkType::Mainnet,
                    signature_scheme: SignatureScheme::Schnorr,
                    account_index: None,
                },
                false,
                &locked,
            )
            .await?;
        assert_eq!(next.clone().as_derivation_capable()?.account_index(), 1);
        drop(locked);
        let ecdsa = f
            .wallet
            .import_mnemonic(
                &f.secret,
                None,
                f.keydata.as_mnemonic(None)?.unwrap(),
                AccountImportArgs::Kcc3 { origin: Some(AccountDerivation::try_new(NetworkType::Mainnet, SignatureScheme::Ecdsa, 0)?) },
            )
            .await?;
        assert_eq!(ecdsa.clone().as_derivation_capable()?.account_index(), 0);
        f.wallet.select(Some(&f.account)).await?;
        let address = f.account.clone().as_derivation_capable()?.new_change_address().await?;
        f.wallet.change_network_context(&NetworkId::new(NetworkType::Devnet)).await?;
        assert!(f.wallet.account().is_err());
        f.wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
        let restored = f.wallet.account()?;
        assert_eq!(restored.id(), f.account.id());
        assert_eq!(restored.change_address()?, address);
        let backup = f
            .wallet
            .store()
            .wallet_export(&f.secret, crate::storage::interface::WalletExportOptions { include_transactions: false })
            .await?;
        let backup = crate::storage::local::WalletStorage::try_from_slice(&backup)?;
        let payload = backup.payload(&f.secret)?;
        assert_eq!(payload.as_ref().prv_key_data.len(), 1);
        let stored = payload.as_ref().accounts.iter().find(|a| a.id == *restored.id()).unwrap();
        assert_eq!(stored.kind, KCC3_ACCOUNT_KIND);
        let meta = backup.metadata.iter().find(|m| m.id == stored.id).unwrap();
        let restored_wallet =
            Arc::new(Wallet::try_with_rpc(None, Wallet::resident_store()?, Some(NetworkId::new(NetworkType::Mainnet)))?);
        let loaded = Kcc3::try_load(&restored_wallet, stored, Some(Arc::new(meta.clone())))?;
        assert_eq!(loaded.change_address()?, address);
        assert!(payload.as_ref().accounts.iter().any(|a| a.kind == BIP32_ACCOUNT_KIND));
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn discovery_subtrees_keep_utxo_extents_independent() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Ecdsa).await?;
        let rpc = Arc::new(crate::tests::RpcCoreMock::new());
        let wallet = Arc::new(Wallet::try_with_rpc(
            Some(rpc.clone().into()),
            Wallet::resident_store()?,
            Some(NetworkId::new(NetworkType::Mainnet)),
        )?);
        let origin = AccountDerivation::try_new(NetworkType::Mainnet, SignatureScheme::Ecdsa, 2)?;
        let key = f.keydata.create_kcc3_xpub(None, &origin)?;
        let public = key.derive_public_key(1, 7)?;
        let address =
            kaspa_wallet_keys::derivation::gen1::PubkeyDerivationManager::create_address(public.public_key(), Prefix::Mainnet, true)?;
        let entry = kaspa_consensus_core::tx::UtxoEntry::new(100, kaspa_txscript::pay_to_address_script(&address), 0, false, None);
        *rpc.utxo_entries.lock().unwrap() = Some(vec![kaspa_rpc_core::RpcUtxosByAddressesEntry {
            address: Some(address.clone()),
            outpoint: kaspa_consensus_core::tx::TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([9; 32]), 0).into(),
            utxo_entry: entry.into(),
        }]);
        let mnemonic = Secret::from(f.keydata.as_mnemonic(None)?.unwrap().phrase());
        assert_eq!(Kcc3::scan_accounts(&wallet, mnemonic.clone(), None, 8, 3, SignatureScheme::Schnorr).await?, 0);
        assert_eq!(rpc.utxo_requests.lock().unwrap().len(), 3);
        assert_eq!(Kcc3::scan_accounts(&wallet, mnemonic, None, 8, 3, SignatureScheme::Ecdsa).await?, 2);
        let requests = rpc.utxo_requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        assert!(requests.iter().all(|request| request.len() == 16));
        assert!(!requests[..3].iter().any(|request| request.contains(&address)));
        assert!(requests[5].contains(&address));
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn watch_context_provisioning_and_default_creation() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        let source = f.account.downcast_ref::<Kcc3>().unwrap();
        let description = source.public_description()?;
        let mut wrong = description.clone();
        wrong.signature_scheme = SignatureScheme::Ecdsa;
        assert!(wrong.try_key().is_err());
        let args = AccountCreateArgs::Kcc3Watch { account_name: None, description };
        let bytes = borsh::to_vec(&args)?;
        assert_eq!(bytes[0], 6); // New variants follow all five historical variants.
        let guard = f.wallet.guard();
        let locked = guard.lock().await;
        let watch = f.wallet.create_account(&f.secret, AccountCreateArgs::try_from_slice(&bytes)?, false, &locked).await?;
        drop(locked);
        f.wallet.select(Some(&watch)).await?;
        let testnet: NetworkId = "testnet-10".parse()?;
        f.wallet.change_network_context(&testnet).await?;
        assert!(Kcc3::provision_context(&f.wallet, &f.secret, watch.id(), None, None).await.is_err());
        let destination =
            f.keydata.create_kcc3_xpub(None, &AccountDerivation::try_new(NetworkType::Testnet, SignatureScheme::Schnorr, 4)?)?;
        Kcc3::provision_context(&f.wallet, &f.secret, watch.id(), None, Some(destination)).await?;
        let locked = guard.lock().await;
        let provisioned = f.wallet.get_account_by_id(watch.id(), &locked).await?.unwrap();
        assert_eq!(provisioned.id(), watch.id());
        assert_eq!(provisioned.clone().as_derivation_capable()?.account_index(), 4);
        assert!(provisioned.receive_address().is_ok());
        drop(locked);
        for kind in [None, Some(BIP32_ACCOUNT_KIND.into())] {
            let wallet = Arc::new(Wallet::try_with_rpc(None, Wallet::resident_store()?, Some(NetworkId::new(NetworkType::Mainnet)))?);
            let (_, _, _, account) = wallet
                .create_wallet_with_accounts(
                    &f.secret,
                    WalletCreateArgs::new(None, None, EncryptionKind::XChaCha20Poly1305, None, false),
                    None,
                    kind,
                    kaspa_bip32::WordCount::Words12,
                    None,
                )
                .await?;
            assert_eq!(account.account_kind(), kind.unwrap_or(KCC3_ACCOUNT_KIND.into()));
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn default_account_creation_rejects_unsupported_kinds() -> Result<()> {
        use crate::api::message::AccountsEnsureDefaultRequest;
        use crate::api::traits::WalletApi;

        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        f.wallet.select(Some(&f.account)).await?;
        for kind in
            [LEGACY_ACCOUNT_KIND, MULTISIG_ACCOUNT_KIND, BIP32_WATCH_ACCOUNT_KIND, KCC3_WATCH_ACCOUNT_KIND, KEYPAIR_ACCOUNT_KIND]
        {
            let error = f
                .wallet
                .clone()
                .accounts_ensure_default_call(AccountsEnsureDefaultRequest {
                    wallet_secret: f.secret.clone(),
                    payment_secret: None,
                    account_kind: kind.into(),
                    mnemonic_phrase: None,
                })
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "Account kind is not supported");
        }
        assert!(
            f.wallet
                .create_wallet_with_accounts(
                    &f.secret,
                    WalletCreateArgs::new(None, None, EncryptionKind::XChaCha20Poly1305, None, false),
                    None,
                    Some(LEGACY_ACCOUNT_KIND.into()),
                    kaspa_bip32::WordCount::Words12,
                    None,
                )
                .await
                .is_err()
        );
        assert_eq!(f.wallet.account()?.id(), f.account.id());
        assert!(f.wallet.account()?.receive_address().is_ok());
        for kind in [KCC3_ACCOUNT_KIND, BIP32_ACCOUNT_KIND] {
            let wallet = Arc::new(Wallet::try_with_rpc(None, Wallet::resident_store()?, Some(NetworkId::new(NetworkType::Mainnet)))?);
            wallet.create_wallet(&f.secret, WalletCreateArgs::new(None, None, EncryptionKind::XChaCha20Poly1305, None, false)).await?;
            assert!(
                wallet
                    .clone()
                    .accounts_ensure_default_call(AccountsEnsureDefaultRequest {
                        wallet_secret: f.secret.clone(),
                        payment_secret: None,
                        account_kind: LEGACY_ACCOUNT_KIND.into(),
                        mnemonic_phrase: None,
                    })
                    .await
                    .is_err()
            );
            assert!(wallet.store().as_account_store()?.iter(None).await?.next().await.is_none());
            let response = wallet
                .accounts_ensure_default_call(AccountsEnsureDefaultRequest {
                    wallet_secret: f.secret.clone(),
                    payment_secret: None,
                    account_kind: kind.into(),
                    mnemonic_phrase: Some(Secret::from(f.keydata.as_mnemonic(None)?.unwrap().phrase())),
                })
                .await?;
            assert_eq!(response.account_descriptor.kind, kind);
            assert!(response.account_descriptor.receive_address.is_some());
        }
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn network_transitions_preserve_legacy_public_cache() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        let legacy =
            f.wallet.import_with_mnemonic(&f.secret, None, f.keydata.as_mnemonic(None)?.unwrap(), LEGACY_ACCOUNT_KIND.into()).await?;
        f.wallet.select(Some(&legacy)).await?;
        let address = legacy.receive_address()?;
        f.wallet.change_network_context(&"testnet-10".parse()?).await?;
        let test_address = f.wallet.account()?.receive_address()?;
        assert_eq!(test_address.prefix, Prefix::Testnet);
        assert_eq!(test_address.payload, address.payload);
        f.wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
        assert_eq!(f.wallet.account()?.receive_address()?, address);
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn reset_waits_for_in_flight_context_and_invalidates_views() -> Result<()> {
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        f.wallet.select(Some(&f.account)).await?;
        let context_guard = f.wallet.context_guard();
        let locked = context_guard.lock().await;
        let mut reset = Box::pin(f.wallet.reset(true));
        assert!(futures::poll!(reset.as_mut()).is_pending());
        assert_eq!(f.wallet.account()?.id(), f.account.id());
        drop(locked);
        reset.await?;
        assert!(f.wallet.account().is_err());
        assert!(f.wallet.active_accounts().is_empty());
        assert!(f.account.ensure_active_context().is_err());
        assert!(f.account.receive_address().is_err());
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn unavailable_accounts_remain_enumerable_and_can_be_deactivated() -> Result<()> {
        use crate::api::message::{AccountsActivateRequest, AccountsDeactivateRequest};
        use crate::api::traits::WalletApi;

        for targeted in [false, true] {
            let f = Fixture::new(SignatureScheme::Schnorr).await?;
            f.wallet.change_network_context(&NetworkId::new(NetworkType::Devnet)).await?;
            let guard = f.wallet.guard();
            let locked = guard.lock().await;
            let accounts = f.wallet.accounts(None, &locked).await?.try_collect::<Vec<_>>().await?;
            assert_eq!(accounts.len(), 1);
            assert!(accounts[0].descriptor()?.receive_address.is_none());
            assert!(f.wallet.active_accounts().is_empty());
            drop(locked);
            f.wallet.clone().accounts_activate_call(AccountsActivateRequest { account_ids: Some(vec![*f.account.id()]) }).await?;
            f.wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
            let restored = f.wallet.active_accounts().get(f.account.id()).unwrap();
            assert!(restored.receive_address().is_ok());
            f.wallet.select(Some(&restored)).await?;
            f.wallet.change_network_context(&NetworkId::new(NetworkType::Devnet)).await?;
            f.wallet
                .clone()
                .accounts_deactivate_call(AccountsDeactivateRequest { account_ids: targeted.then(|| vec![*f.account.id()]) })
                .await?;
            f.wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
            assert!(f.wallet.active_accounts().is_empty());
            assert!(f.wallet.account().is_err());
        }
        let f = Fixture::new(SignatureScheme::Schnorr).await?;
        let wallet = f.wallet.clone();
        wallet.select(Some(&f.account)).await?;
        wallet.change_network_context(&NetworkId::new(NetworkType::Devnet)).await?;
        drop(f);
        assert!(wallet.set_network_id(&NetworkId::new(NetworkType::Mainnet)).is_err());
        wallet.change_network_context(&NetworkId::new(NetworkType::Mainnet)).await?;
        assert!(wallet.account()?.receive_address().is_ok());
        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn p2pk_signatures_execute_and_reject_substituted_utxos() -> Result<()> {
        use kaspa_consensus_core::{
            subnets::SUBNETWORK_ID_NATIVE,
            tx::{
                SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
                VerifiableTransaction,
            },
        };
        for scheme in [SignatureScheme::Schnorr, SignatureScheme::Ecdsa] {
            let f = Fixture::new(scheme).await?;
            let address = f.account.receive_address()?;
            let entry = UtxoEntry::new(100_000_000, kaspa_txscript::pay_to_address_script(&address), 0, false, None);
            let tx = Transaction::new(
                0,
                vec![TransactionInput::new(TransactionOutpoint::new(kaspa_hashes::Hash::from_bytes([1; 32]), 0), vec![], 0, 1)],
                vec![TransactionOutput::new(90_000_000, kaspa_txscript::pay_to_address_script(&f.account.change_address()?))],
                0,
                SUBNETWORK_ID_NATIVE,
                0,
                vec![],
            );
            let unsigned = SignableTransaction::with_entries(tx, vec![entry]);
            let signer = f.account.clone().create_signer(f.secret.clone(), None).await?;
            let signed = signer.try_sign(unsigned.clone(), std::slice::from_ref(&address))?;
            let verifiable = signed.as_verifiable();
            let cache = kaspa_txscript::caches::Cache::new(0);
            let reused = kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync::new();
            let covenants = kaspa_txscript::covenants::CovenantsContext::from_tx(&verifiable).unwrap();
            let ctx = kaspa_txscript::EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&covenants);
            for (index, (input, entry)) in verifiable.populated_inputs().enumerate() {
                kaspa_txscript::TxScriptEngine::from_transaction_input(&verifiable, input, index, entry, ctx, Default::default())
                    .execute()
                    .unwrap();
            }
            drop(verifiable);
            let mut wrong = unsigned;
            wrong.entries[0].as_mut().unwrap().script_public_key = kaspa_txscript::pay_to_address_script(&f.account.change_address()?);
            assert!(signer.try_sign(wrong, std::slice::from_ref(&address)).is_err());
            assert!(signer.try_sign(signed, &[]).is_err());
        }
        Ok(())
    }
}
