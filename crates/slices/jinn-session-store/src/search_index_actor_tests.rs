//! Tests for the `SearchIndexActor` tick/drain behavior.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test code")]

use std::time::Duration;

use crate::search_index_actor::{REINDEX_INTERVAL, SearchIndexActorDeps};
use crate::sqlite::SqliteSessionStore;
use jinn_core_types::SessionId;
use jinn_domain::common::bus::test_harness::TestHarness;
use jinn_domain::feat::session::SessionStoreService;
use jinn_domain::feat::session::chat_session::ChatSessionState;

/// Builds actor deps whose session store is a real SQLite store in a temp
/// dir, so drains exercise the actual dirty-marker → FTS pipeline. Returns
/// the harness too, so tests can spawn recorders on the same bus the actor
/// publishes to, and the concrete store for direct DB access.
async fn sqlite_actor_deps() -> (
    tempfile::TempDir,
    TestHarness,
    jinn_domain::common::actor_deps::ActorDeps,
    std::sync::Arc<SqliteSessionStore>,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = std::sync::Arc::new(SqliteSessionStore::new_in(dir.path()).await.expect("store"));
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.session_store = SessionStoreService::new(store.clone());
    let deps = jinn_domain::common::actor_deps::ActorDeps { services };
    (dir, harness, deps, store)
}

/// A two-entry session whose entries mention "needle" so it is findable.
fn needle_session(id: &SessionId) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    session.set_session_id(id.clone());
    session.set_title("tick".to_owned());
    session.push_entry(jinn_core_types::ChatEntry::user(
        "the needle thread pulls through",
    ));
    session.push_entry(jinn_core_types::ChatEntry::assistant(
        "stitching the needle into place",
    ));
    session
}

async fn search_all(
    store: &SessionStoreService,
) -> jinn_domain::feat::session_search::SearchOutcome {
    store
        .search(jinn_domain::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search")
}

/// Expects `f` to succeed before `attempts` polls of `pause` elapse.
async fn poll_until<F, Fut>(pause: Duration, attempts: usize, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..attempts {
        if f().await {
            return true;
        }
        tokio::time::sleep(pause).await;
    }
    f().await
}

#[rstest::rstest]
#[tokio::test]
async fn startup_drain_indexes_all_dirty_sessions() {
    // Given a dirty session in the store (fresh saves seed the dirty table).
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    let session_id = SessionId::new();
    deps.services
        .session_store
        .save(&needle_session(&session_id))
        .await
        .expect("save");

    // When spawning the actor (its on_start kicks an immediate drain).
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: REINDEX_INTERVAL,
            batch: usize::MAX,
        },
    );

    // Then the startup drain indexes the session without waiting a tick.
    let indexed = poll_until(Duration::from_millis(50), 40, || async {
        search_all(&deps.services.session_store).await.total_matches == 2
    })
    .await;
    assert!(indexed, "startup drain should index the dirty session");
}

#[rstest::rstest]
#[tokio::test]
async fn tick_loop_picks_up_sessions_marked_after_startup() {
    // Given a running actor with a tiny injected interval.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // When a session is saved after the actor is already running.
    let session_id = SessionId::new();
    deps.services
        .session_store
        .save(&needle_session(&session_id))
        .await
        .expect("save");

    // Then a subsequent tick indexes it (no manual drain involved).
    let indexed = poll_until(Duration::from_millis(50), 40, || async {
        search_all(&deps.services.session_store).await.total_matches == 2
    })
    .await;
    assert!(indexed, "tick loop should converge on newly dirty sessions");
}

#[rstest::rstest]
#[tokio::test]
async fn failed_drain_leaves_marker_and_next_drain_recovers() {
    // Given a session whose marker is re-inserted directly (simulating
    // pending work left behind by a failed drain).
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    let session_id = SessionId::new();
    deps.services
        .session_store
        .save(&needle_session(&session_id))
        .await
        .expect("save");

    // Drain once so the index is built, then re-mark the session dirty the
    // way a crashed drain would leave it (insert straight into fts_dirty).
    let primed = deps
        .services
        .session_store
        .dirty_session_ids()
        .await
        .expect("ids");
    for id in &primed {
        deps.services
            .session_store
            .reindex_session_chunk(id, 10_000)
            .await
            .expect("priming reindex");
    }
    store
        .pool()
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO fts_dirty(session_id) VALUES (?) \
                 ON CONFLICT(session_id) DO NOTHING",
                rusqlite::params![session_id.to_string()],
            )
            .map_err(daow::Error::from)
        })
        .await
        .expect("mark dirty");

    // When the actor runs with a tiny interval.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then a tick drains the pending work to zero and the session stays
    // searchable throughout.
    let drained = poll_until(Duration::from_millis(50), 40, || async {
        deps.services
            .session_store
            .pending_dirty_count()
            .await
            .expect("count")
            == 0
    })
    .await;
    assert!(drained, "actor should drain the re-marked session");
    assert_eq!(
        search_all(&deps.services.session_store).await.total_matches,
        2,
        "session remains searchable"
    );
}

#[rstest::rstest]
#[test]
fn production_interval_is_five_seconds() {
    // Given the production interval constant.
    // Then it is exactly 5 seconds (matches the record/plan contract).
    assert_eq!(REINDEX_INTERVAL, Duration::from_secs(5));
}

#[rstest::rstest]
#[tokio::test]
async fn failing_session_does_not_block_rest_of_batch() {
    // Given three dirty sessions and a tripwire trigger that aborts the FTS
    // rebuild for one specific session (the poisoning fault).
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    let good_a = SessionId::new();
    let poisoned = SessionId::new();
    let good_b = SessionId::new();
    for id in [&good_a, &poisoned, &good_b] {
        deps.services
            .session_store
            .save(&needle_session(id))
            .await
            .expect("save");
    }
    create_reindex_tripwire(&store, &poisoned).await;

    // When the actor runs with a tiny interval.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then both good sessions are indexed despite the poisoned one failing.
    let indexed = {
        let store = deps.services.session_store.clone();
        poll_until(Duration::from_millis(50), 40, || {
            let store = store.clone();
            let ids = [good_a.clone(), good_b.clone()];
            async move {
                let outcome = search_all(&store).await;
                let hit_sessions: std::collections::HashSet<&str> =
                    outcome.hits.iter().map(|h| h.session_id.as_str()).collect();
                ids.iter()
                    .filter(|id| hit_sessions.contains(id.to_string().as_str()))
                    .count()
                    == ids.len()
            }
        })
        .await
    };
    assert!(
        indexed,
        "good sessions should be indexed around the failure"
    );
    // And the poisoned session has no index rows (its rebuild aborted).
    let outcome = search_all(&deps.services.session_store).await;
    let poisoned_hits = outcome
        .hits
        .iter()
        .filter(|h| h.session_id == poisoned.to_string())
        .count();
    assert_eq!(poisoned_hits, 0, "poisoned session must not be indexed");
}

#[rstest::rstest]
#[tokio::test]
async fn failed_session_marker_survives_and_recovers_when_fault_clears() {
    // Given a poisoned session alongside a good one (tripwire aborts the
    // poisoned session's rebuild).
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    let good = SessionId::new();
    let poisoned = SessionId::new();
    for id in [&good, &poisoned] {
        deps.services
            .session_store
            .save(&needle_session(id))
            .await
            .expect("save");
    }
    create_reindex_tripwire(&store, &poisoned).await;

    // When the actor runs with a tiny interval.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then the failed session's marker survives (durable pending work) while
    // the good session drains.
    let marker_left = poll_until(Duration::from_millis(50), 40, || async {
        deps.services
            .session_store
            .pending_dirty_count()
            .await
            .expect("count")
            == 1
    })
    .await;
    assert!(marker_left, "poisoned marker should remain after the drain");
    let still_dirty: Vec<String> = store
        .pool()
        .query_all("SELECT session_id AS session_id FROM fts_dirty", vec![])
        .await
        .expect("read dirty");
    assert_eq!(still_dirty, vec![poisoned.to_string()]);

    // When the tripwire is removed (fault clears), the next tick retries.
    store
        .pool()
        .with_conn(|conn| {
            conn.execute("DROP TRIGGER reindex_tripwire", [])
                .map_err(daow::Error::from)
        })
        .await
        .expect("drop tripwire");

    // Then the poisoned session recovers: drained and searchable.
    let recovered = poll_until(Duration::from_millis(50), 40, || async {
        deps.services
            .session_store
            .pending_dirty_count()
            .await
            .expect("count")
            == 0
    })
    .await;
    assert!(
        recovered,
        "poisoned session should drain after fault clears"
    );
    assert_eq!(
        search_all(&deps.services.session_store).await.total_matches,
        4,
        "both sessions fully indexed after recovery"
    );
}

/// Creates a `BEFORE DELETE` trigger on `fts_dirty` that aborts only for the
/// given session — simulating a reindex failure for that session alone.
async fn create_reindex_tripwire(store: &SqliteSessionStore, poisoned: &SessionId) {
    let poisoned_id = poisoned.to_string();
    let create_sql = format!(
        "CREATE TRIGGER reindex_tripwire BEFORE DELETE ON fts_dirty \
         FOR EACH ROW WHEN OLD.session_id = '{poisoned_id}' \
         BEGIN SELECT RAISE(ABORT, 'tripwire'); END;"
    );
    store
        .pool()
        .with_conn(move |conn| conn.execute(&create_sql, []).map_err(daow::Error::from))
        .await
        .expect("create tripwire");
}

#[rstest::rstest]
#[tokio::test]
async fn drain_publishes_a_countdown_label_after_each_session() {
    // Given three dirty sessions and a recorder listening for status updates
    // on the same bus the actor publishes to.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    let recorder = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;
    for _ in 0..3 {
        deps.services
            .session_store
            .save(&needle_session(&SessionId::new()))
            .await
            .expect("save");
    }

    // When the actor runs its startup drain with a tiny interval and a
    // batch large enough to finish the whole queue in one heartbeat.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then the drain published the live remaining count after every
    // session — 3, 2, 1 — and "index up to date" once the queue emptied,
    // so the row steps down per session instead of per drain.
    let messages = jinn_domain::common::bus::test_harness::await_recorded(
        &recorder,
        4,
        Duration::from_secs(10),
    )
    .await;
    let labels: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.status_message.as_deref())
        .collect();
    assert_eq!(
        labels,
        vec![
            "3 sessions pending",
            "2 sessions pending",
            "1 sessions pending",
            "index up to date",
        ],
        "one label per completed session, then the idle state"
    );
    // And every message targets the search-index row without touching the
    // identity/lifecycle columns.
    for m in &messages {
        assert_eq!(m.name, "search-index");
        assert_eq!(m.description, None);
        assert_eq!(m.lifecycle, None);
    }
}

#[rstest::rstest]
#[tokio::test]
async fn batch_of_one_reindexes_one_session_per_heartbeat() {
    // Given three dirty sessions and an actor whose batch allows exactly one
    // reindex per heartbeat.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    for _ in 0..3 {
        deps.services
            .session_store
            .save(&needle_session(&SessionId::new()))
            .await
            .expect("save");
    }

    // When the actor runs with a tiny interval.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: 1,
        },
    );

    // Then the pending queue shrinks one session per heartbeat instead of
    // the handler holding itself for the whole queue: each heartbeat yields
    // to the mailbox after its one session and the next one picks the work
    // back up.
    let store = deps.services.session_store.clone();
    let first = poll_until(Duration::from_millis(20), 100, || {
        let store = store.clone();
        async move { store.pending_dirty_count().await.expect("count") <= 2 }
    })
    .await;
    assert!(
        first,
        "first heartbeat should leave at most two sessions pending"
    );
    let second = poll_until(Duration::from_millis(20), 100, || {
        let store = store.clone();
        async move { store.pending_dirty_count().await.expect("count") <= 1 }
    })
    .await;
    assert!(
        second,
        "second heartbeat should leave at most one session pending"
    );
    let third = poll_until(Duration::from_millis(20), 100, || {
        let store = store.clone();
        async move { store.pending_dirty_count().await.expect("count") == 0 }
    })
    .await;
    assert!(third, "backfill should complete across heartbeats");
}

#[rstest::rstest]
#[tokio::test]
async fn heartbeat_processes_at_most_one_batch() {
    // Given fifteen dirty sessions and an actor whose batch is ten with an
    // interval long enough that only the first heartbeat fires.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    for _ in 0..15 {
        deps.services
            .session_store
            .save(&needle_session(&SessionId::new()))
            .await
            .expect("save");
    }

    // When the actor runs its first heartbeat.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_hours(1),
            batch: 10,
        },
    );

    // Then exactly one batch drained: the pending count settles at the five
    // sessions beyond the batch size and — with no second heartbeat
    // possible before the interval elapses — stays there.
    let store = deps.services.session_store.clone();
    let settled = poll_until(Duration::from_millis(20), 100, || {
        let store = store.clone();
        async move { store.pending_dirty_count().await.expect("count") == 5 }
    })
    .await;
    assert!(
        settled,
        "first heartbeat should process exactly one batch of ten"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn redirtied_session_is_requeued_and_reindexed_on_a_later_heartbeat() {
    // Given a session the actor has already fully indexed.
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    let session_id = SessionId::new();
    deps.services
        .session_store
        .save(&needle_session(&session_id))
        .await
        .expect("save");
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );
    let drained = poll_until(Duration::from_millis(20), 100, || {
        let store = deps.services.session_store.clone();
        async move { store.pending_dirty_count().await.expect("count") == 0 }
    })
    .await;
    assert!(drained, "the session should be indexed before re-marking");

    // When the session is re-marked dirty the way a save after indexing
    // would (insert straight into fts_dirty).
    store
        .pool()
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO fts_dirty(session_id) VALUES (?) \
                 ON CONFLICT(session_id) DO NOTHING",
                rusqlite::params![session_id.to_string()],
            )
            .map_err(daow::Error::from)
        })
        .await
        .expect("mark dirty");

    // Then the idle heartbeats pick it back up: the durable queue drains to
    // zero again without any restart.
    let reindexed = poll_until(Duration::from_millis(20), 100, || {
        let store = deps.services.session_store.clone();
        async move { store.pending_dirty_count().await.expect("count") == 0 }
    })
    .await;
    assert!(
        reindexed,
        "re-marked session should be re-queued and re-indexed"
    );
    // And the session stays searchable throughout.
    assert_eq!(
        search_all(&deps.services.session_store).await.total_matches,
        2,
        "session remains searchable"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn countdown_label_counts_every_session_including_the_unprocessed_batch_tail() {
    // Given fifteen dirty sessions, a batch of ten, and a status recorder.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    let recorder = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;
    for _ in 0..15 {
        deps.services
            .session_store
            .save(&needle_session(&SessionId::new()))
            .await
            .expect("save");
    }

    // When the actor drains across two heartbeats (interval well under the
    // wait, so both fire during the assertion window).
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: 10,
        },
    );

    // Then every session decrements the label: the first heartbeat counts
    // its unpopped batch tail (after the first session, 14 = 5 queued + 9
    // in flight), the second heartbeat continues the countdown at its own
    // pre-batch "5" without flashing "index up to date", and the queue
    // empties into the idle label. Later idle heartbeats repeat the idle
    // label, so only the deterministic prefix is asserted.
    let messages = jinn_domain::common::bus::test_harness::await_recorded(
        &recorder,
        17,
        Duration::from_secs(8),
    )
    .await;
    let labels: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.status_message.as_deref())
        .collect();
    let expected: Vec<String> = (5..=15)
        .rev()
        .chain([5, 4, 3, 2, 1])
        .map(|n| format!("{n} sessions pending"))
        .chain(std::iter::once("index up to date".to_owned()))
        .collect();
    assert!(
        labels.len() >= expected.len(),
        "expected the full countdown plus idle label, got {labels:?}"
    );
    let prefix: Vec<&str> = labels.iter().take(expected.len()).copied().collect();
    assert_eq!(
        prefix,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "one decrement per session, counting the in-flight batch tail"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn unreadable_dirty_markers_keep_the_row_out_of_the_up_to_date_state() {
    // Given a store whose only dirty marker is unparseable (dirty_session_ids
    // skips it, pending_dirty_count still counts it) and a status recorder.
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    store
        .pool()
        .execute(
            "INSERT INTO fts_dirty (session_id) VALUES ('not-a-uuid')",
            vec![],
        )
        .await
        .expect("seed corrupt marker");
    let recorder = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;

    // When the actor drains the (effectively empty) queue.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_hours(1),
            batch: 10,
        },
    );

    // Then the row shows the pending label, never "index up to date".
    let messages = jinn_domain::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        Duration::from_secs(10),
    )
    .await;
    let first = messages.first().expect("at least one status update");
    assert_eq!(first.name, "search-index");
    assert_eq!(first.status_message.as_deref(), Some("1 sessions pending"));
}

#[rstest::rstest]
#[tokio::test]
async fn empty_drain_publishes_index_up_to_date() {
    // Given a clean store (nothing dirty) and a recorder for status updates.
    let (_dir, harness, deps, _store) = sqlite_actor_deps().await;
    let recorder = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;

    // When the actor runs (its startup drain fires on an empty queue).
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then the very first published status is already the drained state.
    let messages = jinn_domain::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        Duration::from_secs(10),
    )
    .await;
    let first = messages.first().expect("at least one status update");
    assert_eq!(first.name, "search-index");
    assert_eq!(first.status_message.as_deref(), Some("index up to date"));
}

#[rstest::rstest]
#[tokio::test]
async fn failing_session_still_publishes_progress() {
    // Given a poisoned session (tripwire aborts its rebuild) and a recorder
    // for status updates.
    let (_dir, harness, deps, store) = sqlite_actor_deps().await;
    let recorder = harness
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;
    deps.services
        .session_store
        .save(&needle_session(&SessionId::new()))
        .await
        .expect("save");
    create_reindex_tripwire(&store, &SessionId::from(store_dirty_id(&store).await)).await;

    // When the actor runs its startup drain.
    let _path = crate::search_index_actor::SearchIndexActor::spawn(
        harness.system(),
        SearchIndexActorDeps {
            deps: deps.clone(),
            interval: Duration::from_millis(50),
            batch: usize::MAX,
        },
    );

    // Then progress is still published after the failed operation: the
    // remaining count (the failed session stays pending) reaches the row.
    let messages = jinn_domain::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        messages
            .iter()
            .any(|m| m.status_message.as_deref() == Some("1 sessions pending")),
        "the failed session should be reported as still pending, got: {:?}",
        messages
            .iter()
            .filter_map(|m| m.status_message.as_deref())
            .collect::<Vec<_>>()
    );
}

/// Reads the single dirty session id from the store (test helper for
/// poisoning a specific session's tripwire).
async fn store_dirty_id(store: &SqliteSessionStore) -> String {
    let ids: Vec<String> = store
        .pool()
        .query_all("SELECT session_id AS session_id FROM fts_dirty", vec![])
        .await
        .expect("read dirty");
    ids.into_iter().next().expect("one dirty session")
}
