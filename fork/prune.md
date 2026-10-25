# Atuin Codebase Pruning Triage

Goal: Strip Atuin down to a single-machine shell command history tool (supporting multiple local shells like fish/zsh/bash).

---

## Tier 1: Completely Dead / Orphaned Files & Assets (Zero Callers Right Now)

These are files sitting on disk whose parent `mod` was already deleted, or assets/dependencies for deleted crates:

1. **Orphaned `.rs` files in `crates/atuin/src/command/client/`**:
   - `crates/atuin/src/command/client/account/*` (`change_password.rs`, `delete.rs`, `link.rs`, `login.rs`, `logout.rs`, `register.rs`) — `account.rs` was deleted, leaving the subfolder behind.
   - `crates/atuin/src/command/client/sync/status.rs` — `sync.rs` was deleted, leaving `sync/status.rs` behind.
   - `crates/atuin/src/command/client/output/search/*` (`mod.rs`, `writers.rs`) — `output/mod.rs` was deleted (`atuin output` CLI for daemon output capture).
   - `crates/atuin/src/command/client/hook/*` (`event.rs`, `wire.rs`) — `hook.rs` was deleted (`atuin hook` for AI/agent hooks).
2. **Orphaned `.rs` file in `crates/atuin-client/src/`**:
   - `crates/atuin-client/src/record/sync/builder.rs` — `record/sync/mod.rs` was deleted, leaving `builder.rs` behind.
3. **Orphaned resources & regression files**:
   - `crates/atuin-common/res/harnesstools/` — embedded shell/python scripts for the deleted `harnesstools` (`ai`) module.
   - `crates/atuin-client/proptest-regressions/{ai_session,packfile}` & `crates/atuin-common/proptest-regressions/{ansi.txt,json}` — proptest seed files for deleted modules.
4. **Unused `[workspace.dependencies]` in root `Cargo.toml`**:
   - ~35 workspace dependencies from deleted server/daemon/ai/sync crates are still listed in root `Cargo.toml` (`axum`, `axoupdater`, `tonic`, `tonic-prost`, `tonic-prost-build`, `tower`, `tower-http`, `prost`, `prost-types`, `protox`, `rmcp`, `rusqlite`, `fjall`, `eye_declare`, `eye_declare_engine`, `eventsource-stream`, `hyper-tls`, `hyper-util`, `http`, `listenfd`, `metrics`, `metrics-exporter-prometheus`, `native-tls`, `opentelemetry*`, `tracing-opentelemetry`, `pulldown-cmark`, `tui-textarea-2`, `wiremock`, `yaml-rust2`, `zstd`, etc.).

---

## Tier 2: Non-History Feature Crates (`dotfiles`, `kv`, `scripts`)

The workspace still compiles and exposes three non-history subcommands that were built on top of the V2 encrypted sync record store:

1. **`atuin-dotfiles`** (`crates/atuin-dotfiles`) + `crates/atuin/src/command/client/dotfiles.rs` (`atuin dotfiles alias` / `atuin dotfiles var`)
   - Legacy read-only viewer for synced shell aliases and env vars.
2. **`atuin-kv`** (`crates/atuin-kv`) + `crates/atuin/src/command/client/kv.rs` (`atuin kv set/get/list/delete`)
   - Synced key-value store (`~/.local/share/atuin/kv.db`).
3. **`atuin-scripts`** (`crates/atuin-scripts`) + `crates/atuin/src/command/client/scripts.rs` (`atuin scripts ...`)
   - Synced snippet/script manager using `minijinja` templates (`~/.local/share/atuin/scripts.db`).

If only shell command history is needed, all three crates (`atuin-dotfiles`, `atuin-kv`, `atuin-scripts`) and their CLI subcommands can be removed completely.

---

## Tier 3: The Encrypted Record Store & Crypto Stack (`records.db`, PASETO, BIP39)

Atuin currently maintains **two** SQLite databases for history:

1. **`history.db`** (`crates/atuin-client/src/database.rs`):
   - The plain local SQLite database that `atuin history start`, `atuin history end`, and `atuin search` query directly.
2. **`records.db`** (`crates/atuin-client/src/record/`, `crates/atuin-client/src/history/store.rs`, `crates/atuin/src/command/client/store.rs`, `crates/atuin-domain/src/record/`):
   - An append-only log of PASETO-V4 envelope-encrypted MessagePack records (`~/.local/share/atuin/records.db` + `~/.local/share/atuin/key`) whose purpose is multi-machine E2E encrypted sync (and backing `kv`/`scripts`/`dotfiles`).

Right now, every `history end` or delete/prune writes to `history.db` **and** encrypts a copy into `records.db` using `rusty_paseto` / `rusty_paserk` / `crypto_secretbox` / `tiny-bip39` / `rmp`.

For a single-machine history tool (without sync, `kv`, `scripts`, or `dotfiles`), `records.db`, `atuin store`, `HistoryStore`, `SqliteStore`, `~/.local/share/atuin/key`, `crates/atuin-common/src/encryption/`, and all encryption/mnemonic dependencies (`rusty_paseto`, `rusty_paserk`, `crypto_secretbox`, `tiny-bip39`, `argon2`, `rmp`) can be dropped. `history.db` alone stores 100% of local shell history.

---

## Tier 4: `atuin-pty-proxy` & Terminal Output Capture

- **`atuin-pty-proxy`** (`crates/atuin-pty-proxy`) wraps the shell in a pseudo-terminal (`atuin pty-proxy` / `atuin hex`).
- It was built for two purposes:
  1. **Output capture** (`crates/atuin-pty-proxy/src/capture.rs` &
     `crates/atuin/src/command/client/search/inspector/output.rs`): Capturing
     stdout/stderr of commands and sending them to `atuin-daemon`. Without
     `atuin-daemon`, output capture is already a no-op because `pty-proxy`
     only captured output when the daemon sent a capture request over the
     socket, and `inspector/output.rs` in the TUI has no way to load output
     without the daemon.
  2. **Centered popup TUI mode** (`crates/atuin-pty-proxy/src/screen.rs`):
     Letting `Ctrl-R` render as a floating box over existing terminal text
     using `atuin-vt100` to snapshot the screen.
- If normal inline/fullscreen `Ctrl-R` via shell hooks
  (`atuin init zsh/bash/fish`) is sufficient,
  `atuin-pty-proxy` and `atuin-vt100` can be removed.

---

## Tier 5: Small CLI Commands & Config Leftovers

1. **`crates/atuin/src/command/client/setup.rs`** (`atuin setup`):
   - Interactive wizard that prompts to enable AI/daemon/pty-proxy features in `config.toml`.
2. **`crates/atuin/src/command/external.rs`** + **`crates/atuin-client/src/plugin.rs`**:
   - Runs `atuin-<subcommand>` binaries from `$PATH`.
3. **`crates/atuin/src/command/contributors.rs`** (`atuin contributors`):
   - Prints a list of GitHub contributors.
4. **Dead settings structs in `crates/atuin-client/src/settings/`**:
   - `settings/daemon.rs` (`[daemon]` config section)
   - `settings/output.rs` (`[output]` capture config section)
   - `settings/meta.rs` (`[meta]` config section)
   - `settings/kv.rs` & `settings/scripts.rs`
   - Sync/update settings (`auto_sync`, `sync_address`, `sync_frequency`, `key_path`, `record_store_path`, `update_check`)
5. **`crates/atuin-client/src/meta.rs`** (`meta.db`):
   - Stores `host_id`, `last_sync_time`, `last_version_check_time`, `session`, and `hub_session`. On a single machine without sync, `meta.db` is only used to persist a local `host_id` UUID.

---

## Target State After Pruning Tiers 1–5

A lean, single-database (`history.db`) workspace containing:
- **`atuin`**: CLI + Ratatui `Ctrl-R` interactive search, `history start/end/list/prune/dedup`, `init zsh/bash/fish/...`, `import`, `stats`, `wrapped`, `doctor`, `info`, `config`, `gen-completions`.
- **`atuin-client`**: `history.db` SQLite queries, history builders, shell history importers, theme, settings.
- **`atuin-history`**: Frecency/smart sort & stats.
- **`atuin-common`** / **`atuin-domain`**: Minimal shared types & SQLite helpers.
