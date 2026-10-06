use atuin_client::database::Sqlite;
use atuin_client::history::store::HistoryStore;
use atuin_client::record::sqlite_store::SqliteStore;
use atuin_client::settings::Settings;
use atuin_common::encryption::paseto_v4;
use clap::Args;
use eyre::{Context as _, Result, bail};

#[derive(Args, Debug)]
pub struct Rebuild {
    pub tag: String,
}

impl Rebuild {
    pub async fn run(
        &self,
        settings: &Settings,
        store: SqliteStore,
        database: &Sqlite,
    ) -> Result<()> {
        // keep it as a string and not an enum atm
        // would be super cool to build this dynamically in the future
        // eg register handles for rebuilding various tags without having to make this part of the
        // binary big
        match self.tag.as_str() {
            "history" => {
                self.rebuild_history(settings, store.clone(), database).await?;
            }

            tag => {
                bail!("unknown tag: {tag}");
            }
        }

        Ok(())
    }

    /// The daemon owns the rebuild when enabled; otherwise we rebuild locally.
    async fn rebuild_history(
        &self,
        settings: &Settings,
        store: SqliteStore,
        database: &Sqlite,
    ) -> Result<()> {
        let encryption_key = paseto_v4::Key::try_load_from_path(&settings.key_path)
            .context("could not load encryption key")?;

        let host_id = Settings::host_id().await?;
        let history_store = HistoryStore::new(store, host_id, encryption_key);

        history_store.build(database).await?;

        Ok(())
    }
}
