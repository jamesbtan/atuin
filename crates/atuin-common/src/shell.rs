#[cfg(unix)]
use std::path::PathBuf;
use std::process::ExitStatus;

use sysinfo::{Process, System, get_current_pid};
use thiserror::Error;

#[derive(PartialEq, Eq, derive_more::Debug, derive_more::Display)]
pub enum Shell {
    #[display("sh")]
    Sh,
    #[display("bash")]
    Bash,
    #[display("fish")]
    Fish,
    #[display("zsh")]
    Zsh,
    #[display("xonsh")]
    Xonsh,
    #[display("nu")]
    Nu,
    #[display("powershell")]
    Powershell,
    #[display("unknown")]
    Unknown,
}

/// An error encountered while trying to query the user's default shell.
#[derive(Debug, Error)]
pub enum ShellLookupError {
    #[error("missing environment variable ${0}")]
    MissingEnv(&'static str),
    #[error("failed to run {command:?}: {error}")]
    RunCommand {
        command: String,
        error: std::io::Error,
    },
    #[error("command exited with status {status}: {command:?}")]
    CommandFailed {
        command: String,
        status: ExitStatus,
    },
    #[error("failed to parse output from command: {command:?}")]
    ParseOutput {
        command: String,
    },
}

impl Shell {
    #[must_use]
    pub fn current() -> Self {
        let sys = System::new_all();

        let process = sys
            .process(get_current_pid().expect("Failed to get current PID"))
            .expect("Process with current pid does not exist");

        let parent = sys
            .process(process.parent().expect("Atuin running with no parent!"))
            .expect("Process with parent pid does not exist");

        let shell = parent.name().to_string_lossy().trim().to_lowercase();
        let shell = shell.strip_prefix('-').unwrap_or(&shell);

        Self::from_string(shell)
    }

    #[must_use]
    pub fn from_env() -> Self {
        std::env::var("ATUIN_SHELL")
            .map_or(Self::Unknown, |shell| Self::from_string(&shell.trim().to_lowercase()))
    }

    #[must_use]
    pub fn config_file(&self) -> Option<PathBuf> {
        let mut path = dirs::home_dir()?;

        // TODO: handle all shells
        match self {
            Self::Bash => path.push(".bashrc"),
            Self::Zsh => path.push(".zshrc"),
            Self::Fish => path.push(".config/fish/config.fish"),

            _ => return None,
        };

        Some(path)
    }

    #[must_use]
    pub fn from_string(name: &str) -> Self {
        match name {
            "bash" => Self::Bash,
            "fish" => Self::Fish,
            "zsh" => Self::Zsh,
            "xonsh" => Self::Xonsh,
            "nu" => Self::Nu,
            "sh" => Self::Sh,
            "powershell" => Self::Powershell,

            _ => Self::Unknown,
        }
    }

    /// Returns true if the shell is posix-like
    /// Note that while fish is not posix compliant, it behaves well enough for our current
    /// featureset that this does not matter.
    #[must_use]
    pub fn is_posixish(&self) -> bool {
        matches!(self, Self::Bash | Self::Fish | Self::Zsh)
    }
}

#[must_use]
pub fn shell_name(parent: Option<&Process>) -> String {
    let sys = System::new_all();

    let parent = if let Some(parent) = parent {
        parent
    } else {
        let process = sys
            .process(get_current_pid().expect("Failed to get current PID"))
            .expect("Process with current pid does not exist");

        sys.process(process.parent().expect("Atuin running with no parent!"))
            .expect("Process with parent pid does not exist")
    };

    let shell = parent.name().to_string_lossy().trim().to_lowercase();
    let shell = shell.strip_prefix('-').unwrap_or(&shell);

    shell.to_string()
}
