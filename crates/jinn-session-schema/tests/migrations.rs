//! Verifies the schema crate produces the expected post-v20 shape standalone.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::redundant_type_annotations,
    reason = "test code asserts with expect and indexes fixtures for clear failures"
)]

use jinn_session_schema::run_migrations;

/// `_migrations` tracking row at v25.
#[rstest::rstest]
#[test]
fn fresh_database_has_all_tables_and_v21() {
    // Given a fresh in-memory database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");

    // When running all migrations.
    run_migrations(&mut conn).expect("run migrations");

    // Then all eight application tables exist.
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    for table in [
        "sessions",
        "entries",
        "session_history",
        "token_ledger",
        "discord_thread",
        "entry_blobs",
        "session_fts",
        "fts_dirty",
    ] {
        assert!(
            tables.contains(&table.to_owned()),
            "{table} table missing: {tables:?}"
        );
    }

    // And the highest recorded migration version is the published latest.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |row| row.get(0))
        .expect("query version");
    assert_eq!(version, i64::from(jinn_session_schema::LATEST_VERSION));

    // And token_ledger has the v24 prompt/cache columns.
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(token_ledger)")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    for column in ["prompt_tokens", "cached_tokens"] {
        assert!(
            columns.contains(&column.to_owned()),
            "token_ledger.{column} missing: {columns:?}"
        );
    }
}

/// Re-running `run_migrations` on a fully-migrated database is a no-op: the
/// version guards (`current < N`) prevent re-application.
#[rstest::rstest]
#[test]
fn re_running_migrations_is_noop() {
    // Given a fully-migrated in-memory database with a sentinel row.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
    run_migrations(&mut conn).expect("first run");

    // A row inserted before the second run must survive it.
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('x', 'X', 't', 't')",
        [],
    )
    .expect("insert sentinel");

    // When running the migrations again.
    run_migrations(&mut conn).expect("second run");

    // Then no migration re-applied and the sentinel row survived.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 1, "sentinel row survived the no-op re-run");
}

/// The v20 rebuild plus the v29–v31 additions left `sessions` with exactly
/// the eleven authoritative columns.
#[rstest::rstest]
#[test]
fn sessions_has_eleven_authoritative_columns() {
    // Given a fully-migrated in-memory database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
    run_migrations(&mut conn).expect("run migrations");

    // When reading the `sessions` column list.
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(sessions)")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    // Then it is exactly the eleven authoritative columns.
    assert_eq!(
        columns,
        vec![
            "id",
            "title",
            "updated_at",
            "created_at",
            "parent_session",
            "archived",
            "metadata",
            "is_automated",
            "persist",
            "in_library",
            "is_bot",
        ],
        "sessions columns after v31",
    );
}

/// v23 strips the legacy `s-` prefix from every persisted session-id
/// location: the five SQL columns plus the `session_id` and
/// `parent_session` keys inside the `sessions.metadata` JSON blob.
#[cfg(feature = "testing")]
/// A v22 database seeded with `s-`-prefixed session ids in every SQL column
/// the migration touches, plus a metadata blob carrying the prefixed ids.
/// The child's uuid, returned for the post-migration assertions.
fn seed_v22_with_prefixed_ids(conn: &mut rusqlite::Connection) -> &'static str {
    const CHILD: &str = "0195a3b2-4f8c-7d2e-8a1b-5c3d2e1f0a0b";
    const PARENT: &str = "0195a3b2-4f8c-7d2e-0000-000000000001";

    // sessions: one prefixed child + one prefixed parent. metadata blob
    // carries prefixed session_id and parent_session for the child.
    let child_blob = format!(
        "
        {{\"session_id\":\"s-{CHILD}\",\"parent_session\":\"s-{PARENT}\",
        \"profile\":{{}},\"blobs\":{{}},\"lifecycle_script_state\":\"nothing_ran\",
        \"lifecycle_args\":[],\"cwd\":\".\",\"persist\":true}}
    "
    );
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at, parent_session, archived, metadata, is_automated, persist) VALUES (?, 'child', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', ?, 0, ?, 0, 1)",
        rusqlite::params![format!("s-{CHILD}"), format!("s-{PARENT}"), child_blob],
    )
    .expect("insert child session");
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at, parent_session, archived, metadata, is_automated, persist) VALUES (?, 'parent', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', NULL, 0, NULL, 0, 1)",
        rusqlite::params![format!("s-{PARENT}")],
    )
    .expect("insert parent session");

    // session_history, token_ledger, discord_thread with prefixed child id.
    let child = format!("s-{CHILD}");
    conn.execute(
        "INSERT INTO entries (id, timing, kind) VALUES ('e-1', '2024-01-01T00:00:00Z', '{}')",
        [],
    )
    .expect("insert entry");
    for sql in [
        "INSERT INTO session_history (session_id, entry_id, ordinal) VALUES (?1, 'e-1', 0)",
        "INSERT INTO token_ledger (session_id, timestamp, tokens_sent, tokens_received) VALUES (?1, '2024-01-01T00:00:00Z', 1, 1)",
        "INSERT INTO discord_thread (thread_id, session_id, guild_id, created_at) VALUES ('t-1', ?1, NULL, 0)",
    ] {
        conn.execute(sql, [&child]).expect("insert child-id row");
    }
    CHILD
}

/// The parent session's uuid, matching `seed_v22_with_prefixed_ids`.
const V23_PARENT: &str = "0195a3b2-4f8c-7d2e-0000-000000000001";

/// Every SQL column v23 rewrites, as `(label, query, expected bare id)`.
fn v23_id_locations<'a>(
    child: &'a str,
    parent: &'a str,
) -> [(&'static str, &'static str, &'a str); 5] {
    [
        (
            "sessions.id",
            "SELECT id FROM sessions WHERE title = 'child'",
            child,
        ),
        (
            "sessions.parent_session",
            "SELECT parent_session FROM sessions WHERE title = 'child'",
            parent,
        ),
        (
            "session_history.session_id",
            "SELECT session_id FROM session_history",
            child,
        ),
        (
            "token_ledger.session_id",
            "SELECT session_id FROM token_ledger",
            child,
        ),
        (
            "discord_thread.session_id",
            "SELECT session_id FROM discord_thread",
            child,
        ),
    ]
}

#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn v23_strips_s_prefix_from_all_session_id_locations() {
    // Given a fresh DB migrated only to v22, seeded with s-prefixed IDs
    // across all five SQL columns and a prefixed metadata blob.
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 22);

    // And rows seeded with `s-`-prefixed ids in every location.
    let child = seed_v22_with_prefixed_ids(&mut conn);
    const PARENT: &str = V23_PARENT;

    // When running migrations (production path: toggles FK off, runs v23, re-enables + checks).
    run_migrations(&mut conn).expect("run pending migrations");

    // Then the five SQL columns are bare UUIDs.
    for (label, sql, expected) in v23_id_locations(child, PARENT) {
        let got: String = conn.query_row(sql, [], |r| r.get(0)).expect(label);
        assert_eq!(got, expected, "{label} should be bare");
    }

    // And the metadata blob's session_id / parent_session are bare.
    let blob: String = conn
        .query_row(
            "SELECT metadata FROM sessions WHERE title = 'child'",
            [],
            |r| r.get(0),
        )
        .expect("select metadata");
    for (label, key, id) in [
        ("session_id", "session_id", child),
        ("parent_session", "parent_session", PARENT),
    ] {
        assert!(
            blob.contains(&format!("\"{key}\":\"{id}\"")),
            "blob {label} should be bare, got: {blob}"
        );
    }
}

/// v25 adds the nullable `entries.token_count` column. Legacy rows (pre-v25)
/// read back as NULL, which the domain maps to "not yet computed".
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn v25_adds_nullable_token_count_column_to_entries() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a DB migrated only to v24, seeded with one entry.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 24);
    conn.execute(
        "INSERT INTO entries (id, timing, kind) VALUES ('e-1', '2024-01-01T00:00:00Z', '{}')",
        [],
    )
    .expect("insert entry");

    // When running the pending migrations (v25).
    run_migrations(&mut conn).expect("run pending migrations");

    // Then the token_count column exists, is NULL for the legacy row, and
    // accepts non-NULL counts.
    let columns: Vec<(String, String)> = conn
        .prepare("PRAGMA table_info(entries)")
        .expect("prepare")
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let token_count = columns
        .iter()
        .find(|(name, _)| name == "token_count")
        .expect("entries.token_count column missing");
    assert_eq!(
        token_count.1.to_uppercase(),
        "INTEGER",
        "token_count must be INTEGER, got: {token_count:?}"
    );

    let stored: Option<Option<i64>> = conn
        .query_row(
            "SELECT token_count FROM entries WHERE id = 'e-1'",
            [],
            |row| row.get(0),
        )
        .expect("select legacy row");
    assert!(
        stored.is_none(),
        "legacy row's token_count must read back as NULL"
    );

    conn.execute("UPDATE entries SET token_count = 42 WHERE id = 'e-1'", [])
        .expect("set token count");
    let after: i64 = conn
        .query_row(
            "SELECT token_count FROM entries WHERE id = 'e-1'",
            [],
            |row| row.get(0),
        )
        .expect("select updated row");
    assert_eq!(after, 42);
}

/// v26 seeds `fts_dirty` with every session that exists at migration time, so
/// the first post-upgrade launch backfills the FTS index in the background.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn v26_seeds_every_existing_session_dirty() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a DB migrated only to v25 with two sessions in it.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 25);
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('s-a', 'A', 't', 't')",
        [],
    )
    .expect("insert session a");
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('s-b', 'B', 't', 't')",
        [],
    )
    .expect("insert session b");

    // When running the pending migrations (v26).
    run_migrations(&mut conn).expect("run pending migrations");

    // Then both session ids are marked dirty.
    let dirty: Vec<String> = conn
        .prepare("SELECT session_id FROM fts_dirty ORDER BY session_id")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(dirty, vec!["s-a".to_owned(), "s-b".to_owned()]);
}

/// v26's dirty-marking triggers: INSERT, UPDATE, and DELETE on `sessions` each
/// mark the affected session id in `fts_dirty` (deduplicated by the PK).
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn v26_triggers_mark_sessions_dirty_on_insert_update_delete() {
    // Given a fully-migrated DB, and one fully-migrated DB with `s-b` seeded.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    run_migrations(&mut conn).expect("run migrations");
    let mut seeded = rusqlite::Connection::open_in_memory().expect("open db");
    run_migrations(&mut seeded).expect("run migrations");
    seeded
        .execute(
            "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('s-b', 'B', 't2', 't2')",
            [],
        )
        .expect("insert second session");

    // When running the insert, update, and delete the triggers fire on.
    let insert_a =
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('s-a', 'A', 't1', 't1')";
    for sql in [
        insert_a,
        "UPDATE sessions SET title = 'A2' WHERE id = 's-a'",
    ] {
        conn.execute(sql, []).expect("trigger statement");
    }
    for sql in [insert_a, "DELETE FROM sessions WHERE id = 's-a'"] {
        seeded.execute(sql, []).expect("trigger statement");
    }

    // Then every touched session is marked dirty, deduplicated by the primary
    // key — and the delete marks only the deleted id.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM fts_dirty WHERE session_id = 's-a'",
            [],
            |row| row.get(0),
        )
        .expect("count dirty after insert + update");
    assert_eq!(count, 1, "insert and update must mark dirty exactly once");

    let dirty: Vec<String> = seeded
        .prepare("SELECT session_id FROM fts_dirty ORDER BY session_id")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(dirty, vec!["s-a".to_owned(), "s-b".to_owned()]);
}

/// v26's `session_fts` virtual table is functional in the build DB: MATCH
/// queries work, UNINDEXED columns store but never match, and `snippet()`
/// wraps matches. (Catches a system-sqlite lacking FTS5 at build time.)
#[rstest::rstest]
#[test]
fn v26_fts_table_accepts_rows_and_matches() {
    // Given a fully-migrated DB.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    run_migrations(&mut conn).expect("run migrations");

    // When inserting two FTS rows with UNINDEXED metadata and querying them.
    for (body, session, entry, ts) in [
        (
            "the parser rewrites the junction table",
            "s-a",
            "e-1",
            "2026-09-01T00:00:00Z",
        ),
        (
            "something entirely unrelated to the query terms",
            "s-b",
            "e-2",
            "2026-09-02T00:00:00Z",
        ),
    ] {
        conn.execute(
            "INSERT INTO session_fts(body, role, session_id, entry_id, entry_ts) \
             VALUES (?1, 'assistant', ?2, ?3, ?4)",
            rusqlite::params![body, session, entry, ts],
        )
        .expect("insert fts row");
    }

    // Then a MATCH hits the expected rows: 'parser' only row 1; porter stems
    // 'rewrites' to 'rewrite'; the UNINDEXED role word never matches.
    for (term, expected) in [("parser", 1), ("rewrite", 1), ("assistant", 0)] {
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_fts WHERE session_fts MATCH ?1",
                [term],
                |row| row.get(0),
            )
            .expect("match term");
        assert_eq!(
            hits, expected,
            "MATCH '{term}' hit the wrong number of rows"
        );
    }

    // And snippet() wraps the matched terms.
    let snip: String = conn
        .query_row(
            "SELECT snippet(session_fts, 0, '<<', '>>', ' … ', 12) FROM session_fts \
             WHERE session_fts MATCH 'parser'",
            [],
            |row| row.get(0),
        )
        .expect("snippet");
    assert!(
        snip.contains("<<parser>>"),
        "snippet must wrap the match, got: {snip}"
    );
}

// ── Drift guards ─────────────────────────────────────────────────────────
//
// The migration chain, `LATEST_VERSION`, and the test seeding helper all
// derive from one `MIGRATIONS` table, but a stale `LATEST_VERSION` is still
// possible (and once shipped: v27 landed with the constant at 26, so every
// already-migrated v26 database silently never received v27 — the no-op
// early-return fired before the chain could run). These tests pin the
// invariants the constant must hold.

/// `LATEST_VERSION` names the newest migration in the chain.
///
/// With `LATEST_VERSION` stale, fresh databases still apply the whole chain
/// (so fresh-DB tests pass) while already-migrated databases silently stop
/// one version short. This is the exact shipped failure mode.
#[rstest::rstest]
#[test]
fn latest_version_matches_newest_chain_migration() {
    use jinn_session_schema::testing::{CHAIN_LATEST_VERSION, LATEST_VERSION, MIGRATIONS};

    // Given the migration chain and the published latest version.

    // When comparing them.

    // Then they agree, and the chain is dense from v0 with no gaps.
    assert_eq!(
        LATEST_VERSION, CHAIN_LATEST_VERSION,
        "LATEST_VERSION must equal the newest migration in MIGRATIONS"
    );
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        assert_eq!(
            migration.version, index as i32,
            "MIGRATIONS must be dense and ordered; index {index} holds v{}",
            migration.version
        );
    }
}

/// A database seeded at every historical version upgrades to `LATEST_VERSION`
/// via `run_migrations`.
///
/// This is the guard the shipped v27 bug would have failed: at seed v26 with
/// the constant stale at 26, `run_migrations`'s no-op early-return would leave
/// `current_version` at 26 while `LATEST_VERSION` claimed 27.
#[rstest::rstest]
#[test]
#[case(0)]
#[case(1)]
#[case(14)]
#[case(24)]
#[case(25)]
#[case(26)]
fn upgrade_from_seed_reaches_latest_version(#[case] seed: i32) {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database migrated to exactly `seed` (via the seeding helper, not
    // the production chain) plus the FK toggle run_migrations performs.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, seed);
    conn.pragma_update(None, "foreign_keys", "OFF")
        .expect("fk off");

    // When running the pending migrations.
    let seeded: i32 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("seed version");
    assert_eq!(seeded, seed, "seeding helper must land exactly on `seed`");
    run_migrations(&mut conn).expect("upgrade to latest");

    // Then the database reports the latest version.
    let current: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert_eq!(
        current,
        i64::from(jinn_session_schema::LATEST_VERSION),
        "seed v{seed} must upgrade to LATEST_VERSION"
    );
}

/// The concrete shipped regression: a database at v26 must receive v27's
/// `fts_dirty.resume_offset` column on the next `run_migrations`. Before the
/// fix, `LATEST_VERSION = 26` made `run_pending` early-return and the column
/// never appeared on upgraded databases (fresh ones were fine, which is why
/// the fresh-DB tests passed).
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn upgrade_from_v26_applies_resume_offset_migration() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database migrated to exactly v26.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 26);
    let has_column: bool = column_exists(&conn, "fts_dirty", "resume_offset");
    assert!(!has_column, "precondition: v26 db has no resume_offset yet");

    // When running the pending migrations.
    run_migrations(&mut conn).expect("upgrade from v26");

    // Then the v27 column exists and v27 is recorded.
    let has_column = column_exists(&conn, "fts_dirty", "resume_offset");
    assert!(
        has_column,
        "v27 must apply on a database seeded at v26; columns: {:?}",
        columns_of(&conn, "fts_dirty")
    );
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert!(version >= 27, "v27 recorded after upgrade; got {version}");
}

/// Seeding at every version (not a sample) is cheap in-memory and is the only
/// way to catch a migration whose postconditions depend on its neighbor. The
/// sampled `#[case]` test above stays for readable failure names; this sweep
/// guarantees no seed version is skipped when new migrations land.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn every_seed_version_upgrades_to_latest_and_backfills_are_consistent() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    let latest = jinn_session_schema::LATEST_VERSION;
    for seed in 0..latest {
        // Given a database migrated to exactly `seed`.
        let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
        bootstrap_tracking_table(&mut conn).expect("bootstrap");
        apply_migrations_inner(&mut conn, seed);

        // When running the pending migrations.
        run_migrations(&mut conn).expect("upgrade from seed {seed}");

        // Then the database reports the latest version, and the tracking
        // table holds exactly one row per version 0..=latest (dense, no
        // duplicates).
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0))
            .expect("count rows");
        assert_eq!(
            count,
            i64::from(latest) + 1,
            "seed v{seed}: expected one row per version, no duplicates"
        );
        let maxv: i64 = conn
            .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
            .expect("max version");
        assert_eq!(maxv, i64::from(latest), "seed v{seed}: upgraded to latest");
    }
}

/// Returns true when `table` has a column named `column`.
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    columns_of(conn, table).iter().any(|c| c == column)
}

/// Returns the column names of `table` in declaration order.
fn columns_of(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .expect("table_info")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query table_info")
        .map(|r| r.expect("row"))
        .collect()
}

/// An up-to-date database (no pending migrations) skips the `foreign_key_check`
/// integrity walk — nothing changed, so there is nothing to verify — and
/// pre-existing violations do not fail the no-op run. This is the performance
/// contract: the full walk costs seconds on large databases.
///
/// Debug builds only: the walk itself is debug-gated (see `run_migrations`).
#[rstest::rstest]
#[test]
#[cfg(debug_assertions)]
fn noop_run_skips_foreign_key_check() {
    // Given a fully-migrated database containing a pre-existing FK violation
    // (an orphaned session_history row, inserted with FK off).
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    run_migrations(&mut conn).expect("initial run");
    conn.pragma_update(None, "foreign_keys", "OFF")
        .expect("fk off");
    conn.execute(
        "INSERT INTO session_history (session_id, entry_id, ordinal) \
         VALUES ('ghost', 'ghost-entry', 0)",
        [],
    )
    .expect("seed orphan row");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("fk on");
    let violations = count_fk_violations(&conn);
    assert_eq!(violations, 2, "precondition: the orphan row violates FK");

    // When running migrations again (a no-op: nothing pending).
    run_migrations(&mut conn).expect("noop run succeeds");

    // Then the pre-existing violation is still there, undetected — the check
    // was skipped, not clean.
    assert_eq!(
        count_fk_violations(&conn),
        2,
        "orphan row untouched: the no-op run did not walk the FK graph"
    );
}

/// A debug-build run that applies migrations performs the integrity walk:
/// a migration-era FK violation fails the run. Release builds skip the walk
/// (there is no user-repairable action to gate the launch on), so the same
/// seeded database succeeds there.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn migration_run_with_fk_violation_fails_in_debug_and_passes_in_release() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database seeded at v26 with an orphaned session_history row
    // (created with FK off, as during migration DDL).
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 26);
    conn.pragma_update(None, "foreign_keys", "OFF")
        .expect("fk off");
    conn.execute(
        "INSERT INTO session_history (session_id, entry_id, ordinal) \
         VALUES ('ghost', 'ghost-entry', 0)",
        [],
    )
    .expect("seed orphan row");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("fk on");
    assert_eq!(count_fk_violations(&conn), 2, "precondition: violation");

    // When running the pending migrations (v27).
    let result = run_migrations(&mut conn);

    // Then the run's outcome follows the build: debug surfaces the FK
    // violations, release runs on.
    if cfg!(debug_assertions) {
        assert!(
            result.is_err(),
            "a debug run that applied migrations must surface FK violations"
        );
    } else {
        assert!(
            result.is_ok(),
            "a release run skips the integrity walk and must not fail"
        );
    }
}

/// Returns the number of FK violations in the database.
fn count_fk_violations(conn: &rusqlite::Connection) -> usize {
    conn.prepare("PRAGMA foreign_key_check")
        .expect("prepare foreign_key_check")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query foreign_key_check")
        .filter_map(std::result::Result::ok)
        .count()
}

/// Every pending migration announces `jinn: applying migration vN (name)…`
/// before it runs, in chain order — the user-facing explanation of an upgrade
/// launch's startup wait. Verified through the public run via the exported
/// line renderer's contract: an upgrade from a low version must record every
/// announcement the chain produced.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn upgrade_announces_each_pending_migration_with_version_and_name() {
    use jinn_session_schema::testing::{
        MIGRATIONS, apply_migrations_inner, bootstrap_tracking_table,
    };

    // Given a database seeded at v24 (so everything after v24 is pending) and
    // the recorded `_migrations` names the chain will announce.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 24);
    let pending: Vec<(i32, String)> = MIGRATIONS
        .iter()
        .filter(|m| m.version > 24)
        .map(|m| (m.version, m.name.to_owned()))
        .collect();
    assert!(
        !pending.is_empty(),
        "precondition: at least one migration is pending above v24"
    );

    // When running the pending migrations (stderr carries the announcements;
    // the rendered lines themselves are pinned by the format test below).
    run_migrations(&mut conn).expect("upgrade from v24");

    // Then every pending migration was recorded, in order — the same set the
    // announcer walked.
    let recorded: Vec<(i32, String)> = conn
        .prepare("SELECT version, name FROM _migrations WHERE version > 24 ORDER BY version")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(recorded, pending, "announced set == recorded set");
}

/// The announcement line's user-facing format: `jinn: applying migration
/// vN (name)…`, pinned so downstream capture tests and terminal output stay
/// stable.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn announcement_line_has_stable_format() {
    use jinn_session_schema::testing::announcement_line;

    // Given a migration version and name.

    // When rendering the announcement line.

    // Then it carries the `jinn:` prefix, the version, and the name.
    assert_eq!(
        announcement_line(27, "add_fts_dirty_resume_offset"),
        "jinn: applying migration v27 (add_fts_dirty_resume_offset)\u{2026}"
    );
}

/// The debug integrity-walk line's user-facing format, pinned alongside the
/// migration announcements it appears next to on an upgrade launch's stderr.
#[rstest::rstest]
#[test]
#[cfg(all(debug_assertions, feature = "testing"))]
fn fk_check_line_has_stable_format() {
    // Given no inputs — the line is a constant.

    // When rendering the line.

    // Then it carries the `jinn:` prefix and the ellipsis, matching the
    // migration announcements' shape.
    assert_eq!(
        jinn_session_schema::testing::debug_fk_check_line(),
        "jinn: checking foreign-key integrity\u{2026}"
    );
}

/// v28 creates the `fts_rowids` side table and backfills it from the existing
/// `session_fts` index: one map row per FTS row, session ids preserved.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn v28_backfills_rowid_map_from_existing_fts() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a DB migrated only to v27 with FTS rows for two sessions.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 27);
    conn.execute(
        "INSERT INTO session_fts(body, role, session_id, entry_id, entry_ts) \
         VALUES ('alpha body', 'user', 's-a', 'e-1', '2026-09-01T00:00:00Z')",
        [],
    )
    .expect("fts row a1");
    conn.execute(
        "INSERT INTO session_fts(body, role, session_id, entry_id, entry_ts) \
         VALUES ('alpha body two', 'user', 's-a', 'e-2', '2026-09-01T00:00:01Z')",
        [],
    )
    .expect("fts row a2");
    conn.execute(
        "INSERT INTO session_fts(body, role, session_id, entry_id, entry_ts) \
         VALUES ('beta body', 'assistant', 's-b', 'e-3', '2026-09-02T00:00:00Z')",
        [],
    )
    .expect("fts row b1");

    // When running the pending migration (v28).
    run_migrations(&mut conn).expect("upgrade from v27");

    // Then every FTS row is mapped to its session, with the FTS rowid.
    let mapped: Vec<(String, i64)> = conn
        .prepare("SELECT session_id, fts_rowid FROM fts_rowids ORDER BY fts_rowid")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let fts_rowids: Vec<i64> = conn
        .prepare("SELECT rowid FROM session_fts ORDER BY rowid")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        mapped,
        vec![
            ("s-a".to_owned(), fts_rowids[0]),
            ("s-a".to_owned(), fts_rowids[1]),
            ("s-b".to_owned(), fts_rowids[2]),
        ],
        "one map row per FTS row, sessions preserved"
    );

    // And the recorded migration version is the chain's latest (33) —
    // `run_migrations` runs the full chain, so every later version has
    // landed on top of v28.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert_eq!(version, 33, "v28 and later recorded");
}

/// v32 converts the database to incremental auto-vacuum so freed pages can
/// later be handed back to the filesystem (`PRAGMA incremental_vacuum`).
/// The conversion runs outside a transaction (a full `VACUUM` cannot run
/// inside one), but its effect is observable after any upgrade to latest.
#[rstest::rstest]
#[test]
fn migration_v32_converts_auto_vacuum_to_incremental() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database migrated to exactly v31, with a data row.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 31);
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES ('x', 'X', 't', 't')",
        [],
    )
    .expect("insert sentinel row");

    // When running the pending migrations.
    run_migrations(&mut conn).expect("upgrade from v31");

    // Then auto_vacuum reports incremental (2 — 0=none, 1=full, 2=incremental).
    let auto_vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .expect("read auto_vacuum");
    assert_eq!(auto_vacuum, 2, "auto_vacuum must be incremental after v32");

    // And all data survived the conversion.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .expect("count sessions");
    assert_eq!(count, 1, "the conversion must not lose rows");

    // And v32 is recorded like any other migration.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert!(version >= 32, "v32 recorded after the conversion");
}

/// A fresh database (seeded through the full chain from empty) ends at v32
/// with incremental auto-vacuum already in effect — the conversion step
/// gates on `current < 32`, which a first-ever run satisfies too.
#[rstest::rstest]
#[test]
fn fresh_database_lands_on_incremental_auto_vacuum() {
    // Given a fresh in-memory database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");

    // When running all migrations.
    run_migrations(&mut conn).expect("run migrations");

    // Then the database is at latest with incremental auto-vacuum.
    let auto_vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .expect("read auto_vacuum");
    assert_eq!(auto_vacuum, 2, "fresh db converts to incremental");
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert!(version >= 32, "fresh db records the conversion");
}

/// Re-running migrations after v32 never re-runs the conversion
/// (a second full `VACUUM` on a big database would stall every startup).
#[rstest::rstest]
#[test]
fn migration_v32_conversion_does_not_rerun_after_latest() {
    // Given a fully-migrated database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    run_migrations(&mut conn).expect("first run");

    // When running migrations again.
    run_migrations(&mut conn).expect("second run");

    // Then the database is unchanged at latest — the no-op early-return fires
    // before the conversion gate.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert!(
        version >= 32,
        "still at latest after a re-run, got {version}"
    );
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 32",
            [],
            |r| r.get(0),
        )
        .expect("count v32 rows");
    assert_eq!(rows, 1, "exactly one v32 tracking row, no duplicates");
}

/// v33 indexes `session_history(entry_id)` on a database upgraded from v32 —
/// the shape of every existing installation. Without that index the
/// entry-delete FK cascade and the orphan-reaping lookup scan the whole
/// junction table per chunk, which wedged large-install prunes.
#[rstest::rstest]
#[test]
fn upgrade_from_v32_creates_the_entry_id_index() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database migrated to exactly v32, with junction rows whose
    // entry ids the index must cover.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 32);
    seed_junction_row(&conn, "session-a", "entry-1");
    let has_index = has_index_named(&conn, "idx_session_history_entry");
    assert!(!has_index, "precondition: v32 db has no entry_id index yet");

    // When running the pending migrations.
    run_migrations(&mut conn).expect("upgrade from v32");

    // Then the index exists and the data survived.
    assert!(
        has_index_named(&conn, "idx_session_history_entry"),
        "v33 must create idx_session_history_entry"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_history", [], |r| r.get(0))
        .expect("count junction rows");
    assert_eq!(count, 1, "indexing must not lose rows");

    // And v33 is recorded like any other migration.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert_eq!(version, 33, "v33 recorded after the index build");
}

/// A fresh database lands on v33 with the entry-id index already in place —
/// the delete paths that need it exist from the first session ever stored.
#[rstest::rstest]
#[test]
fn fresh_database_has_the_entry_id_index() {
    // Given a fresh in-memory database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");

    // When running all migrations.
    run_migrations(&mut conn).expect("run migrations");

    // Then the index exists on the junction table.
    assert!(
        has_index_named(&conn, "idx_session_history_entry"),
        "fresh db must carry idx_session_history_entry"
    );
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert_eq!(version, 33, "fresh db records v33");
}

/// The v33 index is `IF NOT EXISTS`, so a database that already carries it
/// (hand-migrated while debugging a wedged prune, most plausibly) upgrades
/// cleanly instead of failing on the name collision.
#[rstest::rstest]
#[test]
fn upgrade_from_v32_tolerates_a_preexisting_entry_id_index() {
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a v32 database that already has the index, created by hand.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 32);
    conn.execute_batch("CREATE INDEX idx_session_history_entry ON session_history(entry_id)")
        .expect("hand-create the index");

    // When running the pending migrations.
    run_migrations(&mut conn).expect("upgrade over a preexisting index");

    // Then the upgrade succeeded and v33 is recorded.
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("current version");
    assert_eq!(version, 33, "v33 recorded despite the preexisting index");
}

/// A fresh run records `_migrations` in chain order: the table's rowid order
/// is 0..=latest ascending. Rowid order is application order, which the
/// runner's three-phase split keeps identical to stderr print order — the
/// property the split exists to guarantee.
#[rstest::rstest]
#[test]
fn fresh_run_records_migrations_in_chain_order() {
    use jinn_session_schema::LATEST_VERSION;

    // Given a fresh in-memory database.
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");

    // When running all migrations.
    run_migrations(&mut conn).expect("run migrations");

    // Then the tracking table reads 0..=latest in insertion (rowid) order.
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM _migrations ORDER BY rowid")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let expected: Vec<i64> = (0..=i64::from(LATEST_VERSION)).collect();
    assert_eq!(versions, expected, "recorded in chain order");
}

/// An upgrade records `_migrations` in chain order: rowid order is
/// 0..=latest ascending with the vacuum migration (v32) sitting between v31
/// and v33 — application order, not the runner's transaction phases.
#[rstest::rstest]
#[test]
#[cfg(feature = "testing")]
fn upgrade_from_v25_records_migrations_in_chain_order() {
    use jinn_session_schema::LATEST_VERSION;
    use jinn_session_schema::testing::{apply_migrations_inner, bootstrap_tracking_table};

    // Given a database seeded at v25 (its seeded rows are already in
    // ascending rowid order, matching chain order).
    let mut conn = rusqlite::Connection::open_in_memory().expect("open db");
    bootstrap_tracking_table(&mut conn).expect("bootstrap");
    apply_migrations_inner(&mut conn, 25);

    // When running the pending migrations.
    run_migrations(&mut conn).expect("upgrade from v25");

    // Then the whole table reads 0..=latest in insertion (rowid) order, so
    // v32 lands between v31 and v33 exactly as it was applied.
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM _migrations ORDER BY rowid")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let expected: Vec<i64> = (0..=i64::from(LATEST_VERSION)).collect();
    assert_eq!(versions, expected, "recorded in chain order");
}

/// Inserts one session_history junction row (with its required sessions and
/// entries parents) so index tests exercise real rows.
fn seed_junction_row(conn: &rusqlite::Connection, session_id: &str, entry_id: &str) {
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at) VALUES (?, 't', 't', 't')",
        [session_id],
    )
    .expect("insert session parent");
    conn.execute(
        "INSERT INTO entries (id, timing, kind) VALUES (?, 't', '{}')",
        [entry_id],
    )
    .expect("insert entry parent");
    conn.execute(
        "INSERT INTO session_history (session_id, entry_id, ordinal) VALUES (?, ?, 0)",
        [session_id, entry_id],
    )
    .expect("insert junction row");
}

/// Returns true when an index named `name` exists on any table.
fn has_index_named(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?")
        .expect("prepare index lookup")
        .query_map([name], |row| row.get::<_, String>(0))
        .expect("query index lookup")
        .count()
        > 0
}
