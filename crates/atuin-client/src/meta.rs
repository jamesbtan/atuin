use std::ffi::OsStr;
use std::str::FromStr;
use std::time::Duration;

use atuin_common::db;
use atuin_common::db::sqlite::{Journaling, Sqlite, SqliteBuilder};
use atuin_domain::record::HostId;
use eyre::{Result, eyre};
use tokio::sync::OnceCell;
use uuid::Uuid;

// Filename for the legacy plain-text file that we migrate from.
const LEGACY_HOST_ID_FILENAME: &str = "host_id";

const KEY_HOST_ID: &str = "host_id";
const KEY_FILES_MIGRATED: &str = "files_migrated";

pub struct MetaStore {
    sqlite: Sqlite,
    cached_host_id: OnceCell<HostId>,
}

impl MetaStore {
    pub async fn new(path: impl AsRef<OsStr>, timeout: Duration) -> Result<Self> {
        let path = path.as_ref();
        debug!("opening meta sqlite database at {path:?}");

        let builder = Sqlite::builder(path);
        let ephemeral = builder.is_memory();

        let store = Self::from_builder(builder, timeout).await?;

        if !ephemeral {
            store.migrate_files().await?;
        }

        Ok(store)
    }

    pub async fn in_memory(timeout: Duration) -> Result<Self> {
        Self::from_builder(Sqlite::builder_in_memory(), timeout).await
    }

    async fn from_builder(builder: SqliteBuilder<'_>, timeout: Duration) -> Result<Self> {
        let sqlite = builder
            .timeout(timeout)
            .journal(Some(Journaling::Delete))
            .foreign_keys(false)
            .restrict_permissions()
            .open()
            .await?;

        db::migrate!(sqlite.pool(), "./meta-migrations").await?;

        Ok(Self {
            sqlite,
            cached_host_id: OnceCell::const_new(),
        })
    }

    // Generic key-value operations

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = db::query_as("SELECT value FROM meta WHERE key = ?1")
            .bind(key)
            .fetch_optional(self.sqlite.pool())
            .await?;

        Ok(row.map(|r| r.0))
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        db::query(
            "INSERT INTO meta (key, value, updated_at) VALUES (?1, ?2, strftime('%s', 'now'))
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = strftime('%s', 'now')",
        )
        .bind(key)
        .bind(value)
        .execute(self.sqlite.pool())
        .await?;

        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        db::query("DELETE FROM meta WHERE key = ?1").bind(key).execute(self.sqlite.pool()).await?;

        Ok(())
    }

    // Typed accessors

    pub async fn host_id(&self) -> Result<HostId> {
        self.cached_host_id
            .get_or_try_init(|| async {
                if let Some(id) = self.get(KEY_HOST_ID).await? {
                    let parsed = Uuid::from_str(id.as_str())
                        .map_err(|e| eyre!("failed to parse host ID: {e}"))?;
                    return Ok(HostId(parsed));
                }

                let uuid = atuin_common::utils::uuid_v7();
                self.set(KEY_HOST_ID, uuid.as_simple().to_string().as_ref()).await?;

                Ok(HostId(uuid))
            })
            .await
            .copied()
    }

    // File migration: on first open, migrate old plain-text files into the database.
    // Old files are left in place for safe downgrades.

    async fn migrate_files(&self) -> Result<()> {
        if self.get(KEY_FILES_MIGRATED).await?.is_some() {
            return Ok(());
        }

        let data_dir = crate::settings::Settings::effective_data_dir();

        // host_id — validate as UUID
        let host_id_path = data_dir.join(LEGACY_HOST_ID_FILENAME);
        if host_id_path.exists()
            && let Ok(value) = fs_err::read_to_string(&host_id_path)
        {
            let value = value.trim();
            if !value.is_empty() {
                if Uuid::from_str(value).is_ok() {
                    self.set(KEY_HOST_ID, value).await?;
                } else {
                    warn!("skipping migration of host_id: invalid UUID {value:?}");
                }
            }
        }

        self.set(KEY_FILES_MIGRATED, "true").await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    #[fixture]
    async fn store() -> MetaStore {
        MetaStore::in_memory(Duration::from_secs(2)).await.unwrap()
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_set_delete(#[future(awt)] store: MetaStore) {
        assert_eq!(store.get("foo").await.unwrap(), None);

        store.set("foo", "bar").await.unwrap();
        assert_eq!(store.get("foo").await.unwrap(), Some("bar".to_string()));

        store.set("foo", "baz").await.unwrap();
        assert_eq!(store.get("foo").await.unwrap(), Some("baz".to_string()));

        store.delete("foo").await.unwrap();
        assert_eq!(store.get("foo").await.unwrap(), None);
    }

    #[rstest]
    #[tokio::test]
    async fn test_host_id_generation_and_stability(#[future(awt)] store: MetaStore) {
        let id1 = store.host_id().await.unwrap();
        let id2 = store.host_id().await.unwrap();

        assert_eq!(id1, id2, "host_id should be stable across calls");
    }

    #[rstest]
    #[tokio::test]
    async fn memory_store_skips_file_migration() {
        let store = MetaStore::new(":memory:", Duration::from_secs(2)).await.unwrap();

        assert_eq!(store.get(KEY_FILES_MIGRATED).await.unwrap(), None);
    }
}
