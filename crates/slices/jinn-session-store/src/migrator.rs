//! Database migration runner — thin async wrapper.
//!
//! Delegates the actual migration logic to [`jinn_session_schema`], a standalone
//! leaf crate shared with `build.rs` for `#[dao]` compile-time validation. This
//! file exists only to adapt the schema crate's synchronous `run_migrations`
//! (which takes a raw `&mut rusqlite::Connection`) to jinn's async dao pool.
//!
//! See `.plans/dao-validation-story/plan.md` for the rationale (the schema-crate
//! pattern; single source of truth; no `dao_schema.sql` to drift).
//!
//! # Version tracking
//!
//! The `_migrations` table records each completed migration. On startup the
//! runner reads the highest version and skips any migrations already applied.

use daow::Pool;
use error_stack::{Report, ResultExt as _};
use jinn_session_schema::SchemaMigrationError;

use jinn_domain::feat::session::SessionStoreError;

/// Runs all pending schema migrations.
///
/// Adapts [`jinn_session_schema::run_migrations`] to jinn's async dao pool.
///
/// The actual migration sequence (FK-toggle, DDL, `foreign_key_check`) lives
/// entirely in the schema crate; this wrapper only acquires a pool connection
/// and folds `SchemaMigrationError` into `SessionStoreError`.
///
/// # Errors
///
/// Returns an error if the connection cannot be acquired, any migration fails,
/// or `foreign_key_check` reports violations afterward.
pub async fn run_migrations(pool: &Pool) -> Result<(), Report<SessionStoreError>> {
    // The closure returns `daow::Result<Result<_, Report<SchemaMigrationError>>>`:
    // the outer `daow::Error` covers connection/pragma failures; the inner is the
    // migration outcome (which carries rich `.attach()` context from the schema
    // crate). Both layers are folded into `Report<SessionStoreError>` here.
    let outcome = pool
        .with_conn(|conn| {
            jinn_session_schema::run_migrations(conn)
                .map(Ok::<_, Report<SchemaMigrationError>>)
                .or_else(|report| Ok(Err(report)))
        })
        .await
        .change_context(SessionStoreError)
        .attach("failed to run migrations")?;

    outcome.change_context(SessionStoreError)
}

//
// The migrator test module (below) stands DBs up at specific legacy versions and
// asserts single-migration transformations. Those functions live in the schema
// crate behind its `testing` feature; re-export them here so `use super::*` in
// the test module resolves them unchanged.
#[cfg(test)]
use jinn_session_schema::testing::{
    apply_migrations_inner, bootstrap_tracking_table, migrate_v10, migrate_v15, migrate_v16,
    migrate_v17, migrate_v18, migrate_v19, record_version,
};

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code")]
pub(crate) async fn seed_at_version<F>(db_path: &str, target: i32, seed: F)
where
    F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<()> + Send + 'static,
{
    let pool = Pool::open(db_path).expect("open seed pool");
    pool.with_conn(move |conn| {
        bootstrap_tracking_table(conn).expect("bootstrap");
        apply_migrations_inner(conn, target);
        seed(conn).map_err(daow::Error::from)?;
        Ok(())
    })
    .await
    .expect("seed_at_version");
    // Drop the pool handle so the only reference is via the store's later open.
    drop(pool);
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    fn make_pool() -> (Pool, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("test.db");
        // Return the TempDir alongside the Pool so the caller holds it
        // for the test's lifetime. Dropping it would delete `test.db`, but
        // by returning it the caller keeps the file alive until test end.
        let pool = Pool::open(path.to_string_lossy().to_string().as_str()).expect("open pool");
        (pool, dir)
    }

    /// Synchronously applies migrations up to (and including) `target`.
    ///
    /// Runs inside a `with_conn` closure with FK=OFF (matching `run_migrations`).
    async fn apply_migrations_up_to(target: i32) -> (Pool, TempDir) {
        let (pool, dir) = make_pool();
        pool.with_conn(move |conn| {
            bootstrap_tracking_table(conn).expect("bootstrap");
            apply_migrations_inner(conn, target);
            Ok(())
        })
        .await
        .expect("apply_migrations_up_to");
        (pool, dir)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn run_migrations_creates_tracking_table() {
        // Given a fresh database.
        let (pool, _dir) = make_pool();

        // When running migrations.
        run_migrations(&pool).await.unwrap();

        // Then the _migrations table has one entry per migration.
        let rows: Vec<(i32, String)> = pool
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare("SELECT version, name FROM _migrations ORDER BY version")?;
                let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row?);
                }
                Ok(out)
            })
            .await
            .unwrap();

        let expected = usize::try_from(jinn_session_schema::LATEST_VERSION)
            .expect("LATEST_VERSION is non-negative")
            + 1;
        assert_eq!(rows.len(), expected);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[0].1, "create_initial_schema");
        assert_eq!(rows[1].0, 1);
        assert_eq!(rows[1].1, "add_cwd_column");
        assert_eq!(rows[2].0, 2);
        assert_eq!(rows[2].1, "add_created_at_column");
        assert_eq!(rows[3].0, 3);
        assert_eq!(rows[3].1, "add_ignored_to_session_entries");
        assert_eq!(rows[4].0, 4);
        assert_eq!(rows[4].1, "add_cost_to_token_ledger");
        assert_eq!(rows[5].0, 5);
        assert_eq!(rows[5].1, "add_lifecycle_columns_to_sessions");
        assert_eq!(rows[6].0, 6);
        assert_eq!(rows[6].1, "add_archived_column");
        assert_eq!(rows[7].0, 7);
        assert_eq!(rows[7].1, "add_lifecycle_script_state_column");
        assert_eq!(rows[8].0, 8);
        assert_eq!(rows[8].1, "add_metadata_column");
        assert_eq!(rows[9].0, 9);
        assert_eq!(rows[9].1, "rename_session_entries_to_session_history");
        assert_eq!(rows[10].0, 10);
        assert_eq!(rows[10].1, "consolidate_to_compaction_strategy");
        assert_eq!(rows[11].0, 11);
        assert_eq!(rows[11].1, "add_is_workflow_column");
        assert_eq!(rows[12].0, 12);
        assert_eq!(rows[12].1, "replace_ignored_with_context_override");
        assert_eq!(rows[13].0, 13);
        assert_eq!(rows[13].1, "add_judge_meta_column");
        assert_eq!(rows[14].0, 14);
        assert_eq!(rows[14].1, "add_context_history");
        assert_eq!(rows[19].0, 19);
        assert_eq!(rows[19].1, "rewrite_metadata_blob_profile_model");
        assert_eq!(rows[20].0, 20);
        assert_eq!(rows[20].1, "drop_zombie_columns_backfill_metadata");
        assert_eq!(rows[21].0, 21);
        assert_eq!(rows[21].1, "add_discord_thread_table");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn re_running_migrations_is_noop() {
        // Given a database with migrations already applied.
        let (pool, _dir) = make_pool();
        run_migrations(&pool).await.unwrap();

        // When running migrations again.
        run_migrations(&pool).await.unwrap();

        // Then no duplicate entries are added.
        let count: i64 = pool
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) AS count FROM _migrations", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let expected = i64::from(jinn_session_schema::LATEST_VERSION) + 1;
        assert_eq!(count, expected);
    }

    /// Verifies that each migration guard uses `<` not `<=`.
    ///
    /// For each version N (0..=LATEST), we build a database at exactly version
    /// N by seeding via the chain table, then re-run `run_migrations`. It must
    /// succeed (applying only v(N+1) through LATEST) and produce exactly one
    /// row per version, with no duplicates.
    ///
    /// If `current < N` were mutated to `current <= N`, vN would re-run when
    /// current == N. Most migrations would fail (duplicate table/column),
    /// and those that don't fail would produce a duplicate _migrations row,
    /// causing the count assertion to fail.
    #[rstest::rstest]
    #[tokio::test]
    async fn migration_guards_do_not_reapply_completed_version() {
        for target_version in 0..=jinn_session_schema::LATEST_VERSION {
            let (pool, _dir) = apply_migrations_up_to(target_version).await;

            // Re-running should succeed - applying only versions > target_version.
            run_migrations(&pool).await.unwrap_or_else(|e| {
                panic!("re-run at target_version={target_version} should succeed: {e:?}")
            });

            // Verify no duplicate rows: exactly one migration row per version.
            let count: i64 = pool
                .with_conn(|conn| {
                    conn.query_row("SELECT COUNT(*) AS count FROM _migrations", [], |r| {
                        r.get(0)
                    })
                    .map_err(daow::Error::from)
                })
                .await
                .unwrap();
            let expected = i64::from(jinn_session_schema::LATEST_VERSION) + 1;
            assert_eq!(
                count, expected,
                "at target_version={target_version}: expected one row per version, no duplicates"
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fresh_database_has_all_tables() {
        // Given a fresh database.
        let (pool, _dir) = make_pool();

        // When running migrations.
        run_migrations(&pool).await.unwrap();

        // Then all expected tables exist.
        let tables: Vec<String> = pool
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
                let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row?);
                }
                Ok(out)
            })
            .await
            .unwrap();
        let names: Vec<&str> = tables.iter().map(String::as_str).collect();
        assert!(names.contains(&"_migrations"));
        assert!(names.contains(&"entries"));
        assert!(names.contains(&"session_history"));
        assert!(names.contains(&"sessions"));
        assert!(names.contains(&"token_ledger"));
        assert!(names.contains(&"discord_thread"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v10_consolidates_strategy_state() {
        // Given a database with sessions containing mixed strategy state and profile.
        let (pool, _dir) = apply_migrations_up_to(9).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, strategy_state, blobs, lifecycle_script_state) \
                 VALUES ('test-1', 'Test', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"strategy\": \"sliding_window\", \"model\": \"test\", \"persona_name\": \"coding-assistant\", \"token_budget\": 150000, \"sliding_window_size\": 5}', \
                 '{\"passthrough\": \"Passthrough\", \"compaction\": {\"compaction\": {\"compaction_count\": 2}}}', \
                 '{}', 'nothing_ran')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v10.
        pool.with_conn(|conn| {
            migrate_v10(conn).expect("migrate v10");
            Ok(())
        })
        .await
        .unwrap();

        // Then strategy_state only has the compaction key.
        let (state_str, profile_str): (String, String) = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT strategy_state, profile FROM sessions WHERE id = 'test-1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();

        let state_map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&state_str).expect("parse state");
        assert!(state_map.contains_key("compaction"));
        assert!(!state_map.contains_key("passthrough"));
        assert!(!state_map.contains_key("sliding_window"));
        assert!(!state_map.contains_key("token_budget"));

        // And profile.strategy is "compaction".
        let profile: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&profile_str).expect("parse profile");
        assert_eq!(profile["strategy"], "compaction");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v10_inserts_default_when_no_compaction_key() {
        // Given a database with a session having no compaction key in strategy_state.
        let (pool, _dir) = apply_migrations_up_to(9).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, strategy_state, blobs, lifecycle_script_state) \
                 VALUES ('test-2', 'Test', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"strategy\": \"passthrough\", \"model\": \"test\"}', \
                 '{\"passthrough\": \"Passthrough\"}', \
                 '{}', 'nothing_ran')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v10.
        pool.with_conn(|conn| {
            migrate_v10(conn).expect("migrate v10");
            Ok(())
        })
        .await
        .unwrap();

        // Then strategy_state has a compaction key with default data.
        let state_str: String = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT strategy_state FROM sessions WHERE id = 'test-2'",
                    [],
                    |r| r.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let state_map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&state_str).expect("parse state");
        assert!(state_map.contains_key("compaction"));
        assert!(!state_map.contains_key("passthrough"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v15_drops_strategy_state_column() {
        // Given a database built to v14 (strategy_state column still present) with a session in it.
        let (pool, _dir) = apply_migrations_up_to(14).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, strategy_state, blobs, lifecycle_script_state) \
                 VALUES ('keep-me', 'Survivor', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{}', '{}', '{}', 'nothing_ran')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v15.
        pool.with_conn(|conn| {
            migrate_v15(conn).expect("migrate v15");
            Ok(())
        })
        .await
        .unwrap();

        // Then the sessions table no longer has a strategy_state column.
        let cols: Vec<String> = column_names(&pool, "sessions").await;
        assert!(
            !cols.iter().any(|c| c == "strategy_state"),
            "strategy_state column should be dropped; found columns: {cols:?}"
        );

        // And the session row survived the table rebuild.
        let ids: Vec<String> = pool
            .with_conn(|conn| {
                let mut stmt = conn.prepare("SELECT id FROM sessions")?;
                let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row?);
                }
                Ok(out)
            })
            .await
            .unwrap();
        assert!(
            ids.iter().any(|i| i == "keep-me"),
            "session 'keep-me' must survive the v15 table rebuild"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn v15_rebuild_preserves_session_history_rows() {
        // Given a v14 database with a session linked to an entry via session_history,
        // and the connection running with foreign_keys=ON — as the production
        // bootstrap connection does.
        let (pool, _dir) = apply_migrations_up_to(14).await;
        pool.with_conn(|conn| {
            conn.pragma_update(None, "foreign_keys", "ON")?;
            seed_session_with_children(conn);
            Ok(())
        })
        .await
        .unwrap();

        // When running migrations (applies v15, the DROP TABLE sessions rebuild).
        run_migrations(&pool).await.unwrap();

        // Then the session_history junction row survived the rebuild.
        let count: i64 = table_count(&pool, "session_history").await;
        assert_eq!(
            count, 1,
            "session_history junction row must survive v15 rebuild"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn v15_rebuild_preserves_token_ledger_rows() {
        // Given a v14 database with a token_ledger row for a session,
        // and the connection running with foreign_keys=ON.
        let (pool, _dir) = apply_migrations_up_to(14).await;
        pool.with_conn(|conn| {
            conn.pragma_update(None, "foreign_keys", "ON")?;
            seed_session_with_children(conn);
            Ok(())
        })
        .await
        .unwrap();

        // When running migrations (applies v15).
        run_migrations(&pool).await.unwrap();

        // Then the token_ledger row survived the rebuild.
        let count: i64 = table_count(&pool, "token_ledger").await;
        assert_eq!(count, 1, "token_ledger row must survive v15 rebuild");
    }

    /// Seeds a v14 database with one session, one entry, one session_history
    /// junction row linking them, and one token_ledger row.
    ///
    /// Call after `apply_migrations_up_to(14)`.
    fn seed_session_with_children(conn: &mut rusqlite::Connection) {
        conn.execute(
            "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, strategy_state, blobs, lifecycle_script_state) \
             VALUES ('s-1', 'T', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', '{}', '{}', '{}', 'nothing_ran')",
            [],
        )
        .expect("seed session");
        conn.execute(
            "INSERT INTO entries (id, timestamp, kind) VALUES ('e-1', '2024-01-01T00:00:00Z', '\"User\"')",
            [],
        )
        .expect("seed entry");
        conn.execute(
            "INSERT INTO session_history (session_id, entry_id, ordinal) VALUES ('s-1', 'e-1', 0)",
            [],
        )
        .expect("seed junction");
        conn.execute(
            "INSERT INTO token_ledger (session_id, timestamp, tokens_sent, tokens_received) \
             VALUES ('s-1', '2024-01-01T00:00:00Z', 10, 20)",
            [],
        )
        .expect("seed ledger");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn run_migrations_preserves_child_rows_and_leaves_fk_clean() {
        // Given a v14 database with FK=ON and a full session (junction + ledger).
        let (pool, _dir) = apply_migrations_up_to(14).await;
        pool.with_conn(|conn| {
            conn.pragma_update(None, "foreign_keys", "ON")?;
            seed_session_with_children(conn);
            Ok(())
        })
        .await
        .unwrap();

        // When running the full migration suite.
        run_migrations(&pool).await.unwrap();

        // Then both child tables retain their row.
        assert_eq!(
            table_count(&pool, "session_history").await,
            1,
            "session_history row preserved"
        );
        assert_eq!(
            table_count(&pool, "token_ledger").await,
            1,
            "token_ledger row preserved"
        );

        // And foreign_key_check reports no violations on the final state.
        let violations: Vec<String> = pool
            .with_conn(|conn| {
                let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
                let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row?);
                }
                Ok(out)
            })
            .await
            .unwrap();
        assert!(
            violations.is_empty(),
            "foreign_key_check must be clean after migrations; found: {violations:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v17_rewrites_bare_string_model_to_single() {
        // Given a database at v16 with a session having a bare-string model.
        let (pool, _dir) = apply_migrations_up_to(16).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, lifecycle_script_state, is_automated, persist) \
                 VALUES ('test-v17', 'Test', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"strategy\": \"sliding_window\", \"model\": \"ollama/llama3\", \"persona_name\": \"coding-assistant\", \"token_budget\": 150000, \"sliding_window_size\": 5}', \
                 '{}', 'nothing_ran', 0, 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v17.
        pool.with_conn(|conn| {
            migrate_v17(conn).expect("migrate v17");
            Ok(())
        })
        .await
        .unwrap();

        // Then the profile JSON has model rewritten as {"single":"ollama/llama3"}.
        let profile_str: String = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT profile FROM sessions WHERE id = 'test-v17'",
                    [],
                    |r| r.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let profile: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&profile_str).expect("parse profile");
        assert_eq!(
            profile["model"],
            serde_json::json!({"single": "ollama/llama3"})
        );
        // And other profile fields are preserved.
        assert_eq!(profile["strategy"], "sliding_window");
        assert_eq!(profile["persona_name"], "coding-assistant");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v17_handles_no_provider_id() {
        // Given a database at v16 with a session having <none> as model.
        let (pool, _dir) = apply_migrations_up_to(16).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, lifecycle_script_state, is_automated, persist) \
                 VALUES ('test-none', 'Test', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"strategy\": \"sliding_window\", \"model\": \"<none>\", \"persona_name\": \"coding-assistant\"}', \
                 '{}', 'nothing_ran', 0, 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v17.
        pool.with_conn(|conn| {
            migrate_v17(conn).expect("migrate v17");
            Ok(())
        })
        .await
        .unwrap();

        // Then the profile JSON has model rewritten as {"single":"<none>"}.
        let profile_str: String = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT profile FROM sessions WHERE id = 'test-none'",
                    [],
                    |r| r.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let profile: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&profile_str).expect("parse profile");
        assert_eq!(profile["model"], serde_json::json!({"single": "<none>"}));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v17_is_idempotent() {
        // Given a database at v16 with a session.
        let (pool, _dir) = apply_migrations_up_to(16).await;
        pool.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, lifecycle_script_state, is_automated, persist) \
                 VALUES ('test-idem', 'Test', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"strategy\": \"sliding_window\", \"model\": \"ollama/llama3\"}', \
                 '{}', 'nothing_ran', 0, 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migration v17 twice.
        pool.with_conn(|conn| {
            migrate_v17(conn).expect("first");
            Ok(())
        })
        .await
        .unwrap();
        pool.with_conn(|conn| {
            migrate_v17(conn).expect("second");
            Ok(())
        })
        .await
        .unwrap();

        // Then the profile is still correct, not double-wrapped.
        let profile_str: String = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT profile FROM sessions WHERE id = 'test-idem'",
                    [],
                    |r| r.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let profile: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&profile_str).expect("parse profile");
        assert_eq!(
            profile["model"],
            serde_json::json!({"single": "ollama/llama3"})
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v17_adds_model_used_column() {
        // Given a database at v16 (no model_used column).
        let (pool, _dir) = apply_migrations_up_to(16).await;

        // When running migration v17.
        pool.with_conn(|conn| {
            migrate_v17(conn).expect("migrate v17");
            Ok(())
        })
        .await
        .unwrap();

        // Then the token_ledger table has a model_used column.
        let cols = column_names(&pool, "token_ledger").await;
        assert!(
            cols.contains(&"model_used".to_owned()),
            "expected model_used column, got: {cols:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v18_renames_timestamp_and_preserves_value() {
        // Given a database at v16 (timestamp column, before rename).
        let (pool, _dir) = apply_migrations_up_to(15).await;
        pool.with_conn(|conn| {
            migrate_v16(conn).expect("v16");
            record_version(conn, 16, "add_persist_and_is_automated").expect("record v16");
            Ok(())
        })
        .await
        .unwrap();

        // Insert a row with the old `timestamp` column.
        let legacy_ts = "2024-01-15T10:30:00Z";
        pool.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO entries (id, timestamp, kind) VALUES ('e1', ?, 'user')",
                params![legacy_ts],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migrations (v18 renames timestamp → timing).
        run_migrations(&pool).await.unwrap();

        // Then the column is renamed and the value is preserved.
        let timing: String = pool
            .with_conn(|conn| {
                conn.query_row("SELECT timing FROM entries WHERE id = 'e1'", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        assert_eq!(timing, legacy_ts, "value preserved through rename");
    }

    // ── v19: rewrite embedded profile.model in metadata blob ─────────
    //
    // A realistic 0.65 metadata blob looks like a PersistableCore JSON with
    // profile.model as a bare string. The helper constructs one.

    /// Builds a metadata blob (PersistableCore shape) whose profile.model is the
    /// given JSON value, so tests can inject bare-string, object, or other shapes.
    fn make_metadata_blob(model: &serde_json::Value) -> String {
        let blob = serde_json::json!({
            "session_id": "s1",
            "title": "T",
            "updated_at": "2024-01-01T00:00:00Z",
            "created_at": "2024-01-01T00:00:00Z",
            "profile": {
                "model": model.clone(),
                "persona_name": "coding-assistant"
            },
            "cwd": ".",
            "blobs": {},
            "lifecycle_args": [],
            "lifecycle_script_state": "nothing_ran"
        });
        serde_json::to_string(&blob).expect("serialize blob")
    }

    /// Inserts a session row with the given id and metadata blob at v18 schema.
    async fn insert_v19_session(pool: &Pool, id: &str, metadata: &str) {
        let id = id.to_owned();
        let metadata = metadata.to_owned();
        pool.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, lifecycle_script_state, is_automated, persist, metadata) VALUES (?, 'T', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', '{}', '{}', 'nothing_ran', 0, 0, ?)",
                params![id, metadata],
            )?;
            Ok(())
        })
        .await
        .expect("insert v19 test session");
    }

    /// Builds a v18 database (the pre-v19 state) so v19 tests can run in isolation.
    async fn make_v18_pool() -> Pool {
        let (pool, _dir) = apply_migrations_up_to(17).await;
        pool.with_conn(|conn| {
            migrate_v18(conn).expect("v18");
            record_version(conn, 18, "rename_entries_timestamp_to_timing").expect("record v18");
            Ok(())
        })
        .await
        .expect("build v18");
        pool
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v19_rewrites_bare_string_model_in_blob() {
        // Given a database at v18 with a 0.65-shape metadata blob (bare-string model).
        let pool = make_v18_pool().await;
        let blob = make_metadata_blob(&serde_json::json!("ollama/llama3"));
        insert_v19_session(&pool, "s1", &blob).await;

        // When running migration v19.
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("migrate v19");
            Ok(())
        })
        .await
        .unwrap();

        // Then the blob's embedded profile.model is rewritten to {"single":...}.
        let meta_str: String = pool
            .with_conn(|conn| {
                conn.query_row("SELECT metadata FROM sessions WHERE id = 's1'", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let meta: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&meta_str).expect("parse metadata");
        let profile = meta["profile"].as_object().expect("profile is object");
        assert_eq!(
            profile["model"],
            serde_json::json!({"single": "ollama/llama3"})
        );
        // And other profile fields are preserved.
        assert_eq!(profile["persona_name"], "coding-assistant");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v19_leaves_already_object_model_unchanged() {
        // Given a database at v18 with a 0.66-shape blob (model already an object).
        let pool = make_v18_pool().await;
        let blob = make_metadata_blob(&serde_json::json!({"single": "ollama/llama3"}));
        insert_v19_session(&pool, "s1", &blob).await;

        // When running migration v19 twice (idempotency check).
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("first");
            Ok(())
        })
        .await
        .unwrap();
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("second");
            Ok(())
        })
        .await
        .unwrap();

        // Then the blob is unchanged - model is still {"single":...}, not double-wrapped.
        let meta_str: String = pool
            .with_conn(|conn| {
                conn.query_row("SELECT metadata FROM sessions WHERE id = 's1'", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let meta: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&meta_str).expect("parse metadata");
        let profile = meta["profile"].as_object().expect("profile is object");
        assert_eq!(
            profile["model"],
            serde_json::json!({"single": "ollama/llama3"})
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v19_leaves_blob_without_model_field_unchanged() {
        // Given a blob whose profile has no model field at all.
        let pool = make_v18_pool().await;
        let blob = "{\"session_id\":\"s1\",\"profile\":{\"persona_name\":\"x\"}}";
        insert_v19_session(&pool, "s1", blob).await;

        // When running migration v19.
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("migrate v19");
            Ok(())
        })
        .await
        .unwrap();

        // Then the blob is semantically unchanged (no model added, no fields lost).
        let meta_str: String = pool
            .with_conn(|conn| {
                conn.query_row("SELECT metadata FROM sessions WHERE id = 's1'", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let after: serde_json::Value = serde_json::from_str(&meta_str).expect("parse after");
        let before: serde_json::Value = serde_json::from_str(blob).expect("parse before");
        assert_eq!(
            after, before,
            "blob without model field must be semantically untouched"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v19_preserves_alloy_model() {
        // Given a blob with an Alloy model (object form, must not be re-wrapped as Single).
        let pool = make_v18_pool().await;
        let blob = make_metadata_blob(&serde_json::json!({
            "Alloy": {"models": ["a", "b"], "strategy": "RoundRobin"}
        }));
        insert_v19_session(&pool, "s1", &blob).await;

        // When running migration v19.
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("migrate v19");
            Ok(())
        })
        .await
        .unwrap();

        // Then the Alloy model is preserved exactly, not wrapped as Single.
        let meta_str: String = pool
            .with_conn(|conn| {
                conn.query_row("SELECT metadata FROM sessions WHERE id = 's1'", [], |r| {
                    r.get(0)
                })
                .map_err(daow::Error::from)
            })
            .await
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_str).expect("parse");
        let model = &meta["profile"]["model"];
        assert_eq!(
            model,
            &serde_json::json!({"Alloy": {"models": ["a", "b"], "strategy": "RoundRobin"}}),
            "Alloy model must be preserved, not wrapped as Single"
        );
    }

    // ── v20: backfill NULL metadata, then drop zombie columns ─────────

    /// Builds a v19 database with one NULL-metadata (legacy pre-v8) row, ready
    /// for v20 testing. Mirrors the fixture the retired
    /// `migrate_v19_skips_null_metadata_rows` test used.
    async fn make_v19_pool_with_legacy_row() -> Pool {
        let pool = make_v18_pool().await;
        pool.with_conn(|conn| {
            migrate_v19(conn).expect("v19");
            record_version(conn, 19, "rewrite_metadata_blob_profile_model").expect("record v19");
            conn.execute(
                "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, \
                 lifecycle_script_state, is_automated, persist) \
                 VALUES ('legacy', 'Legacy', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
                 '{\"model\":{\"single\":\"ollama/llama3\"},\"persona_name\":\"coding-assistant\"}', '{}', 'nothing_ran', 0, 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("seed legacy row");
        pool
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v20_backfills_null_metadata_and_drops_zombies() {
        // Given a v19 database with one NULL-metadata (pre-v8) row.
        let pool = make_v19_pool_with_legacy_row().await;

        // When migrating to v20.
        run_migrations(&pool).await.expect("migrate to v20");

        // Then sessions has exactly 9 columns.
        let cols = column_names(&pool, "sessions").await;
        assert_eq!(
            cols.len(),
            9,
            "sessions must have exactly 9 columns; got {cols:?}"
        );
        assert!(
            !cols.iter().any(|c| matches!(
                c.as_str(),
                "profile"
                    | "blobs"
                    | "cwd"
                    | "lifecycle_name"
                    | "lifecycle_args"
                    | "lifecycle_script_state"
                    | "judge_meta"
            )),
            "zombie + judge_meta columns must be dropped; got {cols:?}"
        );

        // And the legacy row's metadata was backfilled (non-NULL, parseable, with
        // the expected model shape).
        let meta_str: Option<String> = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT metadata FROM sessions WHERE id = 'legacy'",
                    [],
                    |r| r.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .expect("query legacy metadata");
        let meta_str = meta_str.expect("metadata must be backfilled (non-NULL)");
        let meta: serde_json::Value =
            serde_json::from_str(&meta_str).expect("backfilled blob parses");
        // v17 already rewrote the column, so the backfilled blob carries the
        // {"single": ...} shape.
        assert_eq!(
            meta["profile"]["model"],
            serde_json::json!({"single": "ollama/llama3"}),
            "backfilled blob must carry the v17-rewritten model"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migrate_v20_backfilled_blob_loads_as_persistable_core() {
        // Given a v19 database with one NULL-metadata (pre-v8) row.
        let pool = make_v19_pool_with_legacy_row().await;

        // When migrating to v20.
        run_migrations(&pool).await.expect("migrate to v20");

        // Then the backfilled blob deserializes cleanly into PersistableCore.
        let meta_str: String = pool
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT metadata FROM sessions WHERE id = 'legacy'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .expect("query backfilled blob");
        let meta: serde_json::Value =
            serde_json::from_str(&meta_str).expect("backfilled blob parses as JSON");
        assert_eq!(meta["title"].as_str(), Some("Legacy"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn migration_failure_rolls_back_to_prior_state() {
        // Given a fresh database poisoned so a mid-chain migration collides.
        //
        // v15 rebuilds `sessions` via a temp table named `sessions_new`.
        // Pre-creating that name on an empty DB lets v0..=v14 run, then v15's
        // `CREATE TABLE sessions_new (...)` fails with "table already exists".
        let (pool, _dir) = make_pool();
        pool.with_conn(|conn| {
            conn.execute_batch("CREATE TABLE sessions_new (id TEXT)")?;
            Ok(())
        })
        .await
        .unwrap();

        // When running migrations, which fail at v15.
        let result = run_migrations(&pool).await;

        // Then migration fails.
        assert!(
            result.is_err(),
            "migrations must fail when a mid-chain collision occurs"
        );

        // And the whole chain rolled back: the real `sessions` table (created by
        // v0 inside the transaction) is absent, so v0's `CREATE TABLE sessions`
        // was undone.
        let tables = table_list(&pool).await;
        assert!(
            !tables.iter().any(|t| t == "sessions"),
            "real `sessions` table must be absent after rollback; got {tables:?}"
        );

        // And `_migrations` exists (bootstrapped pre-BEGIN) but holds 0 rows
        // (every `record_version` ran inside the rolled-back transaction).
        assert!(
            tables.iter().any(|t| t == "_migrations"),
            "`_migrations` table must exist (bootstrapped before BEGIN)"
        );
        assert_eq!(
            table_count(&pool, "_migrations").await,
            0,
            "no version rows must survive a rolled-back transaction"
        );

        // And the poison table is still there (proves we're at the pre-chain
        // state, not a partially-applied one).
        assert!(
            tables.iter().any(|t| t == "sessions_new"),
            "poison `sessions_new` must still exist after rollback"
        );
    }

    /// Returns the names of all tables in the database, sorted.
    async fn table_list(pool: &Pool) -> Vec<String> {
        pool.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
            let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in mapped {
                out.push(row?);
            }
            Ok(out)
        })
        .await
        .expect("list tables")
    }

    // ── shared test helpers ───────────────────────────────────────────

    async fn table_count(pool: &Pool, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        pool.with_conn(move |conn| {
            conn.query_row(&sql, [], |r| r.get(0))
                .map_err(daow::Error::from)
        })
        .await
        .expect("count")
    }

    async fn column_names(pool: &Pool, table: &str) -> Vec<String> {
        let table = table.to_owned();
        pool.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let mapped = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut out = Vec::new();
            for row in mapped {
                out.push(row?);
            }
            Ok(out)
        })
        .await
        .expect("table_info")
    }
}
