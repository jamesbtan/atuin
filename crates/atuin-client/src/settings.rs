use std::collections::HashMap;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use atuin_common::logs::LogLevel;
use atuin_common::path::PathExt;
// `AsDuration` is deprecated.
#[allow(deprecated)]
use atuin_common::time::{AsDuration, Days, Seconds};
use atuin_domain::record::HostId;
use clap::ValueEnum;
use config::builder::DefaultState;
use config::{Config, ConfigBuilder, Environment, File as ConfigFile, FileFormat};
use eyre::{Context, Result, eyre};
use fs_err::{File, create_dir_all};
use regex::RegexSet;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use thiserror::Error;
use tokio::sync::OnceCell;

static EXAMPLE_CONFIG: &str = include_str!("../config.toml");

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
static META_CONFIG: OnceLock<(String, f64)> = OnceLock::new();
static META_STORE: OnceCell<crate::meta::MetaStore> = OnceCell::const_new();

mod kv;
pub(crate) mod meta;
mod scripts;
pub mod shells;

pub use shells::Shells;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Prefix,
    FullText,
    Fuzzy,
    DaemonFuzzy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RequestedSearchMode {
    Prefix,
    #[clap(alias("full-text"))]
    Fulltext,
    Fuzzy,
    DaemonFuzzy,
    /// Removed: falls back to [`Self::Fuzzy`] but shows a warning.
    #[clap(hide = true)]
    Skim,
}

impl RequestedSearchMode {
    /// Return the actual search mode that will be used.
    ///
    /// Not all requested modes are supported; this method returns the closest supported mode for
    /// the given requested mode.
    #[must_use]
    pub fn effective_mode(self) -> SearchMode {
        match self {
            Self::Prefix => SearchMode::Prefix,
            Self::Fulltext => SearchMode::FullText,
            Self::Fuzzy | Self::Skim => SearchMode::Fuzzy,
            Self::DaemonFuzzy => SearchMode::DaemonFuzzy,
        }
    }
}

impl From<RequestedSearchMode> for SearchMode {
    fn from(requested: RequestedSearchMode) -> Self {
        requested.effective_mode()
    }
}

impl SearchMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prefix => "PREFIX",
            Self::FullText => "FULLTXT",
            Self::Fuzzy => "FUZZY",
            Self::DaemonFuzzy => "DAEMON",
        }
    }

    #[must_use]
    pub fn next(self, settings: &Settings) -> Self {
        match self {
            Self::Prefix => Self::FullText,
            // if the user is using daemon-fuzzy, we go to daemon-fuzzy
            Self::FullText if settings.active_search_mode() == Self::DaemonFuzzy => {
                Self::DaemonFuzzy
            }
            // otherwise fuzzy.
            Self::FullText => Self::Fuzzy,
            Self::Fuzzy | Self::DaemonFuzzy => Self::Prefix,
        }
    }
}

#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq, ValueEnum, Serialize)]
pub enum FilterMode {
    #[serde(rename = "global")]
    Global = 0,

    #[serde(rename = "host")]
    Host = 1,

    #[serde(rename = "session")]
    Session = 2,

    #[serde(rename = "directory")]
    Directory = 3,

    #[serde(rename = "workspace")]
    Workspace = 4,

    #[serde(rename = "session-preload")]
    SessionPreload = 5,
}

impl FilterMode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "GLOBAL",
            Self::Host => "HOST",
            Self::Session => "SESSION",
            Self::Directory => "DIRECTORY",
            Self::Workspace => "WORKSPACE",
            Self::SessionPreload => "SESSION+",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Copy, Serialize)]
pub enum ExitMode {
    #[serde(rename = "return-original")]
    ReturnOriginal,

    #[serde(rename = "return-query")]
    ReturnQuery,
}

// FIXME: Can use upstream Dialect enum if https://github.com/stevedonovan/chrono-english/pull/16 is merged
// FIXME: Above PR was merged, but dependency was changed to interim (fork of chrono-english) in the ... interim
#[derive(Clone, Debug, Default, Deserialize, Copy, Serialize)]
pub enum Dialect {
    #[default]
    #[serde(rename = "us")]
    Us,

    #[serde(rename = "uk")]
    Uk,
}

impl From<Dialect> for interim::Dialect {
    fn from(d: Dialect) -> Self {
        match d {
            Dialect::Uk => Self::Uk,
            Dialect::Us => Self::Us,
        }
    }
}

use atuin_common::time::UtcOffsetSpec;

#[derive(Clone, Debug, Deserialize, Copy, Serialize)]
pub enum Style {
    #[serde(rename = "auto")]
    Auto,

    #[serde(rename = "full")]
    Full,

    #[serde(rename = "compact")]
    Compact,
}

#[derive(Clone, Debug, Deserialize, Copy, Serialize)]
pub enum WordJumpMode {
    #[serde(rename = "emacs")]
    Emacs,

    #[serde(rename = "subl")]
    Subl,
}

#[derive(Clone, Debug, Deserialize, Copy, PartialEq, Eq, ValueEnum, Serialize)]
pub enum KeymapMode {
    #[serde(rename = "emacs")]
    Emacs,

    #[serde(rename = "vim-normal")]
    VimNormal,

    #[serde(rename = "vim-insert")]
    VimInsert,

    #[serde(rename = "auto")]
    Auto,
}

impl KeymapMode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Emacs => "EMACS",
            Self::VimNormal => "VIMNORMAL",
            Self::VimInsert => "VIMINSERT",
            Self::Auto => "AUTO",
        }
    }
}

// We want to translate the config to crossterm::cursor::SetCursorStyle, but
// the original type does not implement trait serde::Deserialize unfortunately.
// It seems impossible to implement Deserialize for external types when it is
// used in HashMap (https://stackoverflow.com/questions/67142663).  We instead
// define an adapter type.
#[derive(Clone, Debug, Deserialize, Copy, PartialEq, Eq, ValueEnum, Serialize)]
pub enum CursorStyle {
    #[serde(rename = "default")]
    DefaultUserShape,

    #[serde(rename = "blink-block")]
    BlinkingBlock,

    #[serde(rename = "steady-block")]
    SteadyBlock,

    #[serde(rename = "blink-underline")]
    BlinkingUnderScore,

    #[serde(rename = "steady-underline")]
    SteadyUnderScore,

    #[serde(rename = "blink-bar")]
    BlinkingBar,

    #[serde(rename = "steady-bar")]
    SteadyBar,
}

impl CursorStyle {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DefaultUserShape => "DEFAULT",
            Self::BlinkingBlock => "BLINKBLOCK",
            Self::SteadyBlock => "STEADYBLOCK",
            Self::BlinkingUnderScore => "BLINKUNDERLINE",
            Self::SteadyUnderScore => "STEADYUNDERLINE",
            Self::BlinkingBar => "BLINKBAR",
            Self::SteadyBar => "STEADYBAR",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Stats {
    #[serde(default = "Stats::common_prefix_default")]
    pub common_prefix: Vec<String>, // sudo, etc. commands we want to strip off
    #[serde(default = "Stats::common_subcommands_default")]
    pub common_subcommands: Vec<String>, // kubectl, commands we should consider subcommands for
    #[serde(default = "Stats::ignored_commands_default")]
    pub ignored_commands: Vec<String>, // cd, ls, etc. commands we want to completely hide from stats
}

impl Stats {
    fn common_prefix_default() -> Vec<String> {
        vec!["sudo", "doas"].into_iter().map(String::from).collect()
    }

    fn common_subcommands_default() -> Vec<String> {
        vec![
            "apt",
            "cargo",
            "composer",
            "dnf",
            "docker",
            "dotnet",
            "git",
            "go",
            "ip",
            "jj",
            "kubectl",
            "nix",
            "nmcli",
            "npm",
            "pecl",
            "pnpm",
            "podman",
            "port",
            "systemctl",
            "tmux",
            "yarn",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn ignored_commands_default() -> Vec<String> {
        vec![]
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            common_prefix: Self::common_prefix_default(),
            common_subcommands: Self::common_subcommands_default(),
            ignored_commands: Self::ignored_commands_default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Default, Serialize)]
pub struct Keys {
    pub scroll_exits: bool,
    pub exit_past_line_start: bool,
    pub accept_past_line_end: bool,
    pub accept_past_line_start: bool,
    pub accept_with_backspace: bool,
    pub prefix: String,
}

impl Keys {
    /// The standard default values for all `[keys]` options.
    /// These match the config defaults set in `builder_with_data_dir()`.
    #[must_use]
    pub fn standard_defaults() -> Self {
        Self {
            scroll_exits: true,
            exit_past_line_start: true,
            accept_past_line_end: true,
            accept_past_line_start: false,
            accept_with_backspace: false,
            prefix: "a".to_string(),
        }
    }

    /// Returns true if any value differs from the standard defaults.
    #[must_use]
    pub fn has_non_default_values(&self) -> bool {
        let d = Self::standard_defaults();
        self.scroll_exits != d.scroll_exits
            || self.exit_past_line_start != d.exit_past_line_start
            || self.accept_past_line_end != d.accept_past_line_end
            || self.accept_past_line_start != d.accept_past_line_start
            || self.accept_with_backspace != d.accept_with_backspace
            || self.prefix != d.prefix
    }
}

/// A single rule within a conditional keybinding config.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KeyRuleConfig {
    /// Optional condition expression (e.g. "cursor-at-start", "input-empty && no-results").
    /// If absent, the rule always matches.
    #[serde(default)]
    pub when: Option<String>,
    /// The action to perform (e.g. "exit", "cursor-left", "accept").
    pub action: String,
}

/// A keybinding config value: either a simple action string or an ordered list of conditional rules.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum KeyBindingConfig {
    /// Simple unconditional binding: `"ctrl-c" = "return-original"`
    Simple(String),
    /// Conditional binding: `"left" = [{ when = "cursor-at-start", action = "exit" }, { action = "cursor-left" }]`
    Rules(Vec<KeyRuleConfig>),
}

/// User-facing keymap configuration. Each mode maps key strings to bindings.
/// Keys present here override the defaults for that key; unmentioned keys keep defaults.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct KeymapConfig {
    #[serde(default)]
    pub emacs: HashMap<String, KeyBindingConfig>,
    #[serde(default, rename = "vim-normal")]
    pub vim_normal: HashMap<String, KeyBindingConfig>,
    #[serde(default, rename = "vim-insert")]
    pub vim_insert: HashMap<String, KeyBindingConfig>,
    #[serde(default)]
    pub inspector: HashMap<String, KeyBindingConfig>,
    #[serde(default)]
    pub prefix: HashMap<String, KeyBindingConfig>,
}

impl KeymapConfig {
    /// Returns true if no keybinding overrides are configured in any mode.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.emacs.is_empty()
            && self.vim_normal.is_empty()
            && self.vim_insert.is_empty()
            && self.inspector.is_empty()
            && self.prefix.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Preview {
    pub strategy: PreviewStrategy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Theme {
    /// Name of desired theme ("default" for base)
    pub name: String,

    /// Whether any available additional theme debug should be shown
    pub debug: Option<bool>,

    /// How many levels of parenthood will be traversed if needed
    pub max_depth: Option<u8>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PtyProxy {
    /// If enabled, `atuin init` emits shell code that re-execs the shell
    /// inside `atuin pty-proxy`, so no separate `atuin pty-proxy init` line
    /// is needed in shell config. Supported for bash, zsh, fish and nu on
    /// unix platforms.
    #[serde(alias = "enable")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Search {
    /// The list of enabled filter modes, in order of priority.
    pub filters: Vec<FilterMode>,

    /// The recency score multiplier for the search index (default: 1.0).
    /// Values < 1.0 reduce weight, > 1.0 increase weight, 0.0 disables.
    pub recency_score_multiplier: f64,

    /// The frequency score multiplier for the search index (default: 1.0).
    /// Values < 1.0 reduce weight, > 1.0 increase weight, 0.0 disables.
    pub frequency_score_multiplier: f64,

    /// The overall frecency score multiplier for the search index (default: 1.0).
    /// Applied after combining recency and frequency scores.
    pub frecency_score_multiplier: f64,

    /// Controls which shells' commands are shown in interactive search.
    ///
    /// One of: `"all"`, `"auto"`, or an array of strings.
    pub shells: Shells,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tmux {
    /// Enable using atuin with tmux popup (tmux >= 3.2)
    pub enabled: bool,

    /// Width of the tmux popup (percentage)
    pub width: String,

    /// Height of the tmux popup (percentage)
    pub height: String,
}

/// Configuration for a specific log type (e.g. search).
#[allow(deprecated)]
#[serde_as]
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LogConfig {
    /// Log file name (relative to dir) or absolute path.
    pub file: String,

    /// Override global enabled setting for this log type.
    pub enabled: Option<bool>,

    /// Override global level setting for this log type.
    pub level: Option<LogLevel>,

    /// Override the global retention for this log type (a bare number is days). `0` keeps nothing.
    #[serde_as(as = "Option<AsDuration<Days>>")]
    pub retention: Option<Duration>,
}

impl LogConfig {
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            ..Default::default()
        }
    }
}

#[allow(deprecated)]
#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Logs {
    /// Enable file logging globally. Defaults to true.
    #[serde(default = "Logs::default_enabled")]
    pub enabled: bool,

    /// Directory for log files. Defaults to ~/.atuin/logs
    pub dir: String,

    /// Default log level for file logging. Defaults to "info".
    /// Note: ATUIN_LOG environment variable overrides this.
    #[serde(default)]
    pub level: LogLevel,

    /// Default retention for log files (a bare number is days). Defaults to 4 days. `0` keeps
    /// nothing (prunes every rotated log).
    #[serde(default = "Logs::default_retention")]
    #[serde_as(as = "AsDuration<Days>")]
    pub retention: Duration,

    /// Search log settings; only used with `--interactive`
    #[serde(default)]
    pub search: LogConfig,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            strategy: PreviewStrategy::Auto,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "".to_string(),
            debug: None::<bool>,
            max_depth: Some(10),
        }
    }
}

impl Default for Logs {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: "".to_string(),
            level: LogLevel::default(),
            retention: Self::default_retention(),
            search: LogConfig::new("search.log"),
        }
    }
}

impl Logs {
    fn default_enabled() -> bool {
        true
    }

    fn default_retention() -> Duration {
        Duration::from_secs(4 * 86_400)
    }
}

impl Default for Search {
    fn default() -> Self {
        Self {
            filters: vec![
                FilterMode::Global,
                FilterMode::Host,
                FilterMode::Session,
                FilterMode::SessionPreload,
                FilterMode::Workspace,
                FilterMode::Directory,
            ],

            recency_score_multiplier: 1.0,
            frequency_score_multiplier: 1.0,
            frecency_score_multiplier: 1.0,
            shells: Default::default(),
        }
    }
}

impl Default for Tmux {
    fn default() -> Self {
        Self {
            enabled: false,
            width: "80%".to_string(),
            height: "60%".to_string(),
        }
    }
}

// The preview height strategy also takes max_preview_height into account.
#[derive(Clone, Debug, Deserialize, Copy, PartialEq, Eq, ValueEnum, Serialize)]
pub enum PreviewStrategy {
    // Preview height is calculated for the length of the selected command.
    #[serde(rename = "auto")]
    Auto,

    // Preview height is calculated for the length of the longest command stored in the history.
    #[serde(rename = "static")]
    Static,

    // max_preview_height is used as fixed height.
    #[serde(rename = "fixed")]
    Fixed,
}

/// Column types available for the interactive search UI.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiColumnType {
    /// Command execution duration (e.g., "123ms")
    Duration,
    /// Relative time since execution (e.g., "59s ago")
    Time,
    /// Absolute timestamp (e.g., "2025-01-22 14:35")
    Datetime,
    /// Working directory
    Directory,
    /// Hostname
    Host,
    /// Username
    User,
    /// Exit code
    Exit,
    /// The command itself (should be last, expands to fill)
    Command,
}

impl UiColumnType {
    /// Returns the default width for this column type (in characters).
    /// The Command column returns 0 as it expands to fill remaining space.
    #[must_use]
    pub fn default_width(&self) -> u16 {
        match self {
            Self::Duration => 5,  // "814ms"
            Self::Time => 9,      // "459ms ago"
            Self::Datetime => 16, // "2025-01-22 14:35"
            Self::Directory => 20,
            Self::Host => 15,
            Self::User => 10,
            Self::Exit => {
                if cfg!(windows) {
                    11 // 32-bit integer on Windows: "-1978335212"
                } else {
                    3 // Usually a byte on Unix
                }
            }
            Self::Command => 0, // Expands to fill
        }
    }
}

/// A column configuration with type and optional custom width.
/// Can be specified as just a string (uses default width) or as an object with type and width.
#[derive(Clone, Debug, Serialize)]
pub struct UiColumn {
    pub column_type: UiColumnType,
    pub width: u16,
    /// If true, this column expands to fill remaining space. Only one column should expand.
    pub expand: bool,
}

impl UiColumn {
    #[must_use]
    pub fn new(column_type: UiColumnType) -> Self {
        Self {
            width: column_type.default_width(),
            expand: column_type == UiColumnType::Command,
            column_type,
        }
    }

    #[must_use]
    pub fn with_width(column_type: UiColumnType, width: u16) -> Self {
        Self {
            column_type,
            width,
            expand: column_type == UiColumnType::Command,
        }
    }
}

// Custom deserialize to handle both string and object formats:
// "duration" or { type = "duration", width = 8, expand = true }
impl<'de> serde::Deserialize<'de> for UiColumn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};

        struct UiColumnVisitor;

        impl<'de> Visitor<'de> for UiColumnVisitor {
            type Value = UiColumn;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a column type string or an object with 'type' and optional 'width'/'expand'",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<UiColumn, E>
            where
                E: de::Error,
            {
                let column_type: UiColumnType =
                    serde::Deserialize::deserialize(serde::de::value::StrDeserializer::new(value))?;
                Ok(UiColumn::new(column_type))
            }

            fn visit_map<M>(self, mut map: M) -> Result<UiColumn, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut column_type: Option<UiColumnType> = None;
                let mut width: Option<u16> = None;
                let mut expand: Option<bool> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            column_type = Some(map.next_value()?);
                        }
                        "width" => {
                            width = Some(map.next_value()?);
                        }
                        "expand" => {
                            expand = Some(map.next_value()?);
                        }
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let column_type = column_type.ok_or_else(|| de::Error::missing_field("type"))?;
                let width = width.unwrap_or_else(|| column_type.default_width());
                let expand = expand.unwrap_or(column_type == UiColumnType::Command);
                Ok(UiColumn {
                    column_type,
                    width,
                    expand,
                })
            }
        }

        deserializer.deserialize_any(UiColumnVisitor)
    }
}

/// UI-specific settings for the interactive search.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Ui {
    /// Columns to display in interactive search, from left to right.
    /// The indicator column (" > ") is always shown first implicitly.
    /// The "command" column should be last as it expands to fill remaining space.
    /// Can be simple strings or objects with type and width.
    #[serde(default = "Ui::default_columns")]
    pub columns: Vec<UiColumn>,

    /// Syntax highlight commands in the interactive search results.
    #[serde(default = "Ui::default_syntax_highlight")]
    pub syntax_highlight: bool,
}

impl Ui {
    fn default_columns() -> Vec<UiColumn> {
        vec![
            UiColumn::new(UiColumnType::Duration),
            UiColumn::new(UiColumnType::Time),
            UiColumn::new(UiColumnType::Command),
        ]
    }

    fn default_syntax_highlight() -> bool {
        true
    }

    /// Validate the UI configuration.
    /// Returns an error if more than one column has expand = true.
    pub fn validate(&self) -> Result<(), UiValidationError> {
        let expand_count = self.columns.iter().filter(|c| c.expand).count();
        if expand_count > 1 {
            return Err(UiValidationError::MultipleExpandingColumns(expand_count));
        }
        Ok(())
    }
}

/// A [`Ui`] configuration that cannot be used as written.
#[derive(Debug, Error)]
pub enum UiValidationError {
    #[error("Only one column can have expand = true, but {0} columns are set to expand")]
    MultipleExpandingColumns(usize),
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            columns: Self::default_columns(),
            syntax_highlight: Self::default_syntax_highlight(),
        }
    }
}

#[allow(deprecated)]
#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    pub data_dir: Option<String>,
    pub dialect: Dialect,
    pub timezone: UtcOffsetSpec,
    pub style: Style,
    pub db_path: PathBuf,
    pub record_store_path: PathBuf,
    pub key_path: PathBuf,
    #[serde(rename = "search_mode")]
    pub requested_search_mode: RequestedSearchMode,
    pub filter_mode: Option<FilterMode>,
    pub filter_mode_shell_up_key_binding: Option<FilterMode>,
    #[serde(rename = "search_mode_shell_up_key_binding")]
    pub requested_search_mode_shell_up_key_binding: Option<RequestedSearchMode>,

    /// This is not a real setting. Instead, `atuin search` manually sets this field to true if
    /// the hidden `--shell-up-key-binding` option was passed.
    pub shell_up_key_binding: bool,
    pub inline_height: u16,
    pub inline_height_shell_up_key_binding: Option<u16>,
    pub invert: bool,
    pub show_preview: bool,
    pub max_preview_height: u16,
    pub show_help: bool,
    pub show_tabs: bool,
    pub show_numeric_shortcuts: bool,
    pub auto_hide_height: u16,
    pub exit_mode: ExitMode,
    pub keymap_mode: KeymapMode,
    pub keymap_mode_shell: KeymapMode,
    pub keymap_cursor: HashMap<String, CursorStyle>,
    pub word_jump_mode: WordJumpMode,
    pub word_chars: String,
    pub scroll_context_lines: usize,
    pub history_format: String,
    pub strip_trailing_whitespace: bool,
    pub prefers_reduced_motion: bool,
    pub store_failed: bool,
    pub no_mouse: bool,

    #[serde(with = "serde_regex", default = "RegexSet::empty", skip_serializing)]
    pub history_filter: RegexSet,

    #[serde(with = "serde_regex", default = "RegexSet::empty", skip_serializing)]
    pub cwd_filter: RegexSet,

    pub secrets_filter: bool,
    pub workspaces: bool,
    pub ctrl_n_shortcuts: bool,

    #[serde_as(as = "AsDuration<Seconds>")]
    pub local_timeout: Duration,

    pub enter_accept: bool,
    pub smart_sort: bool,
    pub command_chaining: bool,

    #[serde(default)]
    pub stats: Stats,

    #[serde(default)]
    pub keys: Keys,

    #[serde(default)]
    pub keymap: KeymapConfig,

    #[serde(default)]
    pub preview: Preview,

    #[serde(default)]
    pub pty_proxy: PtyProxy,

    #[serde(default)]
    pub search: Search,

    #[serde(default)]
    pub theme: Theme,

    #[serde(default)]
    pub ui: Ui,

    #[serde(default)]
    pub scripts: scripts::Settings,

    #[serde(default)]
    pub kv: kv::Settings,

    #[serde(default)]
    pub tmux: Tmux,

    #[serde(default)]
    pub logs: Logs,

    #[serde(default)]
    pub meta: meta::Settings,
}

impl Settings {
    #[must_use]
    pub fn utc() -> Self {
        Self::builder()
            .expect("Could not build default")
            .set_override("timezone", "0")
            .expect("failed to override timezone with UTC")
            .build()
            .expect("Could not build config")
            .try_deserialize()
            .expect("Could not deserialize config")
    }

    #[must_use]
    pub fn search_mode(&self) -> SearchMode {
        self.requested_search_mode.into()
    }

    pub fn search_mode_shell_up_key_binding(&self) -> Option<SearchMode> {
        self.requested_search_mode_shell_up_key_binding.map(Into::into)
    }

    /// Return the active search mode depending on whether Atuin was invoked from the "up"
    /// keybinding.
    ///
    /// If Atuin was invoked from the "up" keybinding, this returns
    /// [`Self::search_mode_shell_up_key_binding`], falling back to [`Self::search_mode`] if that
    /// binding isn't defined. Otherwise, [`Self::search_mode`] is returned.
    #[must_use]
    pub fn active_search_mode(&self) -> SearchMode {
        self.shell_up_key_binding
            .then(|| self.search_mode_shell_up_key_binding())
            .flatten()
            .unwrap_or_else(|| self.search_mode())
    }

    /// The resolved data directory: a custom `data_dir` / `ATUIN_DATA_DIR` if one was configured
    /// when settings were last built this process, otherwise [`atuin_common::utils::data_dir`].
    #[must_use]
    pub fn effective_data_dir() -> PathBuf {
        DATA_DIR.get().cloned().unwrap_or_else(atuin_common::utils::data_dir)
    }

    // -- Meta store: lazily initialized on first access --

    pub async fn meta_store() -> Result<&'static crate::meta::MetaStore> {
        META_STORE
            .get_or_try_init(|| async {
                let (db_path, timeout) = META_CONFIG.get().ok_or_else(|| {
                    eyre!("meta store config not set — Settings::new() has not been called")
                })?;
                crate::meta::MetaStore::new(
                    db_path,
                    std::time::Duration::try_from_secs_f64(*timeout)?,
                )
                .await
            })
            .await
    }

    pub async fn host_id() -> Result<HostId> {
        Self::meta_store().await?.host_id().await
    }

    #[must_use]
    pub fn default_filter_mode(&self, git_root: bool) -> FilterMode {
        self.filter_mode
            .filter(|x| self.search.filters.contains(x))
            .or_else(|| {
                self.search
                    .filters
                    .iter()
                    .find(|x| match (x, git_root, self.workspaces) {
                        (FilterMode::Workspace, true, true) => true,
                        (FilterMode::Workspace, _, _) => false,
                        (_, _, _) => true,
                    })
                    .copied()
            })
            .unwrap_or(FilterMode::Global)
    }

    pub fn builder() -> Result<ConfigBuilder<DefaultState>> {
        Ok(Self::builder_with_data_dir(&atuin_common::utils::data_dir())?)
    }

    fn builder_with_data_dir(
        data_dir: &std::path::Path,
    ) -> Result<ConfigBuilder<DefaultState>, config::ConfigError> {
        let db_path = data_dir.join("history.db");
        let record_store_path = data_dir.join("records.db");
        let kv_path = data_dir.join("kv.db");
        let scripts_path = data_dir.join("scripts.db");
        let logs_dir = atuin_common::utils::logs_dir();

        let key_path = data_dir.join("key");
        let meta_path = data_dir.join("meta.db");

        Ok(Config::builder()
            .set_default("history_format", "{time}\t{command}\t{duration}")?
            .set_default("db_path", db_path.to_str())?
            .set_default("record_store_path", record_store_path.to_str())?
            .set_default("key_path", key_path.to_str())?
            .set_default("dialect", "us")?
            .set_default("timezone", "local")?
            .set_default("search_mode", "fuzzy")?
            .set_default("filter_mode", None::<String>)?
            .set_default("style", "compact")?
            .set_default("inline_height", 40)?
            .set_default("show_preview", true)?
            .set_default("preview.strategy", "auto")?
            .set_default("max_preview_height", 4)?
            .set_default("show_help", true)?
            .set_default("show_tabs", true)?
            .set_default("show_numeric_shortcuts", true)?
            .set_default("auto_hide_height", 8)?
            .set_default("invert", false)?
            .set_default("exit_mode", "return-original")?
            .set_default("word_jump_mode", "emacs")?
            .set_default(
                "word_chars",
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            )?
            .set_default("scroll_context_lines", 1)?
            .set_default("shell_up_key_binding", false)?
            .set_default("workspaces", true)?
            .set_default("ctrl_n_shortcuts", false)?
            .set_default("secrets_filter", true)?
            .set_default("strip_trailing_whitespace", true)?
            .set_default("local_timeout", 2.0)?
            // enter_accept defaults to false here, but true in the default config file. The dissonance is
            // intentional!
            // Existing users will get the default "False", so we don't mess with any potential
            // muscle memory.
            // New users will get the new default, that is more similar to what they are used to.
            .set_default("enter_accept", false)?
            .set_default("keys.scroll_exits", true)?
            .set_default("keys.accept_past_line_end", true)?
            .set_default("keys.exit_past_line_start", true)?
            .set_default("keys.accept_past_line_start", false)?
            .set_default("keys.accept_with_backspace", false)?
            .set_default("keys.prefix", "a")?
            .set_default("keymap_mode", "emacs")?
            .set_default("keymap_mode_shell", "auto")?
            .set_default("keymap_cursor", HashMap::<String, String>::new())?
            .set_default("smart_sort", false)?
            .set_default("command_chaining", false)?
            .set_default("store_failed", true)?
            .set_default("logs.enabled", true)?
            .set_default("logs.dir", logs_dir.to_str())?
            .set_default("logs.level", "info")?
            .set_default("logs.search.file", "search.log")?
            .set_default("kv.db_path", kv_path.to_str())?
            .set_default("scripts.db_path", scripts_path.to_str())?
            .set_default("search.recency_score_multiplier", 1.0)?
            .set_default("search.frequency_score_multiplier", 1.0)?
            .set_default("search.frecency_score_multiplier", 1.0)?
            .set_default("search.shells", "auto")?
            .set_default("meta.db_path", meta_path.to_str())?
            .set_default("ui.syntax_highlight", true)?
            .set_default("search.filters", vec![
                "global",
                "host",
                "session",
                "workspace",
                "directory",
                "session-preload",
            ])?
            .set_default("theme.name", "default")?
            .set_default("theme.debug", None::<bool>)?
            .set_default("tmux.enabled", false)?
            .set_default("tmux.width", "80%")?
            .set_default("tmux.height", "60%")?
            .set_default(
                "prefers_reduced_motion",
                std::env::var("NO_MOTION")
                    .ok()
                    .map(|_| config::Value::new(None, config::ValueKind::Boolean(true)))
                    .unwrap_or_else(|| config::Value::new(None, config::ValueKind::Boolean(false))),
            )?
            .set_default("no_mouse", false)?
            .set_default("pty_proxy.enabled", false)?
            .add_source(Environment::with_prefix("atuin").prefix_separator("_").separator("__")))
    }

    pub fn get_config_path() -> Result<PathBuf> {
        let config_dir = atuin_common::utils::config_dir();

        create_dir_all(&config_dir)
            .wrap_err_with(|| format!("could not create dir {config_dir:?}"))?;

        let mut config_file = if let Ok(p) = std::env::var("ATUIN_CONFIG_DIR") {
            PathBuf::from(p)
        } else {
            let mut config_file = PathBuf::new();
            config_file.push(config_dir);
            config_file
        };

        config_file.push("config.toml");

        Ok(config_file)
    }

    /// Build a merged `Config` from defaults, config file, and environment.
    ///
    /// This resolves `data_dir`, initializes the data directory on disk,
    /// and layers defaults → config file → env overrides. Both `new()` and
    /// `get_config_value()` use this so the resolution logic lives in one place.
    fn build_config() -> Result<Config> {
        let config_file = Self::get_config_path()?;

        // extract data_dir first so we can use it as the base for other path defaults
        let effective_data_dir = if config_file.exists() {
            #[derive(Deserialize, Default)]
            struct DataDirOnly {
                data_dir: Option<String>,
            }

            let config_file_str =
                config_file.to_str().ok_or_else(|| eyre!("config file path is not valid UTF-8"))?;

            let partial_config = Config::builder()
                .add_source(ConfigFile::new(config_file_str, FileFormat::Toml))
                .add_source(Environment::with_prefix("atuin").prefix_separator("_").separator("__"))
                .build()
                .ok();

            let custom_data_dir = partial_config
                .and_then(|c| c.try_deserialize::<DataDirOnly>().ok())
                .and_then(|d| d.data_dir);

            match custom_data_dir {
                Some(dir) => {
                    let expanded = shellexpand::full(&dir)
                        .map_err(|e| eyre!("failed to expand data_dir path: {}", e))?;
                    PathBuf::from(expanded.as_ref())
                }
                None => atuin_common::utils::data_dir(),
            }
        } else {
            atuin_common::utils::data_dir()
        };

        DATA_DIR.set(effective_data_dir.clone()).ok();

        create_dir_all(&effective_data_dir)
            .wrap_err_with(|| format!("could not create dir {effective_data_dir:?}"))?;

        let mut config_builder = Self::builder_with_data_dir(&effective_data_dir)?;

        config_builder = if config_file.exists() {
            let config_file_str =
                config_file.to_str().ok_or_else(|| eyre!("config file path is not valid UTF-8"))?;
            config_builder.add_source(ConfigFile::new(config_file_str, FileFormat::Toml))
        } else {
            let mut file = File::create(config_file).wrap_err("could not create config file")?;
            file.write_all(EXAMPLE_CONFIG.as_bytes())
                .wrap_err("could not write default config file")?;

            config_builder
        };

        // all paths should be expanded
        let built = config_builder.build_cloned()?;

        config_builder =
            ["db_path", "record_store_path", "key_path", "logs.dir", "logs.search.file"]
                .iter()
                .map(|key| (key, built.get_string(key).unwrap_or_default()))
                .filter(|(_, value)| !value.is_empty())
                .filter_map(|(key, value)| match Self::expand_path(&value) {
                    Ok(expanded) => Some((key, expanded)),
                    Err(e) => {
                        tracing::warn!("failed to expand path for {key}: {e}");
                        None
                    }
                })
                .fold(config_builder, |builder, (key, value)| {
                    builder.set_override(key, value).unwrap_or_else(|_| {
                        panic!("failed to set absolute path override for {key}")
                    })
                });

        config_builder.build().map_err(Into::into)
    }

    /// Look up a single config value by dotted key (e.g. `"keys.scroll_exits"`).
    ///
    /// Returns the effective value after merging defaults, config file, and
    /// environment — without the side-effects of full `Settings` construction
    /// (meta store init, path expansion, etc.).
    pub fn get_config_value(key: &str) -> Result<String> {
        use config::Value;

        let config = Self::build_config()?;

        let value: Value =
            config.get(key).map_err(|e| eyre!("failed to get config value '{}': {}", key, e))?;

        Ok(Self::format_resolved_value(&value, key))
    }

    fn format_resolved_value(value: &config::Value, prefix: &str) -> String {
        use config::ValueKind;

        match &value.kind {
            ValueKind::Nil => String::new(),
            ValueKind::Boolean(b) => b.to_string(),
            ValueKind::I64(i) => i.to_string(),
            ValueKind::I128(i) => i.to_string(),
            ValueKind::U64(u) => u.to_string(),
            ValueKind::U128(u) => u.to_string(),
            ValueKind::Float(f) => f.to_string(),
            ValueKind::String(s) => s.clone(),
            ValueKind::Array(arr) => {
                let items: Vec<String> =
                    arr.iter().map(|v| Self::format_resolved_value(v, "")).collect();
                format!("[{}]", items.join(", "))
            }
            ValueKind::Table(map) => {
                let mut lines = Vec::new();
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();

                for k in keys {
                    let v = &map[k];
                    let full_key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };

                    match &v.kind {
                        ValueKind::Table(_) => {
                            lines.push(Self::format_resolved_value(v, &full_key));
                        }
                        _ => {
                            lines.push(format!(
                                "{} = {}",
                                full_key,
                                Self::format_resolved_value(v, "")
                            ));
                        }
                    }
                }

                lines.join("\n")
            }
        }
    }

    pub fn new() -> Result<Self> {
        let config = Self::build_config()?;
        let settings: Self =
            config.try_deserialize().map_err(|e| eyre!("failed to deserialize: {}", e))?;

        // Validate UI settings
        settings.ui.validate()?;

        // Register meta store config for lazy initialization on first access
        META_CONFIG.set((settings.meta.db_path.clone(), settings.local_timeout.as_secs_f64())).ok();

        Ok(settings)
    }

    fn expand_path(path: &str) -> Result<String> {
        shellexpand::full(path)
            .map(|p| p.to_string())
            .map_err(|e| eyre!("failed to expand path: {}", e))
    }

    #[must_use]
    pub fn example_config() -> &'static str {
        EXAMPLE_CONFIG
    }

    #[must_use]
    pub fn paths_ok(&self) -> bool {
        let paths: [&Path; 4] = [
            self.db_path.as_path(),
            self.record_store_path.as_path(),
            self.key_path.as_path(),
            Path::new(&self.meta.db_path),
        ];
        paths.iter().all(|p| !p.is_dangling_symlink())
    }

    /// Check that a TOML string can be successfully deserialized into a [`Settings`] object.
    pub fn validate_str(toml: &str) -> Result<(), ValidationError> {
        let config = Self::builder_with_data_dir(&atuin_common::utils::data_dir())?
            .add_source(ConfigFile::from_str(toml, FileFormat::Toml))
            .build()?;

        let settings: Self = config.try_deserialize()?;
        if let Some(dir) = &settings.data_dir {
            shellexpand::full(dir).map_err(ValidationError::DataDir)?;
        }

        settings.ui.validate()?;
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        // if this panics something is very wrong, as the default config
        // does not build or deserialize into the settings struct
        Self::builder()
            .expect("Could not build default")
            .build()
            .expect("Could not build config")
            .try_deserialize()
            .expect("Could not deserialize config")
    }
}

/// Error returned by [`Settings::validate_str`] when validation fails.
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error(transparent)]
    Ui(#[from] UiValidationError),

    #[error("failed to expand `data_dir`: {0}")]
    DataDir(shellexpand::LookupError<std::env::VarError>),
}

/// Initialize the meta store configuration for testing.
///
/// This should only be used in tests. It allows tests to bypass the normal
/// Settings::new() flow while still being able to use Settings::host_id()
/// and other meta store dependent functions.
///
/// # Safety
/// This function is not thread-safe with concurrent calls to Settings::new()
/// or other meta store initialization. Only call from tests.
#[doc(hidden)]
pub fn init_meta_config_for_testing(meta_db_path: impl Into<String>, local_timeout: f64) {
    META_CONFIG.set((meta_db_path.into(), local_timeout)).ok();
}

#[cfg(test)]
pub(crate) fn test_local_timeout() -> Duration {
    let secs = std::env::var("ATUIN_TEST_LOCAL_TIMEOUT")
        .ok()
        .and_then(|x| x.parse::<f64>().ok())
        // this hardcoded value should be replaced by a simple way to get the
        // default local_timeout of Settings if possible
        .unwrap_or(2.0);
    Duration::try_from_secs_f64(secs).unwrap_or_else(|_| Duration::from_secs(2))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use eyre::Result;
    use rstest::rstest;

    use super::{
        ConfigFile, FileFormat, FilterMode, RequestedSearchMode, SearchMode, Settings,
        UtcOffsetSpec,
    };

    #[rstest]
    #[case::plus_two_digit_hours("+02", (2, 0, 0))]
    #[case::minus_two_digit_hours("-04", (-4, 0, 0))]
    #[case::plus_hours_minutes("+05:30", (5, 30, 0))]
    #[case::minus_hours_minutes("-09:30", (-9, -30, 0))]
    // single digit hours are allowed
    #[case::plus_single_digit_hour("+2", (2, 0, 0))]
    #[case::minus_single_digit_hour("-4", (-4, 0, 0))]
    #[case::plus_single_digit_hour_minutes("+5:30", (5, 30, 0))]
    #[case::minus_single_digit_hour_minutes("-9:30", (-9, -30, 0))]
    // fully qualified form
    #[case::plus_fully_qualified("+09:30:00", (9, 30, 0))]
    #[case::minus_fully_qualified("-09:30:00", (-9, -30, 0))]
    // these offsets don't really exist but are supported anyway
    #[case::plus_zero_hour_minutes("+0:5", (0, 5, 0))]
    #[case::minus_zero_hour_minutes("-0:5", (0, -5, 0))]
    #[case::plus_with_seconds("+01:23:45", (1, 23, 45))]
    #[case::minus_with_seconds("-01:23:45", (-1, -23, -45))]
    fn can_parse_offset_timezone_spec(
        #[case] input: &str,
        #[case] expected: (i8, i8, i8),
    ) -> Result<()> {
        assert_eq!(UtcOffsetSpec::from_str(input)?.0.as_hms(), expected);
        Ok(())
    }

    /// A leading sign is required, for clarity.
    #[rstest]
    #[case::bare_hour("5")]
    #[case::bare_hour_minutes("10:30")]
    fn rejects_timezone_spec_without_leading_sign(#[case] input: &str) {
        assert!(UtcOffsetSpec::from_str(input).is_err());
    }

    #[rstest]
    #[case::in_git_context(true, true, FilterMode::Workspace)]
    #[case::not_in_git_context(true, false, FilterMode::Host)]
    #[case::workspaces_disabled(false, true, FilterMode::Host)]
    fn workspace_filter_selection(
        #[case] workspaces: bool,
        #[case] git_root: bool,
        #[case] expected: FilterMode,
    ) {
        let mut settings = super::Settings::default();
        settings.search.filters = vec![
            super::FilterMode::Workspace,
            super::FilterMode::Host,
            super::FilterMode::Directory,
            super::FilterMode::Session,
            super::FilterMode::Global,
        ];
        settings.workspaces = workspaces;

        assert_eq!(settings.default_filter_mode(git_root), expected);
    }

    #[rstest]
    fn builder_with_data_dir_uses_custom_paths() -> Result<()> {
        use std::path::PathBuf;

        let custom_dir = PathBuf::from("/custom/data/dir");
        let builder = super::Settings::builder_with_data_dir(&custom_dir)?;
        let config = builder.build()?;

        let db_path: String = config.get("db_path")?;
        let key_path: String = config.get("key_path")?;
        let record_store_path: String = config.get("record_store_path")?;
        let kv_db_path: String = config.get("kv.db_path")?;
        let scripts_db_path: String = config.get("scripts.db_path")?;
        let meta_db_path: String = config.get("meta.db_path")?;

        assert_eq!(db_path, custom_dir.join("history.db").to_str().unwrap());
        assert_eq!(key_path, custom_dir.join("key").to_str().unwrap());
        assert_eq!(record_store_path, custom_dir.join("records.db").to_str().unwrap());
        assert_eq!(kv_db_path, custom_dir.join("kv.db").to_str().unwrap());
        assert_eq!(scripts_db_path, custom_dir.join("scripts.db").to_str().unwrap());
        assert_eq!(meta_db_path, custom_dir.join("meta.db").to_str().unwrap());

        Ok(())
    }

    #[rstest]
    #[case::valid_config("search_mode = \"fuzzy\"\n")]
    #[case::empty_config("")]
    #[case::plain_data_dir("data_dir = \"/tmp/atuin-test\"\n")]
    fn validate_accepts(#[case] toml: &str) {
        assert!(Settings::validate_str(toml).is_ok());
    }

    /// The error should always name the offending key.
    #[rstest]
    #[case::invalid_enum_variant("search_mode = \"invalid\"\n", "search_mode")]
    #[case::value_of_the_wrong_type("enter_accept = \"banana\"\n", "enter_accept")]
    #[case::invalid_nested_value("[search]\nfilters = [\"nope\"]\n", "search.filters")]
    #[case::data_dir_with_an_unexpandable_variable(
        "data_dir = \"${DEFINITELY_UNSET_VAR_XYZ}/atuin\"\n",
        "data_dir"
    )]
    #[case::more_than_one_expanding_column(
        "[ui]\ncolumns = [{ type = \"duration\", expand = true }, { type = \"command\", expand = \
         true }]\n",
        "expand"
    )]
    fn validate_rejects(#[case] toml: &str, #[case] expected_err: &str) {
        let err = Settings::validate_str(toml).expect_err("config should not validate").to_string();

        assert!(err.contains(expected_err), "error should mention `{expected_err}`, got: {err}");
    }

    #[rstest]
    fn effective_data_dir_returns_default_when_not_set() {
        let effective = super::Settings::effective_data_dir();
        let default = atuin_common::utils::data_dir();

        assert!(effective.to_str().is_some());
        assert!(effective.ends_with("atuin") || effective == default);
    }

    #[rstest]
    fn keymap_config_deserializes_simple_binding() {
        let json = r#"{"emacs": {"ctrl-c": "exit"}}"#;
        let config: super::KeymapConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.emacs.len(), 1);
        if let super::KeyBindingConfig::Simple(s) = &config.emacs["ctrl-c"] {
            assert_eq!(s, "exit");
        }
    }

    #[rstest]
    fn keymap_config_deserializes_conditional_binding() {
        let json = r#"{
            "emacs": {
                "left": [
                    {"when": "cursor-at-start", "action": "exit"},
                    {"action": "cursor-left"}
                ]
            }
        }"#;
        let config: super::KeymapConfig = serde_json::from_str(json).unwrap();
        if let super::KeyBindingConfig::Rules(rules) = &config.emacs["left"] {
            assert_eq!(rules.len(), 2);
            assert_eq!(rules[0].when.as_deref(), Some("cursor-at-start"));
            assert_eq!(rules[0].action, "exit");
            assert!(rules[1].when.is_none());
            assert_eq!(rules[1].action, "cursor-left");
        } else {
            panic!("expected Rules variant");
        }
    }

    #[rstest]
    fn keymap_config_deserializes_vim_normal() {
        let json = r#"{"vim-normal": {"j": "select-next", "k": "select-previous"}}"#;
        let config: super::KeymapConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.vim_normal.len(), 2);
        assert!(config.emacs.is_empty());
    }

    #[rstest]
    fn keymap_config_is_empty_when_default() {
        let config = super::KeymapConfig::default();
        assert!(config.is_empty());
    }

    #[rstest]
    fn keymap_config_mixed_modes() {
        let json = r#"{
            "emacs": {"ctrl-c": "exit"},
            "vim-normal": {"q": "exit"},
            "inspector": {"d": "delete"}
        }"#;
        let config: super::KeymapConfig = serde_json::from_str(json).unwrap();
        assert!(!config.is_empty());
        assert_eq!(config.emacs.len(), 1);
        assert_eq!(config.vim_normal.len(), 1);
        assert_eq!(config.inspector.len(), 1);
        assert!(config.vim_insert.is_empty());
        assert!(config.prefix.is_empty());
    }

    /// Deserialize a TOML string into a [`Settings`] object.
    fn parse_settings(toml: &str) -> Settings {
        Settings::builder_with_data_dir(&atuin_common::utils::data_dir())
            .expect("could not build settings builder")
            .add_source(ConfigFile::from_str(toml, FileFormat::Toml))
            .build()
            .expect("could not build config")
            .try_deserialize()
            .expect("could not deserialize config")
    }

    #[rstest]
    fn zero_log_retention_is_accepted() {
        // `retention = 0` = keep for zero seconds (prune everything); it must load, not error.
        Settings::validate_str("[logs]\nretention = 0\n").expect("retention 0 must load");

        let settings = parse_settings("[logs]\nretention = 0\n");
        assert_eq!(settings.logs.retention, std::time::Duration::ZERO);
        // an unset per-type override inherits the global; an explicit 0 override is accepted too
        assert_eq!(settings.logs.search.retention, None);
        let overridden = parse_settings("[logs.search]\nretention = 0\n");
        assert_eq!(overridden.logs.search.retention, Some(std::time::Duration::ZERO));
    }

    #[rstest]
    fn skim_is_requested_but_resolves_to_fuzzy() {
        let settings = parse_settings("search_mode = \"skim\"\n");

        assert_eq!(settings.requested_search_mode, RequestedSearchMode::Skim);
        assert_eq!(settings.search_mode(), SearchMode::Fuzzy);
    }

    #[rstest]
    fn skim_shell_up_key_binding_resolves_to_fuzzy() {
        let settings = parse_settings("search_mode_shell_up_key_binding = \"skim\"\n");

        assert_eq!(
            settings.requested_search_mode_shell_up_key_binding,
            Some(RequestedSearchMode::Skim)
        );
        assert_eq!(settings.search_mode_shell_up_key_binding(), Some(SearchMode::Fuzzy));
    }

    #[rstest]
    #[case("prefix", RequestedSearchMode::Prefix)]
    #[case("fulltext", RequestedSearchMode::Fulltext)]
    #[case("fuzzy", RequestedSearchMode::Fuzzy)]
    #[case("daemon-fuzzy", RequestedSearchMode::DaemonFuzzy)]
    #[case("skim", RequestedSearchMode::Skim)]
    fn requested_search_mode_parses_correctly(
        #[case] value: &str,
        #[case] expected: RequestedSearchMode,
    ) {
        let settings = parse_settings(&format!("search_mode = \"{value}\"\n"));
        assert_eq!(settings.requested_search_mode, expected);
    }

    #[rstest]
    #[case(RequestedSearchMode::Prefix, SearchMode::Prefix)]
    #[case(RequestedSearchMode::Fulltext, SearchMode::FullText)]
    #[case(RequestedSearchMode::Fuzzy, SearchMode::Fuzzy)]
    #[case(RequestedSearchMode::DaemonFuzzy, SearchMode::DaemonFuzzy)]
    // "skim" was removed -- maps to "fuzzy"
    #[case(RequestedSearchMode::Skim, SearchMode::Fuzzy)]
    fn effective_search_mode_matches_requested(
        #[case] requested: RequestedSearchMode,
        #[case] expected: SearchMode,
    ) {
        assert_eq!(requested.effective_mode(), expected);
        assert_eq!(SearchMode::from(requested), expected);
    }
}
