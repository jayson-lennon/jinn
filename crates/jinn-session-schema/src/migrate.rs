//! The migration runner and individual migrations (v0..=v33).
//!
//! This crate is the single source of truth for the session schema. Note the
//! deliberate version pins, which keep the migrations immutable:
//!
//! - error type `SessionStoreError` → [`crate::SchemaMigrationError`]
//! - `super::sqlite::LegacySessionColumns` → [`crate::legacy::LegacySessionColumns`]
//! - the v20 backfill builds the blob via [`crate::PersistableCoreV20`] (a
//!   version-pinned snapshot) instead of importing the live `PersistableCore`
//!
//! The DDL strings, version guards, and JSON surgery helpers are unchanged.

use std::collections::HashMap;

use error_stack::{Report, ResultExt as _};
use rusqlite::params;

use crate::SchemaMigrationError;
use crate::legacy::LegacySessionColumns;

/// The highest migration version this runner knows how to apply.
///
/// `run_pending` skips `BEGIN` when the DB is already at this version, so an
/// up-to-date database pays no transaction cost on startup.
///
/// MUST be bumped in lockstep with [`MIGRATIONS`]'s chain
/// table: every new `migrate_vN` block requires this constant set to `N`.
/// The drift-guard test
/// (`upgrade_from_every_seedable_version_reaches_latest`) fails otherwise. A
/// stale value is the worst kind of bug: fresh databases still run the whole
/// chain (so row-count tests pass) while already-migrated databases silently
/// never receive the new migration, because the no-op early-return fires
/// first. This exact bug shipped once — v27 landed with the constant left
/// at 26.
pub const LATEST_VERSION: i32 = CHAIN_LATEST_VERSION;

/// The vacuum migration whose real work must run outside a transaction.
///
/// `run_pending` splits the chain around this version: migrations below it
/// commit in the first transaction, the migration itself runs at its natural
/// position between the two transactions (its real work — see
/// [`convert_to_incremental_auto_vacuum`] — is a full `VACUUM`, which SQLite
/// forbids inside a transaction), and migrations above it commit in the
/// second. Print order, application order, and `_migrations` rowid order are
/// therefore the chain order on every run.
const VACUUM_MIGRATION_VERSION: i32 = 32;

/// Runs all pending migrations in chain order, split around the vacuum
/// migration's database rewrite.
///
/// Bootstraps the tracking table, reads the current version, then applies
/// every unapplied migration — announced before its work, recorded after —
/// as three phases:
///
/// 1. everything below [`VACUUM_MIGRATION_VERSION`] commits inside one
///    `BEGIN IMMEDIATE … COMMIT` transaction;
/// 2. the vacuum migration runs at its natural position, outside any
///    transaction (SQLite forbids `VACUUM` inside one), and its version is
///    recorded in its own tiny transaction immediately after the conversion;
/// 3. everything above it commits inside a second transaction.
///
/// A failure inside either chain transaction (or an interrupt, e.g. Ctrl+C /
/// SIGKILL before its `COMMIT`) rolls that transaction back to where the
/// previous phase left off — no half-applied schema. A failure after the v32
/// conversion does **not** re-run it: the rewrite and its `_migrations` row
/// are already durable, so the next startup resumes at the next version. The
/// one residual window is a crash between the conversion and its record
/// commit, which re-runs the vacuum once on restart — harmless, because
/// `VACUUM` is atomic and the conversion is idempotent.
///
/// The caller ([`crate::run_migrations`]) is responsible for the FK toggle
/// around this call — FK pragmas cannot change inside an open transaction,
/// so the toggle window spans both transactions.
///
/// A database already at [`LATEST_VERSION`] is a no-op: no transaction is
/// issued, so existing-DB startup cost is unchanged.
///
/// Returns `true` when at least one migration was applied (i.e. the database
/// was below [`LATEST_VERSION`]) and `false` on a no-op run. Callers use this
/// to skip post-migration work that only makes sense when the schema changed.
///
/// # Errors
///
/// Returns an error if a transaction cannot be opened or committed, if the
/// v32 conversion fails, or if any migration or version recording fails (the
/// enclosing transaction is rolled back first).
pub(crate) fn run_pending(
    conn: &mut rusqlite::Connection,
) -> Result<bool, Report<SchemaMigrationError>> {
    bootstrap_tracking_table(conn)?;
    let current = current_version(conn)?;

    // No-op path: an up-to-date database pays no transaction cost.
    if current >= LATEST_VERSION {
        return Ok(false);
    }

    // Phase 1: everything below the vacuum migration, one transaction.
    if chain_has_pending_in(current, VACUUM_MIGRATION_VERSION) {
        run_in_transaction(conn, |conn| {
            apply_migration_range(conn, current, VACUUM_MIGRATION_VERSION, &announce_migration)
        })?;
    }

    // Phase 2: the vacuum migration at its natural position — outside any
    // transaction, because SQLite forbids VACUUM inside one. The chain entry
    // is a documented no-op (see [`migrate_v32`]); its version is recorded
    // in its own transaction immediately after the conversion so a later
    // chain failure can never roll the record back while the rewrite
    // persists (that mismatch would force a pointless re-vacuum on restart).
    if current < VACUUM_MIGRATION_VERSION {
        let vacuum = chain_migration(VACUUM_MIGRATION_VERSION);
        announce_migration(vacuum.version, vacuum.name);
        convert_to_incremental_auto_vacuum(conn)?;
        run_in_transaction(conn, |conn| {
            record_version(conn, vacuum.version, vacuum.name)
        })?;
    }

    // Phase 3: everything above the vacuum migration, second transaction.
    if chain_has_pending_in(VACUUM_MIGRATION_VERSION, i32::MAX) {
        run_in_transaction(conn, |conn| {
            apply_migration_range(
                conn,
                VACUUM_MIGRATION_VERSION,
                i32::MAX,
                &announce_migration,
            )
        })?;
    }

    // Only a below-latest database reaches here (the early return above),
    // so at least one migration was applied.
    Ok(true)
}

/// Whether the chain has a migration in `(lower_exclusive, upper_exclusive)`.
///
/// Guards each of [`run_pending`]'s transactions: a phase with nothing
/// pending opens no transaction.
fn chain_has_pending_in(lower_exclusive: i32, upper_exclusive: i32) -> bool {
    MIGRATIONS
        .iter()
        .any(|m| m.version > lower_exclusive && m.version < upper_exclusive)
}

/// The chain's entry for `version`.
///
/// # Panics
///
/// Panics if the chain has no such entry — a programmer error the
/// drift-guard tests catch long before any startup.
#[expect(
    clippy::panic,
    reason = "the version is a chain-member constant; a miss is a programmer error the drift-guard tests catch"
)]
fn chain_migration(version: i32) -> &'static Migration {
    MIGRATIONS
        .iter()
        .find(|m| m.version == version)
        .unwrap_or_else(|| panic!("migration v{version} missing from MIGRATIONS"))
}

/// Opens a `BEGIN IMMEDIATE` transaction.
///
/// Raw `execute_batch`, not rusqlite's `Transaction`, because the connection
/// is a `&mut` borrowed under dao's `with_conn`; `Transaction` would need
/// ownership and change the caller's signature. `IMMEDIATE` acquires the
/// write lock up front so a run can't fail mid-transaction on a lock
/// upgrade.
fn begin_immediate(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .change_context(SchemaMigrationError)
        .attach("begin migration transaction")
}

/// Commits the open transaction.
fn commit(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("COMMIT")
        .change_context(SchemaMigrationError)
        .attach("commit migration transaction")
}

/// Runs `body` inside one `BEGIN IMMEDIATE … COMMIT` transaction.
///
/// A `body` failure rolls back — best-effort, so the original migration
/// error propagates — before returning, leaving no half-applied work on
/// disk.
fn run_in_transaction(
    conn: &mut rusqlite::Connection,
    body: impl FnOnce(&mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>>,
) -> Result<(), Report<SchemaMigrationError>> {
    begin_immediate(conn)?;
    match body(conn) {
        Ok(()) => commit(conn),
        Err(report) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(report)
        }
    }
}

/// A single migration in the chain: its version, stable name, and DDL.
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub apply: fn(&mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>>,
}

/// The full migration chain, in application order.
///
/// One row per migration: version number, stable name (recorded in
/// `_migrations`), and the DDL function. This table is the single source of
/// truth for both [`apply_migration_range`] (production) and
/// [`apply_migrations_inner`] (test seeding), so a new migration cannot
/// appear in one list and be missing from the other.
///
/// Every `apply` returns the Report-bearing signature; `migrate_v6` (the one
/// infallible legacy migration) is wrapped by [`migrate_v6_step`].
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 0,
        name: "create_initial_schema",
        apply: migrate_v0,
    },
    Migration {
        version: 1,
        name: "add_cwd_column",
        apply: migrate_v1,
    },
    Migration {
        version: 2,
        name: "add_created_at_column",
        apply: migrate_v2,
    },
    Migration {
        version: 3,
        name: "add_ignored_to_session_entries",
        apply: migrate_v3,
    },
    Migration {
        version: 4,
        name: "add_cost_to_token_ledger",
        apply: migrate_v4,
    },
    Migration {
        version: 5,
        name: "add_lifecycle_columns_to_sessions",
        apply: migrate_v5,
    },
    Migration {
        version: 6,
        name: "add_archived_column",
        apply: migrate_v6_step,
    },
    Migration {
        version: 7,
        name: "add_lifecycle_script_state_column",
        apply: migrate_v7,
    },
    Migration {
        version: 8,
        name: "add_metadata_column",
        apply: migrate_v8,
    },
    Migration {
        version: 9,
        name: "rename_session_entries_to_session_history",
        apply: migrate_v9,
    },
    Migration {
        version: 10,
        name: "consolidate_to_compaction_strategy",
        apply: migrate_v10,
    },
    Migration {
        version: 11,
        name: "add_is_workflow_column",
        apply: migrate_v11,
    },
    Migration {
        version: 12,
        name: "replace_ignored_with_context_override",
        apply: migrate_v12,
    },
    Migration {
        version: 13,
        name: "add_judge_meta_column",
        apply: migrate_v13,
    },
    Migration {
        version: 14,
        name: "add_context_history",
        apply: migrate_v14,
    },
    Migration {
        version: 15,
        name: "drop_strategy_state_column",
        apply: migrate_v15,
    },
    Migration {
        version: 16,
        name: "rename_is_workflow_to_is_automated_and_add_persist",
        apply: migrate_v16,
    },
    Migration {
        version: 17,
        name: "rewrite_model_to_model_selection_and_add_model_used",
        apply: migrate_v17,
    },
    Migration {
        version: 18,
        name: "rename_entries_timestamp_to_timing",
        apply: migrate_v18,
    },
    Migration {
        version: 19,
        name: "rewrite_metadata_blob_profile_model",
        apply: migrate_v19,
    },
    Migration {
        version: 20,
        name: "drop_zombie_columns_backfill_metadata",
        apply: migrate_v20,
    },
    Migration {
        version: 21,
        name: "add_discord_thread_table",
        apply: migrate_v21,
    },
    Migration {
        version: 22,
        name: "add_entry_blobs_table",
        apply: migrate_v22,
    },
    Migration {
        version: 23,
        name: "strip_s_prefix_from_session_ids",
        apply: migrate_v23,
    },
    Migration {
        version: 24,
        name: "add_token_ledger_prompt_cached_columns",
        apply: migrate_v24,
    },
    Migration {
        version: 25,
        name: "add_entries_token_count_column",
        apply: migrate_v25,
    },
    Migration {
        version: 26,
        name: "add_fts_search_index",
        apply: migrate_v26,
    },
    Migration {
        version: 27,
        name: "add_fts_dirty_resume_offset",
        apply: migrate_v27,
    },
    Migration {
        version: 28,
        name: "add_fts_rowid_map",
        apply: migrate_v28,
    },
    Migration {
        version: 29,
        name: "add_entries_model_used_columns",
        apply: migrate_v29,
    },
    Migration {
        version: 30,
        name: "add_sessions_in_library_column",
        apply: migrate_v30,
    },
    Migration {
        version: 31,
        name: "add_sessions_is_bot_column",
        apply: migrate_v31,
    },
    Migration {
        version: 32,
        name: "convert_database_to_incremental_auto_vacuum",
        apply: migrate_v32,
    },
    Migration {
        version: 33,
        name: "index_session_history_entry_id",
        apply: migrate_v33,
    },
];

/// The newest migration in [`MIGRATIONS`], kept in lockstep with
/// [`LATEST_VERSION`] (which is asserted equal to this by
/// `latest_version_matches_chain`).
pub const CHAIN_LATEST_VERSION: i32 = 33;

/// Applies every migration in `(lower_exclusive, upper_exclusive)` under the
/// given announcer.
///
/// Runs inside the caller's open transaction; the `?` on each step
/// short-circuits on the first failure, at which point the caller rolls
/// back. `MIGRATIONS` is the single source of truth — the range bounds only
/// select from it, so application order is always chain order.
fn apply_migration_range(
    conn: &mut rusqlite::Connection,
    lower_exclusive: i32,
    upper_exclusive: i32,
    announce: &dyn Fn(i32, &str),
) -> Result<(), Report<SchemaMigrationError>> {
    for migration in MIGRATIONS {
        if migration.version > lower_exclusive && migration.version < upper_exclusive {
            announce(migration.version, migration.name);
            (migration.apply)(conn)?;
            record_version(conn, migration.version, migration.name)?;
        }
    }
    Ok(())
}

/// Prints the user-facing pre-TUI announcement for a migration about to run.
///
/// Migrations apply before the TUI launches (terminal still in normal mode),
/// so plain stderr is visible to the user; this explains an upgrade launch's
/// startup wait.
#[expect(
    clippy::print_stderr,
    reason = "pre-TUI user-facing announcement: migrations run before any tracing subscriber exists"
)]
fn announce_migration(version: i32, name: &str) {
    eprintln!("{}", announcement_line(version, name));
}

/// Renders the announcement line exactly as a user sees it.
pub fn announcement_line(version: i32, name: &str) -> String {
    format!("jinn: applying migration v{version} ({name})\u{2026}")
}

/// Wraps the infallible legacy `migrate_v6` so it fits the chain's
/// Report-bearing signature. It cannot fail, so the error arm is unreachable.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature must match every other chain entry's Result-bearing apply"
)]
fn migrate_v6_step(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    migrate_v6(conn);
    Ok(())
}

// ── Tracking table ───────────────────────────────────────────────────────

/// Creates the `_migrations` tracking table.
///
/// This is the only place `IF NOT EXISTS` is used - the tracking table must
/// bootstrap itself before version checking can begin. All other migrations
/// use strict DDL so failures are loud.
///
/// # Errors
///
/// Returns an error if the tracking table cannot be created.
pub fn bootstrap_tracking_table(
    conn: &mut rusqlite::Connection,
) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (\
         version INTEGER NOT NULL,\
         name TEXT NOT NULL,\
         applied_at TEXT NOT NULL DEFAULT (datetime('now')))",
    )
    .change_context(SchemaMigrationError)
    .attach("failed to create _migrations table")?;
    Ok(())
}

/// Reads the highest migration version from the tracking table.
///
/// Returns -1 if no migrations have been recorded (empty database).
fn current_version(conn: &mut rusqlite::Connection) -> Result<i32, Report<SchemaMigrationError>> {
    let version: Option<i32> = conn
        .query_row(
            "SELECT MAX(version) AS version FROM _migrations",
            [],
            |row| row.get(0),
        )
        .change_context(SchemaMigrationError)
        .attach("failed to query migration version")?;
    Ok(version.unwrap_or(-1))
}

/// Records a completed migration in the tracking table.
///
/// # Errors
///
/// Returns an error if the row cannot be inserted.
pub fn record_version(
    conn: &mut rusqlite::Connection,
    version: i32,
    name: &str,
) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute(
        "INSERT INTO _migrations (version, name) VALUES (?, ?)",
        params![version, name],
    )
    .change_context(SchemaMigrationError)
    .attach(format!("failed to record migration v{version}"))?;
    Ok(())
}

// ── Migrations ───────────────────────────────────────────────────────────

/// v0: Initial schema - sessions, entries, session_entries, token_ledger.
pub fn migrate_v0(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE sessions (\
         id TEXT PRIMARY KEY,\
         title TEXT,\
         updated_at TEXT NOT NULL,\
         profile TEXT NOT NULL DEFAULT '{}',\
         blobs TEXT NOT NULL DEFAULT '{}',\
         parent_session TEXT DEFAULT NULL,\
         strategy_state TEXT NOT NULL DEFAULT '{}')",
    )
    .change_context(SchemaMigrationError)
    .attach("v0: create sessions table")?;

    conn.execute_batch(
        "CREATE TABLE entries (\
         id TEXT PRIMARY KEY,\
         timestamp TEXT NOT NULL,\
         kind TEXT NOT NULL)",
    )
    .change_context(SchemaMigrationError)
    .attach("v0: create entries table")?;

    conn.execute_batch(
        "CREATE TABLE session_entries (\
         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,\
         entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,\
         ordinal INTEGER NOT NULL,\
         pin_position TEXT DEFAULT NULL,\
         PRIMARY KEY (session_id, entry_id),\
         UNIQUE (session_id, ordinal))",
    )
    .change_context(SchemaMigrationError)
    .attach("v0: create session_entries table")?;

    conn.execute_batch(
        "CREATE TABLE token_ledger (\
         id INTEGER PRIMARY KEY AUTOINCREMENT,\
         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,\
         timestamp TEXT NOT NULL,\
         tokens_sent INTEGER NOT NULL,\
         tokens_received INTEGER NOT NULL)",
    )
    .change_context(SchemaMigrationError)
    .attach("v0: create token_ledger table")?;

    conn.execute_batch(
        "CREATE INDEX idx_session_entries_session ON session_entries(session_id, ordinal)",
    )
    .change_context(SchemaMigrationError)
    .attach("v0: create session_entries index")?;

    conn.execute_batch("CREATE INDEX idx_token_ledger_session ON token_ledger(session_id)")
        .change_context(SchemaMigrationError)
        .attach("v0: create token_ledger index")?;
    Ok(())
}

/// v1: Add `cwd` column to sessions.
pub fn migrate_v1(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN cwd TEXT NOT NULL DEFAULT '.'")
        .change_context(SchemaMigrationError)
        .attach("v1: add cwd column to sessions")?;
    Ok(())
}

/// v2: Add `created_at` column to sessions.
pub fn migrate_v2(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN created_at TEXT NOT NULL DEFAULT ''")
        .change_context(SchemaMigrationError)
        .attach("v2: add created_at column to sessions")?;
    Ok(())
}

/// v3: Add `ignored` column to session_entries.
///
/// Compaction marks entries as ignored when they've been summarized.
/// Default is `false` (entry is active and visible during prompt assembly).
pub fn migrate_v3(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "ALTER TABLE session_entries ADD COLUMN ignored BOOLEAN NOT NULL DEFAULT FALSE",
    )
    .change_context(SchemaMigrationError)
    .attach("v3: add ignored column to session_entries")?;
    Ok(())
}

/// v4: Add `cost` column to token_ledger.
///
/// Tracks per-request cost in USD as reported by the provider (e.g. OpenRouter).
pub fn migrate_v4(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE token_ledger ADD COLUMN cost DOUBLE")
        .change_context(SchemaMigrationError)
        .attach("v4: add cost column to token_ledger")?;
    Ok(())
}

/// v5: Add `lifecycle_name` and `lifecycle_args` columns to sessions.
///
/// `lifecycle_name` is NULL for sessions created without a lifecycle.
/// `lifecycle_args` is a JSON array of strings, defaulting to empty.
pub fn migrate_v5(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN lifecycle_name TEXT DEFAULT NULL")
        .change_context(SchemaMigrationError)
        .attach("v5: add lifecycle_name column to sessions")?;
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN lifecycle_args TEXT NOT NULL DEFAULT '[]'")
        .change_context(SchemaMigrationError)
        .attach("v5: add lifecycle_args column to sessions")?;
    Ok(())
}

/// v6: Add `archived` column to sessions.
///
/// Sessions default to unarchived. Closing a session sets `archived = TRUE`.
/// On startup, only unarchived sessions are loaded into memory.
#[expect(clippy::expect_used, reason = "infallible")]
pub fn migrate_v6(conn: &mut rusqlite::Connection) {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN archived BOOLEAN NOT NULL DEFAULT FALSE")
        .expect("v6: add archived column to sessions");
}

/// v7: Add `lifecycle_script_state` column to sessions.
///
/// Persists the `LifecycleScriptState` enum so teardown runs correctly
/// after app restart for sessions that had setup run.
/// Default is `'nothing_ran'` - matching the enum's default.
pub fn migrate_v7(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN lifecycle_script_state TEXT NOT NULL DEFAULT 'nothing_ran'",
    )
    .change_context(SchemaMigrationError)
    .attach("v7: add lifecycle_script_state column to sessions")?;
    Ok(())
}

/// v8: Add `metadata` column to sessions.
///
/// Stores a JSON blob of all session metadata. This eliminates the need
/// for individual columns per field - new fields on `SessionCore` are
/// automatically persisted via serde.
pub fn migrate_v8(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN metadata TEXT")
        .change_context(SchemaMigrationError)
        .attach("v8: add metadata column to sessions")?;
    Ok(())
}

/// v9: Rename `session_entries` table to `session_history`.
///
/// The old name was ambiguous - it sounded like a table of sessions.
/// The new name makes it clear this is the chat history junction table.
pub fn migrate_v9(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE session_entries RENAME TO session_history")
        .change_context(SchemaMigrationError)
        .attach("v9: rename session_entries to session_history")?;
    Ok(())
}

/// v10: Consolidate all sessions to compaction-only strategy.
///
/// Rewrites `strategy_state` JSON to remove non-compaction keys
/// and sets `profile.strategy` to `"compaction"` for all sessions.
/// This prepares the database for the removal of other strategy types
/// from the Rust codebase.
///
/// # Errors
///
/// Returns an error if the rows cannot be read or the rewritten JSON
/// cannot be written back.
pub fn migrate_v10(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    let rows = query_map_rows(
        conn,
        "SELECT rowid, strategy_state, profile FROM sessions",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
        "v10",
    )?;

    for (rowid, strategy_state, profile) in rows {
        let new_strategy_state = rewrite_strategy_state(&strategy_state);
        let new_profile = rewrite_profile_strategy(&profile);

        conn.execute(
            "UPDATE sessions SET strategy_state = ?, profile = ? WHERE rowid = ?",
            params![new_strategy_state, new_profile, rowid],
        )
        .change_context(SchemaMigrationError)
        .attach("v10: update session row")?;
    }

    Ok(())
}

/// Rewrites strategy_state JSON, keeping only the "compaction" key.
///
/// If no compaction key exists, inserts a default.
/// Leaves unparseable JSON unchanged.
fn rewrite_strategy_state(raw: &str) -> String {
    let mut map: HashMap<String, serde_json::Value> = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(_) => return raw.to_owned(),
    };

    // Retain only the compaction key.
    map.retain(|k, _| k == "compaction");

    // If compaction key is missing, insert default.
    if !map.contains_key("compaction") {
        map.insert(
            "compaction".to_owned(),
            serde_json::json!({"compaction": {"compaction_count": 0}}),
        );
    }

    serde_json::to_string(&map).unwrap_or_else(|_| raw.to_owned())
}

/// Rewrites profile JSON, setting strategy to "compaction".
///
/// Leaves unparseable JSON unchanged.
fn rewrite_profile_strategy(raw: &str) -> String {
    let mut map: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(raw) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => return raw.to_owned(),
    };

    map.insert(
        "strategy".to_owned(),
        serde_json::Value::String("compaction".to_owned()),
    );

    serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| raw.to_owned())
}

/// Rewrites profile JSON `model` field from bare string to tagged object.
///
/// Before: `"model": "ollama/llama3"`
/// After:  `"model": {"single": "ollama/llama3"}`
///
/// If the `model` field is already an object (already migrated), leaves it unchanged.
/// If the profile JSON is unparseable, leaves it unchanged.
fn rewrite_profile_model(raw: &str) -> String {
    let mut map: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(raw) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => return raw.to_owned(),
    };

    // Only rewrite if `model` is a bare string.
    let Some(model_value) = map.get("model") else {
        return raw.to_owned();
    };
    if let serde_json::Value::String(s) = model_value {
        map.insert("model".to_owned(), serde_json::json!({"single": s}));
    }
    // If model is already an object or missing, leave it alone.

    serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| raw.to_owned())
}

/// Rewrites a bare-string `profile.model` inside a `metadata` JSON blob.
///
/// v19's analog of [`rewrite_profile_model`], operating one level deeper:
/// the metadata blob embeds a `profile` sub-object, whose `model` field
/// needs the same bare-string -> `{"single": ...}` transformation. This
/// extracts the profile sub-object, hands it to [`rewrite_profile_model`],
/// and re-inserts the result. Idempotent and non-destructive: returns the
/// input unchanged on any parse failure, missing `profile` key,
/// non-object `profile`, or when `model` is already an object/missing.
fn rewrite_metadata_blob_model(raw_metadata: &str) -> String {
    let mut root: serde_json::Map<String, serde_json::Value> =
        match serde_json::from_str(raw_metadata) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => return raw_metadata.to_owned(),
        };

    let Some(profile_value) = root.get("profile") else {
        return raw_metadata.to_owned();
    };
    let serde_json::Value::Object(_) = profile_value else {
        // `profile` present but not an object - malformed; leave it alone.
        return raw_metadata.to_owned();
    };

    // Serialize the profile sub-object, run the model rewriter, parse back.
    let Ok(profile_str) = serde_json::to_string(profile_value) else {
        return raw_metadata.to_owned();
    };
    let new_profile_str = rewrite_profile_model(&profile_str);
    let new_profile: serde_json::Value = match serde_json::from_str(&new_profile_str) {
        Ok(v) => v,
        Err(_) => return raw_metadata.to_owned(),
    };
    root.insert("profile".to_owned(), new_profile);

    serde_json::to_string(&serde_json::Value::Object(root))
        .unwrap_or_else(|_| raw_metadata.to_owned())
}

/// v11: Add `is_workflow` column to sessions.
///
/// Marks sessions created by workflow LLM nodes. Enables filtering
/// workflow sessions in the sidebar and elsewhere.
/// Default is `false` - regular chat sessions are not workflow sessions.
pub fn migrate_v11(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN is_workflow BOOLEAN NOT NULL DEFAULT FALSE",
    )
    .change_context(SchemaMigrationError)
    .attach("v11: add is_workflow column to sessions")?;
    Ok(())
}

/// v12: Add `context_override` column to session_history.
///
/// Replaces the boolean `ignored` column with a tri-state text column:
/// `'default'`, `'forced_include'`, `'forced_exclude'`. The old `ignored`
/// column is kept for backward compatibility - the new column takes precedence.
/// Rows with `ignored = 1` are migrated to `'forced_exclude'`.
pub fn migrate_v12(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "ALTER TABLE session_history ADD COLUMN context_override TEXT NOT NULL DEFAULT 'default'",
    )
    .change_context(SchemaMigrationError)
    .attach("v12: add context_override column to session_history")?;
    conn.execute_batch(
        "UPDATE session_history SET context_override = 'forced_exclude' WHERE ignored = 1",
    )
    .change_context(SchemaMigrationError)
    .attach("v12: migrate ignored values to context_override")?;
    Ok(())
}

/// v13: Add `judge_meta` column to sessions.
///
/// Stores judge metadata as a nullable JSON text blob.
/// When NULL, the session is not a judge session.
pub fn migrate_v13(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN judge_meta TEXT")
        .change_context(SchemaMigrationError)
        .attach("v13: add judge_meta column to sessions")?;
    Ok(())
}

/// v14: Add `context_history` column to entries.
///
/// Stores the audit trail of context inclusion/exclusion changes as a JSON array
/// of `ContextChangeEvent`. Defaults to `'[]'` (empty audit) for existing rows.
pub fn migrate_v14(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE entries ADD COLUMN context_history TEXT NOT NULL DEFAULT '[]'")
        .change_context(SchemaMigrationError)
        .attach("v14: add context_history column to entries")?;
    Ok(())
}

/// Drops the now-unused `strategy_state` column from `sessions`.
/// Context-management strategies were replaced by the history-worker architecture
/// (auto-prune and compaction workers derive state at runtime). The column feeds no
/// code path and is removed to align the schema with the Rust types.
///
/// Implemented via the SQLite-recommended 12-step table rebuild rather than
/// `ALTER TABLE ... DROP COLUMN`, because the latter requires SQLite >= 3.35 and
/// the linked (system) SQLite version is not pinned. The rebuild works on any version.
///
/// # Errors
///
/// Returns an error if any step of the table rebuild fails.
pub fn migrate_v15(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE sessions_new (\
         id TEXT PRIMARY KEY,\
         title TEXT,\
         updated_at TEXT NOT NULL,\
         profile TEXT NOT NULL DEFAULT '{}',\
         blobs TEXT NOT NULL DEFAULT '{}',\
         parent_session TEXT DEFAULT NULL,\
         cwd TEXT NOT NULL DEFAULT '.',\
         created_at TEXT NOT NULL DEFAULT '',\
         archived BOOLEAN NOT NULL DEFAULT FALSE,\
         lifecycle_name TEXT DEFAULT NULL,\
         lifecycle_args TEXT NOT NULL DEFAULT '[]',\
         lifecycle_script_state TEXT NOT NULL DEFAULT 'nothing_ran',\
         metadata TEXT,\
         is_workflow BOOLEAN NOT NULL DEFAULT FALSE,\
         judge_meta TEXT)",
    )
    .change_context(SchemaMigrationError)
    .attach("v15: create sessions_new without strategy_state")?;

    conn.execute_batch(
        "INSERT INTO sessions_new (\
         id, title, updated_at, profile, blobs, parent_session, cwd, created_at,\
         archived, lifecycle_name, lifecycle_args, lifecycle_script_state, metadata,\
         is_workflow, judge_meta) \
         SELECT \
         id, title, updated_at, profile, blobs, parent_session, cwd, created_at,\
         archived, lifecycle_name, lifecycle_args, lifecycle_script_state, metadata,\
         is_workflow, judge_meta FROM sessions",
    )
    .change_context(SchemaMigrationError)
    .attach("v15: copy sessions into sessions_new")?;

    conn.execute_batch("DROP TABLE sessions")
        .change_context(SchemaMigrationError)
        .attach("v15: drop old sessions table")?;

    conn.execute_batch("ALTER TABLE sessions_new RENAME TO sessions")
        .change_context(SchemaMigrationError)
        .attach("v15: rename sessions_new to sessions")?;

    Ok(())
}

/// v16: Rebuild `sessions` with `is_automated` + `persist`, replacing
/// `is_workflow`.
///
/// # Errors
///
/// Returns an error if any step of the table rebuild fails.
pub fn migrate_v16(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE sessions_new (\
         id TEXT PRIMARY KEY,\
         title TEXT,\
         updated_at TEXT NOT NULL,\
         profile TEXT NOT NULL DEFAULT '{}',\
         blobs TEXT NOT NULL DEFAULT '{}',\
         parent_session TEXT DEFAULT NULL,\
         cwd TEXT NOT NULL DEFAULT '.',\
         created_at TEXT NOT NULL DEFAULT '',\
         archived BOOLEAN NOT NULL DEFAULT FALSE,\
         lifecycle_name TEXT DEFAULT NULL,\
         lifecycle_args TEXT NOT NULL DEFAULT '[]',\
         lifecycle_script_state TEXT NOT NULL DEFAULT 'nothing_ran',\
         metadata TEXT,\
         is_automated BOOLEAN NOT NULL DEFAULT FALSE,\
         persist BOOLEAN NOT NULL DEFAULT TRUE,\
         judge_meta TEXT)",
    )
    .change_context(SchemaMigrationError)
    .attach("v16: create sessions_new with is_automated + persist")?;

    conn.execute_batch(
        "INSERT INTO sessions_new (\
         id, title, updated_at, profile, blobs, parent_session, cwd, created_at,\
         archived, lifecycle_name, lifecycle_args, lifecycle_script_state, metadata,\
         is_automated, persist, judge_meta) \
         SELECT \
         id, title, updated_at, profile, blobs, parent_session, cwd, created_at,\
         archived, lifecycle_name, lifecycle_args, lifecycle_script_state, metadata,\
         is_workflow, TRUE, judge_meta FROM sessions",
    )
    .change_context(SchemaMigrationError)
    .attach("v16: copy sessions (is_workflow→is_automated, persist=TRUE)")?;

    conn.execute_batch("DROP TABLE sessions")
        .change_context(SchemaMigrationError)
        .attach("v16: drop old sessions table")?;

    conn.execute_batch("ALTER TABLE sessions_new RENAME TO sessions")
        .change_context(SchemaMigrationError)
        .attach("v16: rename sessions_new to sessions")?;

    Ok(())
}

/// v17: Rewrite profile JSON `model` field from bare string to tagged object,
/// and add `model_used` column to `token_ledger`.
///
/// Before: `"model": "ollama/llama3"`
/// After:  `"model": {"single": "ollama/llama3"}`
///
/// This enables the `ModelSelection` enum to deserialize correctly.
/// Unparseable JSON is left unchanged.
///
/// # Errors
///
/// Returns an error if the rows cannot be read, the rewritten JSON cannot
/// be written back, or the new column cannot be added.
pub fn migrate_v17(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    let rows = query_map_rows(
        conn,
        "SELECT rowid, profile FROM sessions",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        "v17",
    )?;

    for (rowid, profile) in rows {
        let new_profile = rewrite_profile_model(&profile);

        conn.execute(
            "UPDATE sessions SET profile = ? WHERE rowid = ?",
            params![new_profile, rowid],
        )
        .change_context(SchemaMigrationError)
        .attach("v17: update session profile")?;
    }

    // Idempotent: ignore "duplicate column" error if migration runs twice.
    match conn.execute("ALTER TABLE token_ledger ADD COLUMN model_used TEXT", []) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") => {}
        Err(e) => {
            return Err(e)
                .change_context(SchemaMigrationError)
                .attach("v17: add model_used column to token_ledger");
        }
    }

    Ok(())
}

/// v18: Rename `entries.timestamp` column to `timing`.
///
/// The column now stores `EntryTiming` JSON (instant or streamed timing data)
/// rather than a plain timestamp string. The rename aligns the column name
/// with the Rust field it maps to.
///
/// # Errors
///
/// Returns an error if the column cannot be renamed.
pub fn migrate_v18(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE entries RENAME COLUMN timestamp TO timing")
        .change_context(SchemaMigrationError)
        .attach("v18: rename entries.timestamp to timing")?;
    Ok(())
}

/// v19: Rewrite a bare-string `profile.model` inside the `metadata` blob.
///
/// Migration v17 rewrote the `profile` column from bare-string model to
/// `{"single": ...}`, but sessions written at v8+ carry their profile
/// embedded in the `metadata` blob (`PersistableCore`). The load path
/// treats the blob as authoritative - so a 0.65 blob (bare-string model)
/// fails to deserialize into `ModelSelection` and the session silently
/// drops out of the sidebar. This migration reaches into each blob's
/// embedded `profile` sub-object and applies the same rewrite v17 applied
/// to the column. Rows with `NULL` metadata (pre-v8 sessions) are skipped;
/// their column data was already fixed by v17.
///
/// # Errors
///
/// Returns an error if the rows cannot be read or the rewritten blobs
/// cannot be written back.
pub fn migrate_v19(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    let rows = query_map_rows(
        conn,
        "SELECT rowid, metadata FROM sessions WHERE metadata IS NOT NULL",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        "v19",
    )?;

    for (rowid, metadata) in rows {
        let new_metadata = rewrite_metadata_blob_model(&metadata);

        // Only UPDATE rows where the blob actually changed.
        if new_metadata == metadata {
            continue;
        }

        conn.execute(
            "UPDATE sessions SET metadata = ? WHERE rowid = ?",
            params![new_metadata, rowid],
        )
        .change_context(SchemaMigrationError)
        .attach("v19: update session metadata blob")?;
    }

    Ok(())
}

/// v20: Retire the six "zombie" `sessions` columns.
///
/// Columns `profile, blobs, cwd, lifecycle_name, lifecycle_args,
/// lifecycle_script_state` are never written by the app (writes go through
/// the `metadata` blob via `PersistableCore`). They survive only as a legacy
/// load fallback for pre-v8 rows whose `metadata` is `NULL`. This migration
/// backfills `metadata` for every NULL row by reconstructing the
/// `PersistableCoreV20` snapshot from the columns, then rebuilds `sessions`
/// without them. After v20, every row has a metadata blob and the columns are gone.
///
/// `judge_meta` (vestigial — referenced only by an orphaned doc comment) is
/// dropped in the same rebuild.
pub fn migrate_v20(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    backfill_missing_metadata(conn)?;
    rebuild_sessions_without_zombies(conn)?;
    Ok(())
}

/// Reconstructs a `metadata` blob for every row where it is `NULL`.
///
/// Mirrors the pre-v20 legacy load branch: deserialize
/// `profile`/`blobs`/`lifecycle_args`/`lifecycle_script_state`
/// from their column JSON, parse `updated_at`/`created_at`/`parent_session`,
/// and serialize a full [`crate::PersistableCoreV20`].
fn backfill_missing_metadata(
    conn: &mut rusqlite::Connection,
) -> Result<(), Report<SchemaMigrationError>> {
    let rows = query_map_rows(
        conn,
        "SELECT rowid, id, title, updated_at, created_at, parent_session, \
                profile, blobs, cwd, lifecycle_name, lifecycle_args, \
                lifecycle_script_state \
         FROM sessions WHERE metadata IS NULL",
        LegacyRow::from_row,
        "v20-backfill",
    )?;

    for row in rows {
        let rowid = row.rowid;
        let blob = row.metadata_blob()?;
        conn.execute(
            "UPDATE sessions SET metadata = ? WHERE rowid = ?",
            params![blob, rowid],
        )
        .change_context(SchemaMigrationError)
        .attach("v20: backfill metadata")?;
    }

    Ok(())
}

/// 12-step SQLite table rebuild dropping the zombie + `judge_meta` columns.
///
/// `sessions` has no indexes beyond its implicit PK, so none need recreating.
fn rebuild_sessions_without_zombies(
    conn: &mut rusqlite::Connection,
) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE sessions_new (\
         id TEXT PRIMARY KEY,\
         title TEXT,\
         updated_at TEXT NOT NULL,\
         created_at TEXT NOT NULL,\
         parent_session TEXT DEFAULT NULL,\
         archived BOOLEAN NOT NULL DEFAULT FALSE,\
         metadata TEXT,\
         is_automated BOOLEAN NOT NULL DEFAULT FALSE,\
         persist BOOLEAN NOT NULL DEFAULT TRUE)",
    )
    .change_context(SchemaMigrationError)
    .attach("v20: create sessions_new without zombie columns")?;

    conn.execute_batch(
        "INSERT INTO sessions_new (\
         id, title, updated_at, created_at, parent_session, archived, metadata,\
         is_automated, persist) \
         SELECT id, title, updated_at, created_at, parent_session, archived, metadata,\
         is_automated, persist FROM sessions",
    )
    .change_context(SchemaMigrationError)
    .attach("v20: copy sessions into sessions_new")?;

    conn.execute_batch("DROP TABLE sessions")
        .change_context(SchemaMigrationError)
        .attach("v20: drop old sessions table")?;

    conn.execute_batch("ALTER TABLE sessions_new RENAME TO sessions")
        .change_context(SchemaMigrationError)
        .attach("v20: rename sessions_new to sessions")?;

    Ok(())
}

/// v21: Add the `discord_thread` mapping table.
///
/// The Discord bot frontend persists a 1:1 mapping between a Discord forum
/// thread and a jinn session id, so a thread can resume its session across
/// bot restarts (and auto-un-archive on inbound message). This table is
/// owned entirely by the discord layer; its columns are independent of the
/// session schema.
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn migrate_v21(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE discord_thread (\
         thread_id  TEXT PRIMARY KEY,\
         session_id TEXT NOT NULL,\
         guild_id   TEXT,\
         created_at INTEGER NOT NULL)",
    )
    .change_context(SchemaMigrationError)
    .attach("v21: create discord_thread mapping table")?;

    Ok(())
}

/// v22: Add the `entry_blobs` table for multimodal attachments.
///
/// Each row stores one attachment blob (raw bytes + media type) keyed by
/// `(entry_id, ordinal)`, where `ordinal` is the position of the attachment
/// within its parent entry. The `entries.kind` JSON no longer needs to carry
/// base64 image data, keeping the JSON column lean. Rows cascade with the
/// parent entry via `ON DELETE CASCADE`.
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn migrate_v22(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE entry_blobs (\
         entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,\
         ordinal INTEGER NOT NULL,\
         media_type TEXT NOT NULL,\
         data BLOB NOT NULL,\
         PRIMARY KEY (entry_id, ordinal))",
    )
    .change_context(SchemaMigrationError)
    .attach("v22: create entry_blobs table")?;

    Ok(())
}

/// v23: Strip the legacy `s-` prefix from session-id strings.
///
/// Historically [`SessionId`] stored its UUID with an `s-` prefix in
/// `Display` (e.g. `s-0195a3b2-...`). The newtype now exposes the bare
/// UUID and `Serialize`/`Deserialize` already round-trip bare values, so
/// the prefix survives only in data written by older jinn versions. This
/// migration rewrites every persisted session-id location to the bare
/// UUID form so the in-memory representation matches the persisted one.
///
/// Locations rewritten:
/// - `sessions.id` (PK)
/// - `sessions.parent_session` (nullable FK)
/// - `session_history.session_id`
/// - `token_ledger.session_id`
/// - `discord_thread.session_id`
/// - `sessions.metadata` JSON blob keys `session_id` and `parent_session`
///
/// Each column is updated only where its value begins with `s-`
/// (`substr(col, 1, 2) = 's-'`), so already-bare values (and any
/// non-conforming legacy strings) pass through untouched. The blob rewrite
/// is fault-tolerant: an unparseable blob is left unchanged (a migration
/// must never block on data an older system could have produced).
///
/// Runs under `PRAGMA foreign_keys=OFF` (the runner toggles it), so the
/// `sessions.id` rewrite does not fire cascades on the child tables — they
/// are updated explicitly below.
///
/// # Errors
///
/// Returns an error if any rewrite fails. The JSON blob rewrite is
/// fault-tolerant by design: an unparseable blob is left unchanged, never
/// an error.
pub fn migrate_v23(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    // SQL columns — strip the prefix only where present.
    conn.execute_batch("UPDATE sessions SET id = substr(id, 3) WHERE substr(id, 1, 2) = 's-'")
        .change_context(SchemaMigrationError)
        .attach("v23: strip s- prefix from sessions.id")?;

    conn.execute_batch(
        "UPDATE sessions SET parent_session = substr(parent_session, 3) WHERE parent_session IS NOT NULL AND substr(parent_session, 1, 2) = 's-'",
    )
    .change_context(SchemaMigrationError)
    .attach("v23: strip s- prefix from sessions.parent_session")?;

    conn.execute_batch(
        "UPDATE session_history SET session_id = substr(session_id, 3) WHERE substr(session_id, 1, 2) = 's-'",
    )
    .change_context(SchemaMigrationError)
    .attach("v23: strip s- prefix from session_history.session_id")?;

    conn.execute_batch(
        "UPDATE token_ledger SET session_id = substr(session_id, 3) WHERE substr(session_id, 1, 2) = 's-'",
    )
    .change_context(SchemaMigrationError)
    .attach("v23: strip s- prefix from token_ledger.session_id")?;

    conn.execute_batch(
        "UPDATE discord_thread SET session_id = substr(session_id, 3) WHERE substr(session_id, 1, 2) = 's-'",
    )
    .change_context(SchemaMigrationError)
    .attach("v23: strip s- prefix from discord_thread.session_id")?;

    // JSON blob — rewrite the session_id / parent_session keys.
    rewrite_metadata_session_ids(conn)?;

    Ok(())
}

/// v24: Add provider-reported prompt/cache token columns to `token_ledger`.
///
/// Adds `prompt_tokens` (provider-reported prompt count, distinct from the
/// local estimate `tokens_sent`) and `cached_tokens` (cache-hit count from
/// `usage.prompt_tokens_details.cached_tokens`). Both nullable: pre-v24 rows
/// and turns without provider usage (e.g. cancelled) load as `NULL`.
///
/// # Errors
///
/// Returns an error if a column cannot be added (a re-run's duplicate
/// column is tolerated by design).
pub fn migrate_v24(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    for col in ["prompt_tokens", "cached_tokens"] {
        let sql = format!("ALTER TABLE token_ledger ADD COLUMN {col} INTEGER");
        // Idempotent: ignore "duplicate column" error if migration runs twice.
        match conn.execute(&sql, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column name") => {}
            Err(e) => {
                return Err(e)
                    .change_context(SchemaMigrationError)
                    .attach(format!("v24: add {col} column to token_ledger"));
            }
        }
    }
    Ok(())
}

/// v25: Add the persisted per-entry token count column to `entries`.
///
/// Adds `token_count` — the content-derived tiktoken estimate for the entry,
/// stored so large sessions don't re-tokenize their whole history on every
/// load. Nullable: pre-v25 rows load as `NULL` (= "not yet computed") and are
/// filled lazily by the token count actor, then saved on the next persist.
///
/// # Errors
///
/// Returns an error if the column cannot be added (a re-run's duplicate
/// column is tolerated by design).
pub fn migrate_v25(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    let sql = "ALTER TABLE entries ADD COLUMN token_count INTEGER";
    // Idempotent: ignore "duplicate column" error if migration runs twice.
    match conn.execute(sql, []) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") => {}
        Err(e) => {
            return Err(e)
                .change_context(SchemaMigrationError)
                .attach("v25: add token_count column to entries");
        }
    }
    Ok(())
}

/// v26: Add the FTS5 search index for cross-session content search.
///
/// Three objects:
///
/// - `session_fts` — a contentful FTS5 virtual table holding one row per
///   **(session, entry)** pair. The searchable unit exists in no single
///   physical table (entries are shared across forked sessions; the junction
///   is rewritten wholesale on every save), so jinn-kernel's search-index
///   actor recomputes each session's rows from the live tables when the
///   session is marked dirty. `body` is the only indexed column — the
///   entry's searchable prose — while `role`, `session_id`, `entry_id`, and
///   `entry_ts` are `UNINDEXED` filters: bare query terms can never match a
///   role word, and scope/date filtering is a plain `WHERE` post-filter.
///   Stored metadata means `snippet()` works with no join.
/// - `fts_dirty` — the durable dirty-marker set. Triggers on `sessions` (the
///   single once-per-save touchpoint; the junction is rewritten per row and
///   carries no text, so per-row triggers there would storm) insert the
///   session id; a background actor drains the set by full-session recompute.
///   The table doubles as crash-recovery pending work: a crash mid-reindex
///   leaves the marker set, and the next startup's drain retries it.
/// - The triggers themselves — INSERT/UPDATE/DELETE on `sessions`. The DELETE
///   trigger marks the deleted id dirty so the drain's reinsert-from-live
///   finds no live rows and removes any stale FTS rows.
///
/// Every existing session is seeded dirty, so the first post-upgrade launch
/// backfills the index lazily in the background — startup never blocks on
/// parsing the full history. Row bodies are written by jinn-kernel (which owns
/// the `ChatEntryKind` JSON schema); this migration only creates the objects.
///
/// # Errors
///
/// Returns an error if any of the three objects cannot be created or the
/// dirty set cannot be seeded.
pub fn migrate_v26(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE session_fts USING fts5(\
         body,\
         role UNINDEXED,\
         session_id UNINDEXED,\
         entry_id UNINDEXED,\
         entry_ts UNINDEXED,\
         tokenize = 'porter unicode61')",
    )
    .change_context(SchemaMigrationError)
    .attach("v26: create session_fts index table")?;

    conn.execute_batch(
        "CREATE TABLE fts_dirty (\
         session_id TEXT PRIMARY KEY)",
    )
    .change_context(SchemaMigrationError)
    .attach("v26: create fts_dirty marker table")?;

    conn.execute_batch(
        "CREATE TRIGGER sessions_fts_dirty_ins AFTER INSERT ON sessions BEGIN \
         INSERT INTO fts_dirty(session_id) VALUES (NEW.id) ON CONFLICT(session_id) DO NOTHING; END;\
         CREATE TRIGGER sessions_fts_dirty_upd AFTER UPDATE ON sessions BEGIN \
         INSERT INTO fts_dirty(session_id) VALUES (NEW.id) ON CONFLICT(session_id) DO NOTHING; END;\
         CREATE TRIGGER sessions_fts_dirty_del AFTER DELETE ON sessions BEGIN \
         INSERT INTO fts_dirty(session_id) VALUES (OLD.id) ON CONFLICT(session_id) DO NOTHING; END",
    )
    .change_context(SchemaMigrationError)
    .attach("v26: create sessions dirty-marking triggers")?;

    conn.execute_batch("INSERT INTO fts_dirty(session_id) SELECT id FROM sessions")
        .change_context(SchemaMigrationError)
        .attach("v26: seed fts_dirty with existing sessions")?;

    Ok(())
}

/// v27 — add `resume_offset` to `fts_dirty` for chunked reindexing.
///
/// The reindex drain chunks each session's FTS rebuild into bounded
/// transactions (a 40k-entry session must not hold the write lock — or the
/// tick handler — for an unbounded time). This column stores how many of the
/// session's entries have been indexed so far: 0 means "start from scratch"
/// (the next chunk deletes stale FTS rows first); a nonzero value resumes
/// appending after that ordinal prefix. It is cleared to 0 in the same
/// transaction that clears the marker on completion, so a crash mid-rebuild
/// leaves both the marker and its resume point intact for the next drain.
pub fn migrate_v27(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE fts_dirty ADD COLUMN resume_offset INTEGER NOT NULL DEFAULT 0")
        .change_context(SchemaMigrationError)
        .attach("v27: add fts_dirty.resume_offset")?;
    Ok(())
}

/// v28 — add `fts_rowids`, the per-session rowid map over `session_fts`.
///
/// `session_id` is UNINDEXED in the FTS5 table (by design: it is stored as
/// metadata, never matched), so a per-session `DELETE FROM session_fts WHERE
/// session_id = ?` full-scans the whole index — seconds on a large corpus,
/// paid once per dirty session at reindex time. This migration creates a
/// plain side table mapping each FTS row to its session, backfilled from the
/// existing index, so reindexing can delete via primary-key rowids instead.
///
/// The map is a derived index: it carries no data of its own and is
/// maintained by the store's single `session_fts` write path (paired inserts,
/// same transaction). Search SQL is untouched.
pub fn migrate_v28(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE TABLE fts_rowids (\
         session_id TEXT NOT NULL,\
         fts_rowid INTEGER NOT NULL,\
         PRIMARY KEY (session_id, fts_rowid));\
         INSERT INTO fts_rowids (session_id, fts_rowid)\
         SELECT session_id, rowid FROM session_fts",
    )
    .change_context(SchemaMigrationError)
    .attach("v28: create fts_rowids and backfill from session_fts")?;
    Ok(())
}

/// v29: Add the per-entry model attribution columns to `entries`.
///
/// Two nullable columns: `model_used` — the concrete model that produced the
/// entry (response-reported when the stream completed normally, requested
/// otherwise) — and `model_requested` — the model asked for at dispatch,
/// never overridden. Both `NULL` for pre-v29 rows, permanently: an old
/// entry's model is a fact nobody recorded, and inventing one would be a
/// lie.
pub fn migrate_v29(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    // Idempotent: ignore "duplicate column" error if migration runs twice.
    for (column, decl) in [("model_used", "TEXT"), ("model_requested", "TEXT")] {
        let sql = format!("ALTER TABLE entries ADD COLUMN {column} {decl}");
        match conn.execute(&sql, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column name") => {}
            Err(e) => {
                return Err(e)
                    .change_context(SchemaMigrationError)
                    .attach(format!("v29: add {column} column to entries"));
            }
        }
    }
    Ok(())
}

/// v30: Add the `in_library` column to `sessions`.
///
/// Library membership is a durable per-session flag, orthogonal to `archived`:
/// a library'd session stays listed in the sidebar's Library section whether
/// loaded or archived, and disappears from the Sessions list. Every session
/// defaults to not-a-member, which is exactly the pre-migration behavior.
pub fn migrate_v30(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN in_library BOOLEAN NOT NULL DEFAULT FALSE")
        .change_context(SchemaMigrationError)
        .attach("v30: add in_library column to sessions")?;
    Ok(())
}

/// v31: Add the `is_bot` column to `sessions`.
///
/// A scalar projection of the metadata blob's `origin == Bot`, read by the
/// library-members query. That query deliberately skips the blob (it was the
/// perf cost of every membership write), but the Library section tints bot
/// members with the bots color the sessions tree uses — a fact it cannot get
/// without either parsing the blob per row or persisting the one bit it
/// needs. `origin` is immutable after creation, so the column never drifts
/// from the blob it projects.
///
/// # Errors
///
/// Returns an error if the column cannot be added.
pub fn migrate_v31(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch("ALTER TABLE sessions ADD COLUMN is_bot BOOLEAN NOT NULL DEFAULT FALSE")
        .change_context(SchemaMigrationError)
        .attach("v31: add is_bot column to sessions")?;
    Ok(())
}

/// v32: Convert the database to incremental auto-vacuum.
///
/// **The real work does not run here.** `PRAGMA auto_vacuum` is a no-op
/// outside an empty database unless followed by a full `VACUUM`, and SQLite
/// forbids `VACUUM` inside a transaction — while [`run_pending`] runs the
/// chain in transactions around this version. This chain entry is therefore
/// a documented no-op that exists to keep the version numbering monotonic
/// and recorded like any other migration. The conversion itself executes in
/// [`run_pending`]'s phase 2 — at this migration's natural position, outside
/// any transaction, gated on `current < 32` so it runs exactly once.
///
/// Incremental auto-vacuum is what lets the session prune actor hand
/// freed pages back to the filesystem (`PRAGMA incremental_vacuum`) after
/// prune sweeps, instead of leaving them on a freelist forever.
///
/// # Errors
///
/// Always succeeds: the real conversion runs in [`run_pending`]'s phase 2
/// (see [`convert_to_incremental_auto_vacuum`]), outside any transaction.
pub fn migrate_v32(_conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    Ok(())
}

/// v33: Index `session_history(entry_id)`.
///
/// Entry deletion cascades by *entry*: the bulk delete's `DELETE FROM entries
/// WHERE id IN (…)` fires the `entry_id` FK cascade on every row, and the
/// orphan-reaping lookup (`WHERE entry_id IN (…)`) scans the junction table
/// directly. Both are full scans of `session_history` without this index —
/// millions of rows deep on large installs, which turned every prune chunk
/// (and before the chunked drain, the whole prune) into a multi-minute
/// write-lock hold and wedged shutdown past the graceful deadline.
/// `session_id`, the other junction column, has always been indexed via
/// `idx_session_entries_session`.
///
/// # Errors
///
/// Returns an error if the index cannot be created.
pub fn migrate_v33(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_session_history_entry ON session_history(entry_id)",
    )
    .change_context(SchemaMigrationError)
    .attach("v33: create idx_session_history_entry on session_history(entry_id)")?;
    Ok(())
}

/// The body of migration v32: flip the database to incremental auto-vacuum
/// and pay for it with a full `VACUUM`.
///
/// Must run on a connection with **no open transaction** (SQLite forbids
/// `VACUUM` inside one) — [`run_pending`] calls it in phase 2, between the
/// two chain transactions, gated on `current < 32`. The conversion is
/// persistent in the database header, and the pool builder never touches
/// `auto_vacuum`, so every later connection inherits it.
///
/// `VACUUM` is atomic: an interrupt (Ctrl+C / SIGKILL) mid-vacuum rolls back
/// to the original file, `_migrations` never records v32, and the next
/// startup redoes the work. A vacuum *failure* propagates like any failed
/// migration, leaving the database at v31 for a retry.
///
/// # Errors
///
/// Returns an error if the pragma cannot be set or the `VACUUM` fails (the
/// database is then left at v31 for a retry).
#[expect(
    clippy::print_stderr,
    reason = "pre-TUI user-facing warning for a long-running rewrite: migrations run before any tracing subscriber exists"
)]
fn convert_to_incremental_auto_vacuum(
    conn: &mut rusqlite::Connection,
) -> Result<(), Report<SchemaMigrationError>> {
    eprintln!(
        "jinn: this migration rewrites the entire database and can take 10+ minutes on large installations"
    );
    conn.pragma_update(None, "auto_vacuum", "incremental")
        .change_context(SchemaMigrationError)
        .attach("v32: set auto_vacuum=incremental")?;
    conn.execute_batch("VACUUM")
        .change_context(SchemaMigrationError)
        .attach("v32: vacuum to apply the auto_vacuum conversion")?;
    Ok(())
}

/// Rewrites the `session_id` and `parent_session` string values inside the
/// `sessions.metadata` JSON blob, stripping a leading `s-` where present.
///
/// Fault-tolerant: rows whose `metadata` is NULL, empty, unparseable as an
/// object, or whose targeted keys are not strings are left unchanged.
fn rewrite_metadata_session_ids(
    conn: &mut rusqlite::Connection,
) -> Result<(), Report<SchemaMigrationError>> {
    let mut stmt = conn
        .prepare("SELECT rowid, metadata FROM sessions WHERE metadata IS NOT NULL")
        .change_context(SchemaMigrationError)
        .attach("v23: prepare metadata scan")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .change_context(SchemaMigrationError)
        .attach("v23: scan metadata rows")?;
    for row in rows {
        let (rowid, raw) = row
            .change_context(SchemaMigrationError)
            .attach("v23: read metadata row")?;
        let rewritten = strip_prefix_in_blob(&raw);
        if rewritten != raw {
            conn.execute(
                "UPDATE sessions SET metadata = ? WHERE rowid = ?",
                rusqlite::params![rewritten, rowid],
            )
            .change_context(SchemaMigrationError)
            .attach("v23: rewrite metadata blob")?;
        }
    }
    Ok(())
}

/// Strips a leading `s-` from the `session_id` and `parent_session` string
/// values inside a `metadata` JSON object. Returns the input verbatim if the
/// JSON is unparseable, not an object, or the keys are absent/non-string.
fn strip_prefix_in_blob(raw: &str) -> String {
    let mut map: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(raw) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => return raw.to_owned(),
    };
    let mut changed = false;
    for key in ["session_id", "parent_session"] {
        if let Some(serde_json::Value::String(s)) = map.get_mut(key)
            && let Some(rest) = s.strip_prefix('s').and_then(|t| t.strip_prefix('-'))
        {
            *s = rest.to_owned();
            changed = true;
        }
    }
    if changed {
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| raw.to_owned())
    } else {
        raw.to_owned()
    }
}

/// A legacy `sessions` row whose `metadata` is NULL — the pre-v8 shape that
/// the backfill reconstructs a [`crate::PersistableCoreV20`] blob from.
struct LegacyRow {
    /// SQLite rowid — the stable key for the subsequent UPDATE.
    rowid: i64,
    /// The pre-v8 column values, verbatim.
    columns: LegacySessionColumns,
}

impl LegacyRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: row.get(0)?,
            columns: LegacySessionColumns {
                session_id: row.get(1)?,
                title: row.get(2)?,
                updated_at: row.get(3)?,
                created_at: row.get(4)?,
                parent_session: row.get(5)?,
                profile: row.get(6)?,
                blobs: row.get(7)?,
                cwd: row.get(8)?,
                lifecycle_name: row.get(9)?,
                lifecycle_args: row.get(10)?,
                lifecycle_script_state: row.get(11)?,
            },
        })
    }

    /// Reconstructs the [`crate::PersistableCoreV20`] blob from the legacy columns,
    /// exactly mirroring the pre-v20 legacy load branch.
    ///
    /// # Errors
    ///
    /// Returns an error if any column JSON fails to deserialize or the
    /// reconstructed blob cannot be serialized.
    fn metadata_blob(&self) -> Result<String, Report<SchemaMigrationError>> {
        crate::PersistableCoreV20::blob_from_legacy_columns(&self.columns)
    }
}

/// Runs `sql`, mapping each row via `map`. The statement borrow is released
/// before this returns, so the caller is free to mutate the connection.
///
/// Centralizes the prepare → query_map → collect pattern used by every
/// row-by-row JSON-surgery migration. The `tag` is attached to every error
/// for context.
fn query_map_rows<T, F>(
    conn: &mut rusqlite::Connection,
    sql: &str,
    map: F,
    tag: &str,
) -> Result<Vec<T>, Report<SchemaMigrationError>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut stmt = conn
        .prepare(sql)
        .change_context(SchemaMigrationError)
        .attach(format!("{tag}: prepare statement"))?;
    let mapped = stmt
        .query_map([], map)
        .change_context(SchemaMigrationError)
        .attach(format!("{tag}: map rows"))?;
    let mut rows = Vec::new();
    for row in mapped {
        rows.push(
            row.change_context(SchemaMigrationError)
                .attach(format!("{tag}: collect row"))?,
        );
    }
    Ok(rows)
}

// ── Test helpers ─────────────────────────────────────────────────────────
//
// `apply_migrations_inner` mirrors jinn-kernel's test-only helper of the same
// name so the schema crate's own tests and (transitively) jinn-kernel's tests
// can stand a DB up at any schema version. It drives [`MIGRATIONS`] directly,
// so seeding and production application can never drift.

/// Test helper driving [`MIGRATIONS`] directly: applies every migration up
/// to and including `target`, recording each version.
///
/// Mirrors jinn-kernel's test-only helper of the same name so the schema
/// crate's own tests and (transitively) jinn-kernel's tests can stand a DB
/// up at any schema version. Seeding and production application can never
/// drift, because both drive the same chain.
///
/// # Panics
///
/// Panics if a migration fails or a version record cannot be written —
/// test seeding has no recovery path, and a failure here invalidates the
/// whole fixture.
#[cfg(feature = "testing")]
#[expect(
    clippy::panic,
    clippy::expect_used,
    reason = "test fixture: panic is the failure channel, there is no recovery path"
)]
pub fn apply_migrations_inner(conn: &mut rusqlite::Connection, target: i32) {
    for migration in MIGRATIONS {
        if target >= migration.version {
            (migration.apply)(conn)
                .unwrap_or_else(|e| panic!("migration v{} failed: {e:?}", migration.version));
            record_version(conn, migration.version, migration.name).expect("record version");
        }
    }
}

/// Test-only re-exports of individual migrations and helpers.
///
/// Downstream test suites (e.g. jinn-kernel's migrator tests) need to stand a
/// DB up at a specific version and assert against a single migration's
#[cfg(feature = "testing")]
pub mod testing {
    pub use super::{
        CHAIN_LATEST_VERSION, LATEST_VERSION, MIGRATIONS, announcement_line,
        apply_migrations_inner, bootstrap_tracking_table, migrate_v10, migrate_v15, migrate_v16,
        migrate_v17, migrate_v18, migrate_v19, migrate_v21, migrate_v22, migrate_v23, migrate_v25,
        migrate_v26, migrate_v31, migrate_v32, migrate_v33, record_version,
    };

    /// The debug-build integrity-walk announcement, re-exported for the
    /// format test. Present in debug builds only, matching the walk itself.
    #[cfg(debug_assertions)]
    pub use crate::debug_fk_check_line;
}
