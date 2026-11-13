//! Standalone schema + migration crate for jinn's session store.
//!
//! This is the **single source of truth** for the SQLite schema. It is consumed
//! in two places, both of which must see the same schema:
//!
//! - **Build time:** `jinn-kernel/build.rs` calls [`run_migrations`] on a fresh
//!   `OUT_DIR` database, then points `dao`'s `#[dao]` macro at it via
//!   `DAOW_DATABASE_URL` for compile-time SQL validation.
//! - **Runtime:** jinn's session store calls [`run_migrations`] through a
//!   `dao::Pool::with_conn` closure on the user's `sessions.db` at startup.
//!
//! Living in its own crate (a leaf — no dependency on `jinn-kernel`) is what
//! breaks the bootstrap cycle: `build.rs` can depend on this crate via
//! `[build-dependencies]`, whereas it cannot depend on the crate it belongs to.
//!
//! See the `dao` crate's README ("The schema-crate pattern") for the rationale.

mod legacy;
mod migrate;

pub use legacy::PersistableCoreV20;

#[cfg(feature = "testing")]
pub use migrate::testing;

/// The highest migration version this runner knows how to apply.
///
/// Re-exported from the migrate module for test suites that assert upgrade
/// postconditions against the published latest version.
pub use migrate::LATEST_VERSION;

use error_stack::{Report, ResultExt as _};
use wherror::Error;

/// Runs all pending schema migrations on the given connection.
///
/// Migrations apply in chain order as three phases (see
/// [`migrate::run_pending`]): everything below the v32 vacuum migration
/// commits in one `BEGIN IMMEDIATE … COMMIT` transaction, the vacuum
/// migration itself runs between the transactions (its `VACUUM` cannot run
/// inside one) and records its version in its own tiny transaction, and
/// everything above it commits in a second transaction. A failure or an
/// interrupt (e.g. Ctrl+C / SIGKILL) inside either chain transaction rolls
/// that transaction back to where the previous phase left off, leaving no
/// half-applied schema. The database therefore rests in one of three states:
/// at the old version, at exactly v32 (its rewrite is durable on its own),
/// or at latest.
///
/// The split's one accepted crash window sits between the v32 rewrite and
/// its record commit: a kill there re-runs the (atomic, idempotent) vacuum
/// once on the next startup. In exchange, a failure at v33 or later no
/// longer discards a completed conversion — the old single-transaction
/// runner rolled everything back while the vacuum's file rewrite persisted,
/// forcing a pointless re-vacuum on restart.
///
/// The FK toggle below (`foreign_keys=OFF` … `ON`) wraps both transactions.
/// The pragmas stay *outside* `run_pending`'s transactions because SQLite
/// forbids changing `foreign_keys` inside an open transaction. The toggle is
/// essential: DDL such as `DROP TABLE sessions` (used by the v15/v16/v20
/// table-rebuild migrations) performs an implicit `DELETE` of all rows, which
/// would otherwise fire the application-level `ON DELETE CASCADE` and wipe
/// every `session_history` and `token_ledger` row. After the migrations
/// complete (or fail), FK is re-enabled.
///
/// The `foreign_key_check` integrity walk runs after a migration-applied run,
/// **in debug builds only**. The migrations run with FK enforcement off, so
/// this walk is the only verification their key writes kept the database
/// referentially sound — a developer's safety net for a broken migration,
/// caught on the first upgrade run against a seeded test database. An
/// end-user release build skips it: there is no automated repair for a
/// violation (failing the launch would strand the user over data nothing
/// can fix), and the walk is measured in seconds on large databases —
/// invisible debug overhead it may be, but a startup stall it is not.
///
/// Safe to call on an empty database (bootstraps the tracking table) and
/// idempotent on a fully-migrated one (the version check short-circuits
/// before any transaction).
///
/// # Errors
///
/// Returns an error if any migration fails or if the FK pragma cannot be
/// toggled. In debug builds, also when `foreign_key_check` reports integrity
/// violations after a run that applied migrations.
pub fn run_migrations(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    conn.pragma_update(None, "foreign_keys", "OFF")
        .change_context(SchemaMigrationError)
        .attach("disable foreign_keys before migration")?;

    let migrate_result = migrate::run_pending(conn);

    // Always re-enable FK, even if a migration failed, so the connection is
    // left in its normal (FK-on) state.
    conn.pragma_update(None, "foreign_keys", "ON")
        .change_context(SchemaMigrationError)
        .attach("re-enable foreign_keys after migration")?;

    // The integrity walk runs only in debug builds, and only after a run
    // that applied migrations — a no-op run changed nothing to verify, and
    // a release build deliberately trusts the migration (see the doc above).
    #[cfg(debug_assertions)]
    if matches!(migrate_result, Ok(true)) {
        debug_fk_check(conn)?;
    }
    match migrate_result {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The debug-build integrity walk after a migration-applied run.
///
/// Announced through the same stderr channel the migration announcements use
/// — the terminal is still in normal mode pre-TUI, so the line explains a
/// wait that otherwise reads as a freeze — then fails the run on any
/// violation. Returns the violating table names on failure.
///
/// # Errors
///
/// Returns an error if the check cannot run or reports violations.
#[cfg(debug_assertions)]
#[expect(
    clippy::print_stderr,
    reason = "pre-TUI user-facing announcement: migrations run before any tracing subscriber exists"
)]
fn debug_fk_check(conn: &mut rusqlite::Connection) -> Result<(), Report<SchemaMigrationError>> {
    eprintln!("{}", debug_fk_check_line());
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .change_context(SchemaMigrationError)
        .attach("prepare foreign_key_check")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .change_context(SchemaMigrationError)
        .attach("query foreign_key_check")?;
    let mut tables = Vec::new();
    for row in rows {
        tables.push(
            row.change_context(SchemaMigrationError)
                .attach("collect foreign_key_check row")?,
        );
    }
    drop(stmt);
    if tables.is_empty() {
        return Ok(());
    }
    Err(Report::new(SchemaMigrationError)
        .attach("foreign_key_check reported violations after migration")
        .attach(format!("violating tables: {}", tables.join(", "))))
}

/// The user-facing line printed before the debug-build integrity walk.
///
/// `pub` only so the `testing` module can re-export it to the format test;
/// `#[doc(hidden)]` keeps it out of the crate's public surface.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn debug_fk_check_line() -> String {
    "jinn: checking foreign-key integrity\u{2026}".to_owned()
}

/// Error type for schema migration failures.
///
/// Carries no variants — the failure detail lives in the `error_stack::Report`
/// context attachments (which migration failed, what SQL, which table violated
/// a constraint). This mirrors jinn's `SessionStoreError` convention.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SchemaMigrationError;
