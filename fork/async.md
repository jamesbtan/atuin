# Atuin Async Resources & Triage

Triage of `async` usages across the Atuin codebase, aimed at decoupling I/O to support a **Functional Core / Imperative Shell** architecture and enabling swappable async/sync runtimes.

---

## Executive Summary

Following the removal of server, daemon, and remote sync crates, **over 90% of all `async` usage across the codebase is driven by SQLite via `sqlx`**.

* **Most file operations are already synchronous**: Shell history importers (Bash, Zsh, Fish, etc.), lockfile acquisition, and PTY screen scraping use `std::fs` or `fs-err`.
* **Async coloring comes from storage**: Core domain operations (history record creation, string normalization, frecency sorting, query tokenization/filtering, shell history parsing) are purely CPU-bound and deterministic. They are marked `async` only because they directly call or implement traits that invoke `sqlx` methods.

---

## Async Resources & Primary Operations

### 1. SQLite Database I/O (`sqlx::SqlitePool`)
This is the primary async boundary and the reason `async`/`await` cascades up through CLI command handlers.

* **Primary Stores**:
  * **`history.db`** (`atuin_client::database::Sqlite`):
    * *Connection & Migrations*: `Sqlite::new(path, timeout)`, `Sqlite::in_memory()` (runs embedded `sqlx::migrate!`).
    * *Mutations*: `save(&History)` (`history start`), `update(&History)` (`history end`), `save_bulk(&[History])` (importers), `delete(&History)`, `delete_rows(&[HistoryId])`.
    * *Queries*: `load(id)`, `last()`, `before(timestamp)`, `list(...)`, `range(start, end)`, `search(mode, filter, ...)` (prefix, fuzzy, full-text), `history_count()`, `stats()`, `get_dups()`.
    * *Inspector queries*: `inspector_runs(&History)` and `inspector_session(&History)`.
  * **`records.db`** (`atuin_client::record::sqlite_store::SqliteStore` & `HistoryStore`):
    * *Appends*: `push()`, `push_batch()`, `push_batch_unique()`.
    * *Maintenance & Queries*: `get()`, `contains()`, `last()`, `next()`, `len()`, `verify()`, `compact()`, `purge()`, `re_encrypt()`.
  * **`meta.db`** (`atuin_client::meta::MetaDatabase`):
    * *Key-Value metadata*: `get()`, `set()`, `delete()`, and caching `host_id()`.
  * **Extension stores** (*slated for pruning in Tier 2*):
    * `kv.db` (`atuin-kv::database::Sqlite`) and `scripts.db` (`atuin-scripts::database::Sqlite`).
* **Low-level DB utilities** (`atuin-common::db::sqlite::*`):
  * *Compactor* (`compactor.rs`): Background loop checking page count and executing `PRAGMA wal_checkpoint(TRUNCATE)`.
  * *Observer* (`observe/`): Dedicated read-only connection streaming row changes (currently orphaned test/daemon utility).

---

### 2. Terminal Input & TUI Concurrency (`crossterm` + `tokio`)
Interactive search (`crates/atuin/src/command/client/search/interactive.rs`) runs the Ratatui TUI event loop:
* **Offloaded Blocking Input**:
  * `tokio::task::spawn_blocking(|| crossterm::event::poll(Duration::from_millis(250)))` wraps synchronous terminal polling to prevent blocking the async runtime.
* **Concurrency (`tokio::select!`)**:
  * Races keyboard/mouse event availability against a background history count query (`history_count = tokio::spawn(async move { count_db.history_count(false).await })`).
* **Interactive Query Execution**:
  * Invokes `app.query_results(&mut db, settings).await` on keystrokes and search filter changes.

---

### 3. File System I/O (`tokio::fs`)
Most filesystem work across the workspace uses synchronous `std::fs` / `fs-err`. Only a few isolated modules use `tokio::fs`:
* **Config CLI** (`crates/atuin/src/command/client/config.rs`):
  * `tokio::fs::read_to_string(&config_file)` and `tokio::fs::write(&config_file, &updated)`.
* **WAL file checks** (`crates/atuin-common/src/db/sqlite/compactor.rs`):
  * `tokio::fs::File::open(&wal_path)` to check WAL size.
* **Dotfiles export / Scripts execution** (*if retained*):
  * Writing temporary files to disk before execution or exporting aliases/env vars to shell scripts (`tokio::fs::create_dir_all`, `tokio::fs::write`).

---

### 4. Child Process Spawning (`tokio::process::Command`)
* **Script Runner** (`crates/atuin-scripts/src/execution.rs`):
  * `tokio::process::Command::new(...)` spawns temporary shell scripts for `atuin scripts run`.
* *(Core history commands spawn no child processes)*.

---

### 5. Async Clocks & Timers (`tokio::time`)
* **Intervals & Timeouts** (`crates/atuin-common/src/futures/`):
  * `tokio::time::sleep`, `tokio::time::timeout`, `tokio::time::interval` used for retry/jitter backoff algorithms and compactor intervals.
* **Advisory File Lock Polling** (`crates/atuin-common/src/fs/lock.rs`):
  * `LockOptions::wait` retries synchronous `try_lock()` attempts using sleep intervals (currently orphaned).

---

### 6. In-Memory Task Synchronization (`tokio::sync`)
* **Lazy Singleton Cells**:
  * `tokio::sync::OnceCell` in `settings.rs` and `meta.rs` for lazy initialization of the metadata DB pool and host ID.
* **One-shot / MPSC Channels**:
  * `tokio::sync::oneshot` and `mpsc` in `atuin-scripts` and inspector tests for task cancellation and exit signalling.

---

## Architectural Implications for Functional Core / Imperative Shell

1. **Storage Decoupling is 95% of the Problem**:
   * Isolate database access behind a clean storage boundary. If the
     core produces domain events or commands (e.g. `CommandStarted`,
     `CommandCompleted`, `FilterRequested`) and delegates persistence to
     a swappable backend, it becomes straightforward to back Atuin with
     a synchronous engine (such as `rusqlite`) or an async one (`sqlx`)
     without touching core logic.
2. **De-contaminating Shell Importers**:
   * Importers (`Importer`) currently parse files synchronously, but
     are forced to be `async` solely because the `Loader` trait takes
     `&mut self` and awaits the DB `push()`. Separating parsing into
     pure iterators/generators from persistence removes `async` from all
     importer modules.
3. **Decoupling Interactive Search (TUI)**:
   * The TUI event loop mixes state transitions, rendering, and database
     queries. Separating the interactive state machine (handling keystrokes,
     buffer navigation, and selection state) from query dispatching allows
     the core TUI state to be tested purely synchronously without any
     Tokio runtime or simulated DB queries.
