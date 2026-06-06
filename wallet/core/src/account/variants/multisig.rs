//!
//! MultiSig account implementation.
//!

use crate::account::{Inner, create_private_keys};
use crate::derivation::{AddressDerivationConfig, AddressDerivationManager, AddressDerivationManagerTrait, MultisigDerivationScheme};
use crate::imports::*;

pub const MULTISIG_ACCOUNT_KIND: &str = "kaspa-multisig-standard";

pub struct Ctor {}

#[async_trait]
impl Factory for Ctor {
    fn name(&self) -> String {
        "multisig".to_string()
    }

    fn description(&self) -> String {
        "Kaspa Core Multi-Signature Account".to_string()
    }

    async fn try_load(
        &self,
        wallet: &Arc<Wallet>,
        storage: &AccountStorage,
        meta: Option<Arc<AccountMetadata>>,
    ) -> Result<Arc<dyn Account>> {
        Ok(Arc::new(MultiSig::try_load(wallet, storage, meta).await?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Payload {
    pub xpub_keys: ExtendedPublicKeys,
    pub cosigner_index: Option<u8>,
    pub minimum_signatures: u16,
    pub ecdsa: bool,
    pub derivation_scheme: MultisigDerivationScheme,
}

impl Payload {
    pub fn new(
        xpub_keys: ExtendedPublicKeys,
        derivation_scheme: MultisigDerivationScheme,
        minimum_signatures: u16,
        ecdsa: bool,
    ) -> Self {
        let cosigner_index = derivation_scheme.xpub_child_branch().and_then(|index| u8::try_from(index).ok());
        Self { xpub_keys, cosigner_index, minimum_signatures, ecdsa, derivation_scheme }
    }

    pub fn new_kaspa45(xpub_keys: ExtendedPublicKeys, cosigner_index: Option<u8>, minimum_signatures: u16, ecdsa: bool) -> Self {
        Self::new(xpub_keys, MultisigDerivationScheme::kaspa45(cosigner_index.map(u32::from)), minimum_signatures, ecdsa)
    }

    pub fn new_bip87(xpub_keys: ExtendedPublicKeys, minimum_signatures: u16, ecdsa: bool) -> Self {
        Self::new(xpub_keys, MultisigDerivationScheme::Bip87, minimum_signatures, ecdsa)
    }

    pub fn try_load(storage: &AccountStorage) -> Result<Self> {
        Ok(Self::try_from_slice(storage.serialized.as_slice())?)
    }
}

impl Storable for Payload {
    const STORAGE_MAGIC: u32 = 0x4749534d;
    const STORAGE_VERSION: u32 = 1;
}

impl AccountStorable for Payload {}

impl BorshSerialize for Payload {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        StorageHeader::new(Self::STORAGE_MAGIC, Self::STORAGE_VERSION).serialize(writer)?;

        BorshSerialize::serialize(&self.xpub_keys, writer)?;
        BorshSerialize::serialize(&self.cosigner_index, writer)?;
        BorshSerialize::serialize(&self.minimum_signatures, writer)?;
        BorshSerialize::serialize(&self.ecdsa, writer)?;
        BorshSerialize::serialize(&self.derivation_scheme, writer)?;

        Ok(())
    }
}

impl BorshDeserialize for Payload {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> IoResult<Self> {
        let StorageHeader { version, .. } =
            StorageHeader::deserialize_reader(reader)?.try_magic(Self::STORAGE_MAGIC)?.try_version(Self::STORAGE_VERSION)?;

        let xpub_keys = BorshDeserialize::deserialize_reader(reader)?;
        let cosigner_index: Option<u8> = BorshDeserialize::deserialize_reader(reader)?;
        let minimum_signatures = BorshDeserialize::deserialize_reader(reader)?;
        let ecdsa = BorshDeserialize::deserialize_reader(reader)?;
        let derivation_scheme = if version == 0 {
            MultisigDerivationScheme::kaspa45(cosigner_index.map(u32::from))
        } else {
            BorshDeserialize::deserialize_reader(reader)?
        };

        Ok(Self { xpub_keys, cosigner_index, minimum_signatures, ecdsa, derivation_scheme })
    }
}

pub struct MultiSig {
    inner: Arc<Inner>,
    xpub_keys: ExtendedPublicKeys,
    prv_key_data_ids: Option<Arc<Vec<PrvKeyDataId>>>,
    derivation_scheme: MultisigDerivationScheme,
    minimum_signatures: u16,
    ecdsa: bool,
    derivation: Arc<AddressDerivationManager>,
}

impl MultiSig {
    pub async fn try_new(
        wallet: &Arc<Wallet>,
        name: Option<String>,
        xpub_keys: ExtendedPublicKeys,
        prv_key_data_ids: Option<Arc<Vec<PrvKeyDataId>>>,
        derivation_scheme: MultisigDerivationScheme,
        minimum_signatures: u16,
        ecdsa: bool,
    ) -> Result<Self> {
        if xpub_keys.is_empty() {
            return Err(Error::MultisigXpubRequired);
        }

        let storable = Payload::new(xpub_keys.clone(), derivation_scheme, minimum_signatures, ecdsa);
        let settings = AccountSettings { name, ..Default::default() };
        let (id, storage_key) = make_account_hashes(from_multisig(&prv_key_data_ids, &storable));
        let inner = Arc::new(Inner::new(wallet, id, storage_key, settings));

        let derivation = AddressDerivationManager::new(
            wallet,
            AddressDerivationConfig::multisig(0, derivation_scheme),
            &xpub_keys,
            ecdsa,
            minimum_signatures,
            Default::default(),
        )
        .await?;

        Ok(Self { inner, xpub_keys, derivation_scheme, minimum_signatures, ecdsa, derivation, prv_key_data_ids })
    }

    pub async fn try_load(wallet: &Arc<Wallet>, storage: &AccountStorage, meta: Option<Arc<AccountMetadata>>) -> Result<Self> {
        let storable = Payload::try_load(storage)?;
        let inner = Arc::new(Inner::from_storage(wallet, storage));

        let Payload { xpub_keys, derivation_scheme, minimum_signatures, ecdsa, .. } = storable;
        if xpub_keys.is_empty() {
            return Err(Error::MultisigXpubRequired);
        }

        let address_derivation_indexes = meta.and_then(|meta| meta.address_derivation_indexes()).unwrap_or_default();

        let derivation = AddressDerivationManager::new(
            wallet,
            AddressDerivationConfig::multisig(0, derivation_scheme),
            &xpub_keys,
            ecdsa,
            minimum_signatures,
            address_derivation_indexes,
        )
        .await?;

        // TODO @maxim check variants transforms - None->Ok(None), Multiple->Ok(Some()), Single->Err()
        let prv_key_data_ids = storage.prv_key_data_ids.clone().try_into()?;

        Ok(Self { inner, xpub_keys, derivation_scheme, minimum_signatures, ecdsa, derivation, prv_key_data_ids })
    }

    pub fn prv_key_data_ids(&self) -> &Option<Arc<Vec<PrvKeyDataId>>> {
        &self.prv_key_data_ids
    }

    pub fn minimum_signatures(&self) -> u16 {
        self.minimum_signatures
    }

    pub fn derivation_scheme(&self) -> MultisigDerivationScheme {
        self.derivation_scheme
    }

    pub fn xpub_keys(&self) -> &ExtendedPublicKeys {
        &self.xpub_keys
    }

    fn watch_only(&self) -> bool {
        self.prv_key_data_ids.is_none()
    }
}

#[async_trait]
impl Account for MultiSig {
    fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }

    fn account_kind(&self) -> AccountKind {
        MULTISIG_ACCOUNT_KIND.into()
    }

    fn feature(&self) -> Option<String> {
        match self.watch_only() {
            true => Some("multisig-watch".to_string()),
            false => None,
        }
    }

    fn xpub_keys(&self) -> Option<&ExtendedPublicKeys> {
        Some(&self.xpub_keys)
    }

    fn prv_key_data_id(&self) -> Result<&PrvKeyDataId> {
        Err(Error::AccountKindFeature)
    }

    fn as_dyn_arc(self: Arc<Self>) -> Arc<dyn Account> {
        self
    }

    fn sig_op_count(&self) -> u8 {
        u8::try_from(self.xpub_keys.len()).unwrap()
    }

    fn minimum_signatures(&self) -> u16 {
        self.minimum_signatures
    }

    fn receive_address(&self) -> Result<Address> {
        self.derivation.receive_address_manager().current_address()
    }

    fn change_address(&self) -> Result<Address> {
        self.derivation.change_address_manager().current_address()
    }

    fn to_storage(&self) -> Result<AccountStorage> {
        let settings = self.context().settings.clone();
        let storable = Payload::new(self.xpub_keys.clone(), self.derivation_scheme, self.minimum_signatures, self.ecdsa);
        let account_storage = AccountStorage::try_new(
            MULTISIG_ACCOUNT_KIND.into(),
            self.id(),
            self.storage_key(),
            self.prv_key_data_ids.clone().try_into()?,
            settings,
            storable,
        )?;

        Ok(account_storage)
    }

    fn metadata(&self) -> Result<Option<AccountMetadata>> {
        let metadata = AccountMetadata::new(self.inner.id, self.derivation.address_derivation_meta());
        Ok(Some(metadata))
    }

    fn descriptor(&self) -> Result<AccountDescriptor> {
        let descriptor = AccountDescriptor::new(
            MULTISIG_ACCOUNT_KIND.into(),
            *self.id(),
            self.name(),
            self.balance(),
            self.prv_key_data_ids.clone().try_into()?,
            self.receive_address().ok(),
            self.change_address().ok(),
            None,
        )
        .with_property(AccountDescriptorProperty::XpubKeys, self.xpub_keys.clone().into())
        .with_property(AccountDescriptorProperty::Ecdsa, self.ecdsa.into())
        .with_property(
            AccountDescriptorProperty::Other("Derivation Scheme".to_string()),
            format!("{:?}", self.derivation_scheme).into(),
        )
        .with_property(AccountDescriptorProperty::DerivationMeta, self.derivation.address_derivation_meta().into());

        Ok(descriptor)
    }

    fn as_derivation_capable(self: Arc<Self>) -> Result<Arc<dyn DerivationCapableAccount>> {
        Ok(self.clone())
    }

    fn as_signature_scheme_capable(self: Arc<Self>) -> Result<Arc<dyn SignatureSchemeCapableAccount>> {
        Ok(self.clone())
    }
}

impl SignatureSchemeCapableAccount for MultiSig {
    fn signature_scheme(&self) -> SignatureScheme {
        if self.ecdsa { SignatureScheme::ECDSA } else { SignatureScheme::Schnorr }
    }
}

impl DerivationCapableAccount for MultiSig {
    fn derivation(&self) -> Arc<dyn AddressDerivationManagerTrait> {
        self.derivation.clone()
    }

    fn account_index(&self) -> u64 {
        0
    }

    fn cosigner_index(&self) -> Option<u8> {
        self.derivation_scheme.xpub_child_branch().and_then(|index| u8::try_from(index).ok())
    }

    fn multisig_derivation_scheme(&self) -> Option<MultisigDerivationScheme> {
        Some(self.derivation_scheme)
    }

    fn create_private_keys<'l>(
        &self,
        key_data: &PrvKeyData,
        payment_secret: &Option<Secret>,
        receive: &[(&'l Address, u32)],
        change: &[(&'l Address, u32)],
    ) -> Result<Vec<(&'l Address, secp256k1::SecretKey)>> {
        let payload = key_data.payload.decrypt(payment_secret.as_ref())?;
        let xkey = payload.get_xprv(payment_secret.as_ref())?;

        create_private_keys(self.derivation_config()?, &xkey, receive, change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

    #[test]
    fn test_storage_multisig() -> Result<()> {
        let storable_in = Payload::new_bip87(vec![make_xpub()].into(), 0xc0fe, false);
        let guard = StorageGuard::new(&storable_in);
        let storable_out = guard.validate()?;

        assert_eq!(storable_in.cosigner_index, storable_out.cosigner_index);
        assert_eq!(storable_in.derivation_scheme, storable_out.derivation_scheme);
        assert_eq!(storable_out.derivation_scheme, MultisigDerivationScheme::Bip87);
        assert_eq!(storable_in.minimum_signatures, storable_out.minimum_signatures);
        assert_eq!(storable_in.ecdsa, storable_out.ecdsa);
        assert_eq!(storable_in.xpub_keys.len(), storable_out.xpub_keys.len());
        for idx in 0..storable_in.xpub_keys.len() {
            assert_eq!(storable_in.xpub_keys[idx], storable_out.xpub_keys[idx]);
        }

        Ok(())
    }

    #[test]
    fn test_storage_multisig_legacy_scheme() -> Result<()> {
        let storable_in = Payload::new_kaspa45(vec![make_xpub()].into(), Some(42), 0xc0fe, false);
        let guard = StorageGuard::new(&storable_in);
        let storable_out = guard.validate()?;

        assert_eq!(storable_out.cosigner_index, Some(42));
        assert_eq!(storable_out.derivation_scheme, MultisigDerivationScheme::kaspa45(Some(42)));

        Ok(())
    }
}
