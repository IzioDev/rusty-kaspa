use std::sync::Arc;

use kaspa_core::info;
use kaspa_database::{
    prelude::{DbKey, DbWriter, DirectDbWriter, StoreError, DB},
    registry::DatabaseStorePrefixes,
};

use crate::address_store_with_cache::Store;

pub const ADDRESS_SCHEMA_VERSION: u32 = 2;
const SCHEMA_VERSION_KEY: &[u8] = b"addresses-schema-version";

pub struct AddressStoreMigrator {
    db: Arc<DB>,
}

impl AddressStoreMigrator {
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
    }

    pub fn ensure_latest(&self, store: &mut Store) -> Result<(), StoreError> {
        let current_version = self.load_version()?.unwrap_or(1);
        if current_version >= ADDRESS_SCHEMA_VERSION {
            return Ok(());
        }

        let rewritten = store.rewrite_all_entries();
        info!(
            "Address store schema upgraded from v{} to v{} (rewrote {} entries)",
            current_version, ADDRESS_SCHEMA_VERSION, rewritten
        );
        self.persist_version(ADDRESS_SCHEMA_VERSION)
    }

    #[cfg(test)]
    pub fn current_version(&self) -> Result<Option<u32>, StoreError> {
        self.load_version()
    }

    fn load_version(&self) -> Result<Option<u32>, StoreError> {
        let key = DbKey::new(DatabaseStorePrefixes::AddressMetadata.as_ref(), SCHEMA_VERSION_KEY);
        Ok(self.db.get_pinned(key)?.map(|raw| {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(&raw);
            u32::from_le_bytes(arr)
        }))
    }

    fn persist_version(&self, version: u32) -> Result<(), StoreError> {
        let key = DbKey::new(DatabaseStorePrefixes::AddressMetadata.as_ref(), SCHEMA_VERSION_KEY);
        let bytes = version.to_le_bytes();
        DirectDbWriter::new(&self.db).put(key, bytes.to_vec())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_database::create_temp_db;
    use kaspa_database::prelude::ConnBuilder;

    #[test]
    fn migrator_sets_schema_version() {
        let (db_lifetime, db) = create_temp_db!(ConnBuilder::default().with_files_limit(10));
        let mut store = crate::address_store_with_cache::new(db.clone());
        let migrator = AddressStoreMigrator::new(db.clone());
        migrator.ensure_latest(&mut store).unwrap();
        assert_eq!(migrator.current_version().unwrap(), Some(ADDRESS_SCHEMA_VERSION));
        drop(store);
        drop(migrator);
        drop(db);
        drop(db_lifetime);
    }
}
