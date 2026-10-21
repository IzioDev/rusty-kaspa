//! AccountMetadata is an associative structure that contains
//! additional information about an account. This structure
//! is not encrypted and is stored in plain text. This is meant
//! to provide an ability to perform various operations (such as
//! new address generation) without the need to re-encrypt the
//! wallet data when storing.

use crate::derivation::AddressDerivationMeta;
use crate::imports::*;
use crate::storage::IdT;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountMetadata {
    pub id: AccountId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexes: Option<AddressDerivationMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kcc3_indexes: Option<std::collections::BTreeMap<NetworkType, AddressDerivationMeta>>,
}

impl AccountMetadata {
    const STORAGE_MAGIC: u32 = 0x4154454d;
    const STORAGE_VERSION: u32 = 0;

    pub fn new(id: AccountId, indexes: AddressDerivationMeta) -> Self {
        Self { id, indexes: Some(indexes), kcc3_indexes: None }
    }

    pub fn address_derivation_indexes(&self) -> Option<AddressDerivationMeta> {
        self.indexes.clone()
    }

    pub fn new_kcc3(id: AccountId) -> Self {
        Self { id, indexes: None, kcc3_indexes: Some(Default::default()) }
    }

    pub fn kcc3_indexes(&self, network: NetworkType) -> AddressDerivationMeta {
        self.kcc3_indexes.as_ref().and_then(|indexes| indexes.get(&network)).cloned().unwrap_or_default()
    }

    pub fn set_kcc3_indexes(&mut self, network: NetworkType, indexes: AddressDerivationMeta) -> Result<()> {
        if indexes.receive() > kaspa_wallet_keys::derivation::gen2::MAX_INDEX
            || indexes.change() > kaspa_wallet_keys::derivation::gen2::MAX_INDEX
        {
            return Err(Error::custom("KCC-3 address counters exceed the non-hardened range"));
        }
        self.kcc3_indexes.get_or_insert_with(Default::default).insert(network, indexes);
        Ok(())
    }
}

impl IdT for AccountMetadata {
    type Id = AccountId;
    fn id(&self) -> &AccountId {
        &self.id
    }
}

impl BorshSerialize for AccountMetadata {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let version = if self.kcc3_indexes.is_some() { 1 } else { Self::STORAGE_VERSION };
        StorageHeader::new(Self::STORAGE_MAGIC, version).serialize(writer)?;
        BorshSerialize::serialize(&self.id, writer)?;
        BorshSerialize::serialize(&self.indexes, writer)?;
        if let Some(indexes) = &self.kcc3_indexes {
            BorshSerialize::serialize(indexes, writer)?;
        }

        Ok(())
    }
}

impl BorshDeserialize for AccountMetadata {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> IoResult<Self> {
        let StorageHeader { version, .. } = StorageHeader::deserialize_reader(reader)?.try_magic(Self::STORAGE_MAGIC)?;
        if version > 1 {
            return Err(std::io::Error::other("Unsupported account metadata version"));
        }

        let id = BorshDeserialize::deserialize_reader(reader)?;
        let indexes = BorshDeserialize::deserialize_reader(reader)?;

        let kcc3_indexes: Option<std::collections::BTreeMap<NetworkType, AddressDerivationMeta>> =
            if version == 1 { Some(BorshDeserialize::deserialize_reader(reader)?) } else { None };
        if kcc3_indexes.as_ref().is_some_and(|entries| {
            entries.values().any(|v| {
                v.receive() > kaspa_wallet_keys::derivation::gen2::MAX_INDEX
                    || v.change() > kaspa_wallet_keys::derivation::gen2::MAX_INDEX
            })
        }) {
            return Err(std::io::Error::other("Invalid KCC-3 address counters"));
        }
        Ok(Self { id, indexes, kcc3_indexes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_bytes_and_network_progress() -> Result<()> {
        let id = AccountId(kaspa_hashes::Hash::from_bytes([3; 32]));
        let legacy = AccountMetadata::new(id, AddressDerivationMeta::new(7, 9));
        let expected = borsh::to_vec(&(AccountMetadata::STORAGE_MAGIC, 0u32, id, Some(AddressDerivationMeta::new(7, 9))))?;
        assert_eq!(borsh::to_vec(&legacy)?, expected);
        let loaded = AccountMetadata::try_from_slice(&expected)?;
        assert_eq!(loaded.address_derivation_indexes().unwrap().receive(), 7);
        assert!(loaded.kcc3_indexes.is_none());
        let mut meta = AccountMetadata::new_kcc3(id);
        meta.set_kcc3_indexes(NetworkType::Mainnet, AddressDerivationMeta::new(12, 13))?;
        meta.set_kcc3_indexes(NetworkType::Testnet, AddressDerivationMeta::new(21, 22))?;
        let loaded = AccountMetadata::try_from_slice(&borsh::to_vec(&meta)?)?;
        assert!(loaded.indexes.is_none());
        assert_eq!(loaded.kcc3_indexes(NetworkType::Mainnet).change(), 13);
        assert_eq!(loaded.kcc3_indexes(NetworkType::Testnet).receive(), 21);
        assert_eq!(loaded.kcc3_indexes(NetworkType::Devnet).receive(), 0);
        assert!(meta.set_kcc3_indexes(NetworkType::Mainnet, AddressDerivationMeta::new(1 << 31, 0)).is_err());
        Ok(())
    }
}
