use atuin_common::logs::LogConfig;
use clap::Subcommand;
use eyre::Result;
#[cfg(not(windows))]
use rustix::{fs::Mode, process::umask};

use crate::i18n::fl;

#[cfg(feature = "client")]
mod client;

mod gen_completions;

#[derive(Subcommand)]
#[command(infer_subcommands = true)]
#[allow(clippy::large_enum_variant)]
pub enum AtuinCmd {
    #[cfg(feature = "client")]
    #[command(flatten)]
    Client(client::Cmd),

    #[cfg(feature = "pty-proxy")]
    #[command(alias = "hex", about = fl!("cmd-pty-proxy"))]
    PtyProxy(atuin_pty_proxy::PtyProxy),

    // Plumbing: every shell init calls `atuin uuid` to seed ATUIN_SESSION; not for interactive use.
    #[command(hide = true, about = fl!("cmd-uuid"))]
    Uuid,

    #[command(about = fl!("cmd-gen-completions"))]
    GenCompletions(gen_completions::Cmd),
}

impl AtuinCmd {
    pub fn run(self) -> Result<()> {
        // set umask before we potentially open/create files
        // or in other words, 077. Do not allow any access to any other user.
        // Keep the previous umask so pty-proxy can restore it in the shell it
        // spawns — the shell must not inherit ours (#3695).
        #[cfg(not(windows))]
        let prev_umask = umask(Mode::RWXG | Mode::RWXO);

        let _log_guard = match &self {
            #[cfg(feature = "client")]
            Self::Client(_) => None,
            _ => Some(crate::logs::LogCtx::try_enable(&LogConfig::stderr_only())?),
        };

        match self {
            #[cfg(feature = "client")]
            Self::Client(client) => client.run(),

            #[cfg(all(feature = "pty-proxy", unix))]
            Self::PtyProxy(proxy) => {
                run_pty_proxy(proxy, prev_umask);
                Ok(())
            }

            #[cfg(all(feature = "pty-proxy", not(unix)))]
            Self::PtyProxy(_) => {
                eprintln!("atuin pty-proxy currently supports unix platforms");
                std::process::exit(1);
            }

            Self::Uuid => {
                println!("{}", atuin_common::utils::uuid_v7().as_simple());
                Ok(())
            }
            Self::GenCompletions(gen_completions) => gen_completions.run(),
        }
    }
}

#[cfg(all(feature = "pty-proxy", unix))]
fn run_pty_proxy(proxy: atuin_pty_proxy::PtyProxy, prev_umask: Mode) {
    // `Mode::bits()` returns u16 on macOS/BSD but u32 on Linux, where this
    // conversion is a no-op.
    #[allow(clippy::useless_conversion)]
    let child_umask = Some(u32::from(prev_umask.bits()));

    proxy.run(None, child_umask);
}
