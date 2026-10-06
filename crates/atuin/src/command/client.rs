use atuin_client::database::Sqlite;
use atuin_client::logs::FromSettings;
use atuin_client::record::sqlite_store::SqliteStore;
use atuin_client::settings::Settings;
use atuin_client::theme;
use atuin_common::logs::LogConfig;
use clap::Subcommand;
use eyre::{Result, WrapErr};

use crate::i18n::fl;
use crate::logs::LogCtx;

mod config;
mod default_config;
mod doctor;
mod dotfiles;
mod history;
mod import;
mod info;
mod init;
mod kv;
mod search;
mod stats;
mod store;

#[derive(Subcommand, Debug)]
#[command(infer_subcommands = true)]
pub enum Cmd {
    // Variant order sets the `--help` command list order, so keep the commands users reach for
    // most (search, sync, stats) at the top and the plumbing/config commands lower down.
    #[command(about = fl!("cmd-search"))]
    Search(search::Cmd),

    #[command(about = fl!("cmd-stats"))]
    Stats(stats::Cmd),

    /// Manipulate shell history
    #[command(subcommand)]
    History(history::Cmd),

    #[command(about = fl!("cmd-init"))]
    Init(init::Cmd),

    /// Import shell history from file
    #[command(subcommand)]
    Import(import::Cmd),

    #[command(about = fl!("cmd-doctor"))]
    Doctor,

    /// Get or set small key-value pairs
    #[command(subcommand)]
    Kv(kv::Cmd),

    /// Manage the atuin data store
    #[command(subcommand)]
    Store(store::Cmd),

    /// List legacy synced dotfiles data
    #[command(subcommand)]
    Dotfiles(dotfiles::Cmd),

    #[command(about = fl!("cmd-default-config"))]
    DefaultConfig,

    /// Get, set, or print values in your atuin config file
    #[command(subcommand)]
    Config(config::Cmd),

    #[command(about = fl!("cmd-info"))]
    Info,
}

impl Cmd {
    pub fn run(self) -> Result<()> {
        let use_multi_thread_runtime = false;

        let Some(future) = self.run_inner()? else {
            // The command was handled synchronously; return.
            return Ok(());
        };

        let runtime = if use_multi_thread_runtime {
            tokio::runtime::Builder::new_multi_thread()
        } else {
            tokio::runtime::Builder::new_current_thread()
        }
        .enable_all()
        .build()
        .unwrap();

        let res = runtime.block_on(future);
        runtime.shutdown_timeout(std::time::Duration::from_millis(50));
        res
    }

    /// Run the command, returning a future for commands that require async.
    ///
    /// If the command was able to be handled synchronously, returns `Ok(None)`. Otherwise, returns
    /// `Ok(Some(future))` where `future` will run the command when awaited. Returns `Err` on error.
    fn run_inner(self) -> Result<Option<impl Future<Output = Result<()>>>> {
        let settings = Settings::new().wrap_err("could not load client settings")?;
        let _logging = self
            .log_config(&settings)
            .map(|c| LogCtx::try_enable(&c))
            .transpose()
            .wrap_err("failed to enable logging")?;

        Ok(Some(async { Box::pin(self.run_async(settings)).await }))
    }

    #[allow(clippy::too_many_lines)]
    // `atuin_ai::commands::run` is not `Send` because `eye_declare` holds a `StdoutLock` across
    // await points.
    #[allow(clippy::future_not_send)]
    async fn run_async(self, mut settings: Settings) -> Result<()> {
        tracing::trace!(command = ?self, "client command");
        let mut theme_manager = theme::ThemeManager::new(settings.theme.debug, None);

        // Skip initializing any databases for history
        // This is a pretty hot path, as it runs before and after every single command the user
        // runs
        match self {
            Self::History(history) => return history.run(&settings).await,
            Self::Init(init) => return init.run(&settings).await,
            Self::Doctor => return doctor::run(&settings).await,
            Self::Config(config) => return config.run(&settings).await,
            _ => {}
        }

        let db_path = &settings.db_path;
        let record_store_path = &settings.record_store_path;

        let db = Sqlite::new(db_path, settings.local_timeout).await?;
        let sqlite_store = SqliteStore::new(record_store_path, settings.local_timeout).await?;

        let theme_name = settings.theme.name.clone();
        let theme = theme_manager.load_theme(theme_name.as_str(), settings.theme.max_depth);

        match self {
            Self::Import(import) => import.run(&db).await,
            Self::Stats(stats) => stats.run(&db, &settings, theme).await,
            Self::Search(search) => search.run(db, &mut settings, sqlite_store, theme).await,

            Self::Kv(kv) => kv.run(&settings, &sqlite_store).await,

            Self::Store(store) => store.run(&settings, &db, sqlite_store).await,

            Self::Dotfiles(dotfiles) => dotfiles.run(&settings, sqlite_store).await,

            Self::Scripts(scripts) => scripts.run(&settings, sqlite_store, &db).await,

            Self::Info => {
                info::run(&settings);
                Ok(())
            }

            Self::DefaultConfig => {
                default_config::run();
                Ok(())
            }

            Self::Wrapped { year } => wrapped::run(year, &db, &settings, sqlite_store, theme).await,

            Self::History(_) | Self::Init(_) | Self::Doctor | Self::Config(_) => {
                unreachable!()
            }
        }
    }

    fn log_config(&self, settings: &Settings) -> Option<LogConfig> {
        match self {
            Self::History(cmd) => cmd.log_config(),

            Self::Search(cmd) if cmd.is_interactive() => {
                Some(LogConfig::from_settings(&settings.logs, &settings.logs.search))
            }

            _ => Some(LogConfig::stderr_only()),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use rstest::rstest;

    use super::Cmd;

    const SECRET: &str = "hunter2-s3cr3t";

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[rstest]
    #[case::rekey(&["store", "rekey", SECRET])]
    fn debug_redacts_argv_secrets(#[case] args: &[&str]) {
        let cli = Cli::try_parse_from(std::iter::once("atuin").chain(args.iter().copied()))
            .expect("the argv parses");
        assert!(!format!("{:?}", cli.cmd).contains(SECRET));
    }
}
