# Atuin SQLite Database Architecture & Triage

Triage of SQLite database usage across Atuin, detailing database files, processes reading and writing to each file, concurrency patterns, and architectural considerations.

---

## 1. Executive Summary

Atuin utilizes **5 distinct SQLite database files** by default, stored in the user data directory (`~/.local/share/atuin/` on Linux/macOS, `%USERPROFILE%/.local/share/atuin/` on Windows):

```yaml
databases:
  - file: history.db
    default_path: ~/.local/share/atuin/history.db
    implementation: atuin_client::database::Sqlite
    journal_mode: WAL
    primary_role: Main shell command history store & search index

  - file: records.db
    default_path: ~/.local/share/atuin/records.db
    implementation: atuin_client::record::sqlite_store::SqliteStore
    journal_mode: WAL
    primary_role: Unified append-only encrypted record store (V2 storage)

  - file: meta.db
    default_path: ~/.local/share/atuin/meta.db
    implementation: atuin_client::meta::MetaStore
    journal_mode: DELETE
    primary_role: Client machine metadata (host_id, migration markers)

  - file: kv.db
    default_path: ~/.local/share/atuin/kv.db
    implementation: atuin_kv::database::Database
    journal_mode: WAL
    primary_role: Materialized key-value store

  - file: scripts.db
    default_path: ~/.local/share/atuin/scripts.db
    implementation: atuin_scripts::database::Database
    journal_mode: WAL
    primary_role: Materialized script management store
```

> [!NOTE]
> **Process Model**: Following the removal of the standalone gRPC daemon
(`atuin-daemon`) and HTTP server (`atuin-server`), **all database
access is executed directly by short-lived `atuin` CLI processes**
(spawned either interactively by the user or automatically as shell
hooks on each command execution). There is no persistent daemon holding
database connections open.

---

## 2. Database Inventory & Process Access Matrix

### 2.1. `history.db` (Shell History Store)

* **Path Configuration**: `settings.db_path` in `config.toml`, overridden by `ATUIN_DB_PATH`.
* **Crate / Module**: [`crates/atuin-client/src/database.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/database.rs)
* **Migrations**: Embedded via `db::migrate!(pool, "./migrations")` in `atuin-client`.
* **Pragmas**: `journal_mode = WAL`, `synchronous = NORMAL`, `foreign_keys = ON`, `journal_size_limit = 4MB`.
* **Schema**:
  * Table `history`: `id` (UUIDv7 string), `timestamp` (nanoseconds), `duration` (nanoseconds), `exit`, `command`, `cwd`, `session`, `hostname`, `author`, `intent`, `deleted_at`, `shell`, `author_kind`.
  * Indexes on `(timestamp, id)`, `(command, timestamp)`, `(session, timestamp)`, etc.

#### Writers
* **`atuin history start`** (shell pre-exec hook): Spawns on command line entry; writes initial command record with zero duration and timestamp. Errors are swallowed to prevent shell lockups.
* **`atuin history end`** (shell pre-cmd / post-exec hook): Spawns upon command completion; updates record with `exit` code and elapsed `duration`. If `store_failed = false` and exit code is non-zero, deletes the partial entry.
* **`atuin import <bash|zsh|nu|...>`**: Executes bulk inserts (`save_bulk`) of legacy shell histories into `history.db`.
* **`atuin history prune`**: Deletes historical entries matching age or size retention limits.
* **`atuin history delete <id...>`**: Deletes specific history rows by ID.
* **`atuin store rebuild`**: Reconstructs rows in `history.db` by replaying decrypted history records from `records.db`.
* **First process initialization**: Runs pending SQL migrations.

#### Readers
* **`atuin history end`**: Reads the matching record via `load(id)` to compute duration and verify state prior to updating.
* **`atuin search`** (TUI interactive or non-interactive): Reads and filters history using prefix, fuzzy, or full-text search strategies, evaluating frecency/recency scores and contextual filters (host, session, directory, git root).
* **`atuin stats`**: Reads and aggregates command statistics, exit code ratios, and command frequencies over daily/weekly periods.
* **`atuin history list` & `atuin history last`**: Reads formatted history log outputs.
* **`atuin wrapped`**: Aggregates annual history metrics.
* **`atuin doctor`**: Checks SQLite version, database connectivity, and row count.
* **`atuin scripts new`**: Inspects recent history to help construct script templates.

---

### 2.2. `records.db` (Append-Only Encrypted Record Store)

* **Path Configuration**: `settings.record_store_path` in `config.toml`.
* **Crate / Module**: [`crates/atuin-client/src/record/sqlite_store.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/record/sqlite_store.rs)
* **Migrations**: Embedded via `db::migrate!(pool, "./record-migrations")` in `atuin-client`.
* **Pragmas**: `journal_mode = WAL`, `synchronous = NORMAL`, `foreign_keys = ON`, `journal_size_limit = 4MB`.
* **Schema**:
  * Table `store`: `id` (UUIDv7), `idx` (monotonic per-host index), `host` (Host UUIDv7), `tag` (data kind: `HISTORY`, `KV`, `SCRIPTS`, `ALIAS`, `VAR`), `timestamp`, `version`, `data` (PASETO V4 encrypted payload), `cek` (encrypted Content Encryption Key).
  * Indexes on `(host, tag, idx)` and `(tag, timestamp)`.

#### Writers
* **`atuin history end`**: Whenever a history entry completes, it is serialized, envelope-encrypted with PASETO V4, and appended to `records.db` via `history_store.push(h)`.
* **`atuin kv set` & `atuin kv delete`**: Pushes encrypted key-value mutations or tombstones tagged with `RecordTag::Kv`.
* **`atuin scripts new`, `edit`, `delete`**: Pushes encrypted script mutations or tombstones tagged with `RecordTag::Scripts`.
* **`atuin store purge`**: Purges deleted or compactible record chunks.
* **`atuin store rekey`**: Re-encrypts all records with a new encryption key and updates records.
* **First process initialization**: Runs pending record-migrations.

#### Readers
* **`atuin store verify`**: Sequentially decrypts and verifies envelope assertions, PASETO tokens, and hash chains.
* **`atuin store status`**: Reads latest indices, total counts, and record headers.
* **`atuin store rebuild`**: Reads and decrypts all records to regenerate materialized databases (`history.db`, `kv.db`, `scripts.db`).
* **`atuin dotfiles list`**: Reads and decrypts legacy `RecordTag::Alias` and `RecordTag::Var` records.
* **`atuin kv rebuild`**: Reads all `RecordTag::Kv` entries to re-populate `kv.db`.

---

### 2.3. `meta.db` (Client Node Metadata Store)

* **Path Configuration**: `settings.meta.db_path` in `config.toml`, overridden by `ATUIN_META__DB_PATH`.
* **Crate / Module**: [`crates/atuin-client/src/meta.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/meta.rs)
* **Migrations**: Embedded via `db::migrate!(pool, "./meta-migrations")` in `atuin-client`.
* **Pragmas**: `journal_mode = DELETE` (non-WAL), `foreign_keys = OFF`, `restrict_permissions` (`0600`).
* **Schema**:
  * Table `meta`: `key TEXT PRIMARY KEY`, `value TEXT`, `updated_at INTEGER`.

#### Writers
* **Initial run / Host ID generation**: When `Settings::host_id()` is first called and no `host_id` exists in `meta.db`, it generates a new UUIDv7 and writes `host_id` to `meta.db`.
* **Legacy file migration**: Migrates legacy plaintext file `~/.local/share/atuin/host_id` into `meta.db` on first open and sets `files_migrated = 1`.
* **First process initialization**: Runs pending meta-migrations.

#### Readers
* **Almost every `atuin` command**:
  * `atuin history start` / `query_context()`: Reads `host_id` to tag the execution environment.
  * `atuin history end`: Reads `host_id` to build the record series key for `records.db`.
  * `atuin search`: Reads `host_id` for host-scoped search filters.
  * `atuin kv ...`: Reads `host_id` to scope key-value pairs.
  * `atuin scripts ...`: Reads `host_id` to scope scripts.
  * `atuin dotfiles ...`: Reads `host_id` to scope aliases and environment variables.
  * `atuin info`: Reads database path and host information.

---

### 2.4. `kv.db` (Materialized Key-Value Store)

* **Path Configuration**: `settings.kv.db_path` in `config.toml`.
* **Crate / Module**: [`crates/atuin-kv/src/database.rs`](file:///home/jms/build/atuin/crates/atuin-kv/src/database.rs)
* **Migrations**: Embedded via `db::migrate!(pool, "./migrations")` in `atuin-kv`.
* **Pragmas**: `journal_mode = WAL`, `synchronous = NORMAL`, `foreign_keys = ON`, `journal_size_limit = 4MB`.
* **Schema**:
  * Table `kv`: `namespace TEXT`, `key TEXT`, `value TEXT`, PRIMARY KEY (`namespace`, `key`).

#### Writers
* **`atuin kv set <namespace> <key> <value>`**: Upserts namespace-scoped key-value pairs.
* **`atuin kv delete <namespace> <key>`**: Deletes key-value pairs.
* **`atuin kv rebuild` / `atuin store rebuild`**: Rebuilds the table from decrypted `RecordTag::Kv` records.
* **First process initialization**: Runs pending KV migrations.

#### Readers
* **`atuin kv get <namespace> <key>`**: Fetches value for key within namespace.
* **`atuin kv list [namespace]`**: Lists keys and values, optionally filtered by namespace.

---

### 2.5. `scripts.db` (Materialized Script Store)

* **Path Configuration**: `settings.scripts.db_path` in `config.toml`.
* **Crate / Module**: [`crates/atuin-scripts/src/database.rs`](file:///home/jms/build/atuin/crates/atuin-scripts/src/database.rs)
* **Migrations**: Embedded via `db::migrate!(pool, "./migrations")` in `atuin-scripts`.
* **Pragmas**: `journal_mode = WAL`, `synchronous = NORMAL`, `foreign_keys = ON`, `journal_size_limit = 4MB`.
* **Schema**:
  * Table `scripts`: `id TEXT PRIMARY KEY`, `name TEXT UNIQUE`, `description TEXT`, `shebang TEXT`, `script TEXT`.

#### Writers
* **`atuin scripts new`**: Inserts a new named script.
* **`atuin scripts edit`**: Modifies an existing script body or description.
* **`atuin scripts delete`**: Deletes a script by name.
* **`atuin store rebuild`**: Rebuilds scripts from decrypted `RecordTag::Scripts` records.
* **First process initialization**: Runs pending script migrations.

#### Readers
* **`atuin scripts run <name>`**: Reads script definition and shebang to execute it.
* **`atuin scripts list`**: Queries and lists registered scripts.
* **`atuin scripts get <name>`**: Retrieves script content.

---

## 3. External SQLite Databases (Read-Only Import Sources)

During `atuin import`, Atuin connects to external, third-party SQLite databases as read-only data sources to migrate historical commands into Atuin's `history.db`:

1. **Zsh HistDB**: Located at `${HISTDB_FILE}` or `~/.histdb/zsh-history.db` ([`crates/atuin-client/src/import/zsh_histdb.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/import/zsh_histdb.rs)).
2. **Nushell SQLite**: Located at `~/.config/nushell/history.sqlite3` ([`crates/atuin-client/src/import/nu_histdb.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/import/nu_histdb.rs)).
3. **Xonsh SQLite**: Located at `~/.local/share/xonsh/xonsh-history.sqlite` ([`crates/atuin-client/src/import/xonsh_sqlite.rs`](file:///home/jms/build/atuin/crates/atuin-client/src/import/xonsh_sqlite.rs)).

---

## 4. Operational & Architectural Notes

### 4.1. Concurrency & Multi-Process SQLite Locking
* **Concurrent CLI Execution**: Because every shell tab or pane runs independent `atuin history start` and `atuin history end` commands simultaneously, SQLite lock contention is expected.
* **WAL Mode**: Used for 4 out of 5 databases (`history.db`, `records.db`, `kv.db`, `scripts.db`). WAL allows arbitrary concurrent readers while a write transaction is executing.
* **Busy Timeout**: Connections are opened with a 2.0-second busy timeout (`local_timeout` setting). Transient lock errors (`BUSY`, `LOCKED`, `BUSY_RECOVERY`, `BUSY_SNAPSHOT`) are classified via [`TransientResultCode`](file:///home/jms/build/atuin/crates/atuin-common/src/db/sqlite/mod.rs#L23-L56).
* **Fail-Safe Shell Hooks**: In [`handle_start`](file:///home/jms/build/atuin/crates/atuin/src/command/client/history.rs#L415-L421), database write errors are logged at `debug` level and suppressed, ensuring that lock contention or a read-only filesystem never blocks or breaks the user's interactive shell prompt.

### 4.2. Dual Storage & Redundancy (`history.db` vs `records.db`)
* Every command recorded via `history end` is written twice:
  1. Plaintext structured record into `history.db` (for immediate local querying and indexing).
  2. Envelope-encrypted record into `records.db` (the append-only event source).
* `records.db` serves as the primary system of record for disaster recovery: all materialized tables (`history.db`, `kv.db`, `scripts.db`) can be wiped and fully reconstructed via `atuin store rebuild`.

### 4.3. Auxiliary SQLite Files
* For each WAL-mode database, SQLite generates two auxiliary files alongside the database file:
  * `*-wal`: The Write-Ahead Log.
  * `*-shm`: Shared-memory file used for inter-process reader/writer coordination index.
* For `meta.db` (`Journaling::Delete`), a rollback journal file `meta.db-journal` appears only during active write transactions and is deleted upon commit.

### 4.4. Background WAL Compactor & Pragmas
* [`Compactor::spawn_active`](file:///home/jms/build/atuin/crates/atuin-common/src/db/sqlite/compactor.rs#L43-L50): When opening a WAL database for read-write access, Atuin spawns a background compactor task in the process runtime:
  * Runs every 1 minute.
  * Checks if WAL file exceeds `32 MB` (`THRESHOLD_BYTES`).
  * If exceeded, executes `PRAGMA wal_checkpoint(TRUNCATE)` on a dedicated connection with a 500ms timeout.
* On pool drop, SQLite's `optimize_on_close` runs `PRAGMA optimize` on read-write connections.

### 4.5. Security & File Permissions
* On Unix platforms, [`SqliteBuilder::restrict_permissions`](file:///home/jms/build/atuin/crates/atuin-common/src/db/sqlite/builder.rs#L194-L203) explicitly sets file mode to `0600` (`-rw-------`) on newly opened databases.
* Furthermore, [`AtuinCmd::run`](file:///home/jms/build/atuin/crates/atuin/src/command/mod.rs#L36-L42) resets the process umask to `077` before opening or creating database files, preventing any other user on the system from accessing the databases.
