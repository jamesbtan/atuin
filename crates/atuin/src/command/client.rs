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
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    #[command(flatten)]
    Async(AsyncCmd),

    /// Manipulate shell history
    #[command(subcommand)]
    History(history::Cmd),

    #[command(about = fl!("cmd-init"))]
    Init(init::Cmd),

    #[command(about = fl!("cmd-doctor"))]
    Doctor,

    #[command(about = fl!("cmd-default-config"))]
    DefaultConfig,

    /// Get, set, or print values in your atuin config file
    #[command(subcommand)]
    Config(config::Cmd),

    #[command(about = fl!("cmd-info"))]
    Info,
}

#[derive(Subcommand, Debug)]
pub enum AsyncCmd {
    // Variant order sets the `--help` command list order, so keep the commands users reach for
    // most (search, sync, stats) at the top and the plumbing/config commands lower down.
    #[command(about = fl!("cmd-search"))]
    Search(search::Cmd),

    #[command(about = fl!("cmd-stats"))]
    Stats(stats::Cmd),

    /// Import shell history from file
    #[command(subcommand)]
    Import(import::Cmd),

    /// Get or set small key-value pairs
    #[command(subcommand)]
    Kv(kv::Cmd),

    /// Manage the atuin data store
    #[command(subcommand)]
    Store(store::Cmd),

    /// List legacy synced dotfiles data
    #[command(subcommand)]
    Dotfiles(dotfiles::Cmd),
}

impl AsyncCmd {
    fn run(self, mut settings: Settings) -> Result<()> {
        block_on(async {
            let db = Sqlite::new(&settings.db_path, settings.local_timeout).await?;
            let store =
                SqliteStore::new(&settings.record_store_path, settings.local_timeout).await?;
            let mut theme_manager = theme::ThemeManager::new(settings.theme.debug, None);
            let theme =
                theme_manager.load_theme(settings.theme.name.as_str(), settings.theme.max_depth);

            match self {
                Self::Import(import) => import.run(&db).await,
                Self::Search(search) => search.run(db, &mut settings, store, theme).await,
                Self::Stats(stats) => stats.run(&db, &settings, theme).await,
                Self::Store(store_cmd) => store_cmd.run(&settings, &db, store).await,
                Self::Kv(kv) => kv.run(&settings, &store).await,
                Self::Dotfiles(dotfiles) => dotfiles.run(&settings, store).await,
            }
        })
    }
}

pub fn block_on<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    let res = runtime.block_on(future);
    runtime.shutdown_timeout(std::time::Duration::from_millis(50));
    res
}

impl Cmd {
    pub fn run(self) -> Result<()> {
        let settings = Settings::new().wrap_err("could not load client settings")?;
        let _logging = self
            .log_config(&settings)
            .map(|c| LogCtx::try_enable(&c))
            .transpose()
            .wrap_err("failed to enable logging")?;

        match self {
            Self::Async(async_cmd) => async_cmd.run(settings),
            Self::History(history) => history.run(&settings),
            Self::Init(init) => init.run(&settings),
            Self::Doctor => doctor::run(&settings),
            Self::DefaultConfig => {
                default_config::run();
                Ok(())
            }
            Self::Config(config) => config.run(&settings),
            Self::Info => {
                info::run(&settings);
                Ok(())
            }
        }
    }

    fn log_config(&self, settings: &Settings) -> Option<LogConfig> {
        match self {
            Self::History(cmd) => cmd.log_config(),

            Self::Async(AsyncCmd::Search(cmd)) if cmd.is_interactive() => {
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
