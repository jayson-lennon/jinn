//! Observable behavior tests for the store-owned session actor.

#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jinn_boot_msg::EnvironmentLoaded;
use jinn_chat_log_view_msg::LayoutChatSession;
use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::state::State;
use jinn_provider_config::ProvidersConfig;
use jinn_session_msg::{SessionArchiveFailed, SessionArchived, SessionClosed};
use jinn_session_state::ChatSessionState;
use jinn_session_state::{SessionStore, SessionStoreService};
use jinn_session_store_msg::{
    ArchiveSession, ArchiveSessionTree, LoadLibraryEntries, LoadSessionPickerEntries,
    PersistSession, SessionLoadCompleted, SessionLoadRequested, SessionState, SetSessionLibrary,
};
use jinn_testutil::bus_harness::{Recorder, TestHarness, await_recorded};

use crate::session_store_actor::{SessionStoreActor, SessionStoreActorDeps};
use crate::session_store_tests_support::ControlledStartupStore;
use crate::sqlite::SqliteSessionStore;

struct ActorFixture {
    _dir: jinn_testutil::JinnTempDir,
    harness: TestHarness,
    state: State,
    store: Arc<SqliteSessionStore>,
    /// The persisted-preferences service, from the actor's own `Services` —
    /// the seeded builder reads the model and reasoning effort through it,
    /// so tests that assert the seed's profile write through this.
    app_state_storage: jinn_preferences_config::AppStateStorageService,
    /// The configuration layer, from the same `Services` — the seeded
    /// builder derives its filters and MCP enablement from it, so tests
    /// that assert the seed's content write through this.
    config: jinn_config::ConfigLayer,
    /// The status bar's cell, taken from the *same* `Slices` the actor holds.
    ///
    /// `HarnessServices::services` mints a fresh `Services` — and so a fresh
    /// registry — on every call, so a test cannot reach the actor's cells
    /// through the harness. The cell is registered on the actor's own
    /// `Services` before the spawn and carried out here for reading.
    status_bar: jinn_slices::cell::TypedCell<jinn_status_bar_msg::StatusBarState>,
    /// The work-time cell, from the same `Slices` — the same reason as
    /// `status_bar`. The fixture spawns the monitor against it, so a restore
    /// publish lands in this cell exactly as production folds it.
    work_time: jinn_slices::cell::TypedCell<jinn_work_time_msg::WorkingTimeState>,
    /// The store actor's path — a test that must know the actor has
    /// processed its inbox drains it through this.
    actor_path: trouper::actor::ActorPath,
    /// The library cell, from the same `Slices` as the actor's own handle —
    /// the same reason as `status_bar`.
    library_cell: jinn_slices::cell::TypedCell<jinn_session_store_msg::LibraryState>,
}

impl ActorFixture {
    /// The library entries currently in the cell.
    fn library_entries(&self) -> Vec<jinn_session_store_msg::SessionSummary> {
        self.library_cell.update(|library| library.entries.clone())
    }

    /// The actor's configuration layer, for tests that seed config sections
    /// the seeded builder reads.
    fn services_config(&self) -> &jinn_config::ConfigLayer {
        &self.config
    }
}

async fn actor_fixture() -> ActorFixture {
    let dir = jinn_testutil::tempdir();
    let store = Arc::new(
        SqliteSessionStore::new_in(&dir.path)
            .await
            .expect("session store"),
    );
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.session_store = SessionStoreService::new(store.clone());
    // The picker cell production's `activate` mints before the spawn; a
    // fixture that skipped it would fail at construction, not at use.
    // The harness seeds the registry through the shared cell catalog, so
    // the picker cell already exists; resolve it rather than minting a
    // second one over the top.
    let session_picker_cell = services
        .slices
        .reader::<jinn_session_store_msg::SessionPickerState>(
            &jinn_session_store_msg::session_picker_slot(),
        )
        .expect("the cell catalog registers the session picker slot");
    // The harness seeds the registry through the shared cell catalog, so the
    // status-bar cell already exists; resolve it so the hint assertion
    // below cannot pass vacuously against a missing cell.
    let status_bar = services
        .slices
        .reader::<jinn_status_bar_msg::StatusBarState>(&jinn_status_bar_msg::status_bar_slot())
        .expect("the cell catalog registers the status-bar slot");
    let work_time = services
        .slices
        .reader::<jinn_work_time_msg::WorkingTimeState>(&jinn_work_time_msg::work_time_slot())
        .expect("the cell catalog registers the work-time slot");
    let library_cell = services
        .slices
        .reader::<jinn_session_store_msg::LibraryState>(&jinn_session_store_msg::library_slot())
        .expect("the cell catalog registers the library slot");
    let state = State::new(AppState::default());
    let app_state_storage = services.app_state_storage.clone();
    let config = services.config.clone();
    let actor_path = SessionStoreActor::spawn(
        harness.system(),
        SessionStoreActorDeps {
            services,
            state: state.clone(),
            session_picker_cell: session_picker_cell.clone(),
            library_cell: library_cell.clone(),
        },
    );
    // The monitor is the work-time cell's single writer, and the load path's
    // restore publish only shows up in the cell through it.
    let _monitor = jinn_work_time::WorkTimeMonitorActor::spawn(harness.system(), work_time.clone());
    ActorFixture {
        _dir: dir,
        harness,
        state,
        store,
        app_state_storage,
        config,
        status_bar,
        work_time,
        actor_path,
        library_cell,
    }
}

/// The controlled fixture: the harness, the state, the picker cell, and
/// the store actor's path (a test that must know the actor has processed
/// its inbox drains it through that path).
type ControlledFixture = (
    TestHarness,
    State,
    jinn_slices::cell::TypedCell<jinn_session_store_msg::SessionPickerState>,
    trouper::actor::ActorPath,
);

/// The controlled fixture, over any store implementation.
///
/// Most tests need `ControlledStartupStore`'s gating and counters; a test that
/// has to provoke a particular store failure brings a store of its own.
async fn controlled_actor_fixture(store: Arc<dyn SessionStore>) -> ControlledFixture {
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.session_store = SessionStoreService::new(store);
    // The harness seeds the registry through the shared cell catalog, so the
    // picker cell already exists; resolve it rather than minting a second
    // one over the top (which the catalog would report as `SlotTaken`).
    let session_picker_cell = services
        .slices
        .reader::<jinn_session_store_msg::SessionPickerState>(
            &jinn_session_store_msg::session_picker_slot(),
        )
        .expect("the cell catalog registers the session picker slot");
    let library_cell = services
        .slices
        .reader::<jinn_session_store_msg::LibraryState>(&jinn_session_store_msg::library_slot())
        .expect("the cell catalog registers the library slot");
    let state = State::new(AppState::default());
    let actor_path = SessionStoreActor::spawn(
        harness.system(),
        SessionStoreActorDeps {
            services,
            state: state.clone(),
            session_picker_cell: session_picker_cell.clone(),
            library_cell,
        },
    );
    (harness, state, session_picker_cell, actor_path)
}

fn empty_providers_config() -> ProvidersConfig {
    ProvidersConfig {
        providers: BTreeMap::new(),
        aliases: Vec::new(),
        default_provider: None,
        endpoint_defaults: Vec::new(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_is_served_while_startup_session_loads_are_still_outstanding() {
    // Given two persisted sessions whose reads are both gated shut.
    let first_id = SessionId::new();
    let second_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            first_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            second_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    store.gate_session_load(first_id.clone());
    store.gate_session_load(second_id.clone());
    let (harness, state, picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration is triggered, and the picker's message is
    // published behind it on the same mailbox.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    store.wait_for_session_load(&first_id).await;
    store.wait_for_session_load(&second_id).await;
    harness.publish(LoadSessionPickerEntries).await;
    let picker_served =
        jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !picker_cell.read().tree.items().is_empty()
        })
        .await;

    // Then the picker is served even though not one session read has finished.
    // Under the old inline loop this message sat in the mailbox until every
    // history had been read — which is the whole stall this pool removes.
    assert!(
        picker_served,
        "the picker must not wait behind startup hydration's reads"
    );
    // And neither session is visible yet, because both reads are still gated.
    assert!(!state.read().session.contains(&first_id));
    assert!(!state.read().session.contains(&second_id));
}

#[rstest::rstest]
#[tokio::test]
async fn the_hydration_flag_clears_only_after_the_last_completion() {
    // Given two persisted sessions, with the newer one's read gated shut.
    let gated_id = SessionId::new();
    let loaded_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            loaded_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            gated_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    store.gate_session_load(gated_id.clone());
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When the ungated session's read completes while the other is still out.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        state.read().session.contains(&loaded_id)
    })
    .await;

    // Then hydration is still active: one completion is not all of them.
    assert!(loaded, "the ungated session should have loaded");
    assert!(
        state.read().session.is_startup_hydrating(),
        "hydration must stay active while a read is still outstanding"
    );

    // And when the last read is released, the flag clears.
    store.release_session_load(&gated_id);
    let cleared = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !state.read().session.is_startup_hydrating()
    })
    .await;
    assert!(cleared, "the last completion must clear the hydration flag");
}

#[rstest::rstest]
#[tokio::test]
async fn a_frozen_tree_member_is_stored_without_reopening_the_hydration_flag() {
    // Given a persisted session whose read resolves, plus a second session that
    // the store reports as a tree member.
    let root_id = SessionId::new();
    let member_id = SessionId::new();
    let store = Arc::new({
        let mut store = ControlledStartupStore::new(&[
            (
                root_id.clone(),
                jiff::Timestamp::from_second(1).expect("valid timestamp"),
            ),
            (
                member_id.clone(),
                jiff::Timestamp::from_second(2).expect("valid timestamp"),
            ),
        ]);
        store.summaries[0].parent_session = None;
        store.summaries[1].parent_session = Some(root_id.clone());
        store
            .archived_only_ids
            .lock()
            .expect("archived-only IDs")
            .push(member_id.clone());
        store
    });
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store).await;

    // When startup hydration runs to completion.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let member_frozen =
        jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            state.read().session.frozen_nodes().contains_key(&member_id)
        })
        .await;

    // Then the tree member is stored as a frozen node, not a live session.
    assert!(member_frozen, "the tree member should be frozen");
    assert!(!state.read().session.contains(&member_id));
    // And the hydration flag stays clear: the frozen wave is counted separately
    // and must not resurrect the indicator the unarchived wave already finished.
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn every_dispatched_load_produces_exactly_one_completion() {
    // Given two persisted sessions.
    let first_id = SessionId::new();
    let second_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            first_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            second_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    let (harness, _state, _picker_cell, _actor_path) =
        controlled_actor_fixture(store.clone()).await;
    let completed = harness.spawn_recorder::<SessionLoadCompleted>().await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let completed = await_recorded(&completed, 2, Duration::from_secs(2)).await;

    // Then each session reports exactly one completion — no job dropped, none
    // counted twice. A dropped job would strand the hydration flag forever.
    let mut ids = completed
        .iter()
        .map(|msg| msg.session_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    let mut expected = vec![first_id, second_id];
    expected.sort();
    assert_eq!(ids, expected);
}

#[rstest::rstest]
#[tokio::test]
async fn startup_hydration_is_visible_before_first_snapshot_completes() {
    // Given a persisted session and a gated summary read.
    let session_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[(
        session_id.clone(),
        jiff::Timestamp::from_second(1).expect("valid timestamp"),
    )]));
    store.gate_session_load(session_id.clone());
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration begins.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    store.wait_for_session_load(&session_id).await;

    // Then the shared projection reports hydration active.
    assert!(state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn first_startup_session_is_visible_before_second_snapshot_load() {
    // Given two persisted sessions with the newer session loaded first.
    let newer_id = SessionId::new();
    let older_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            older_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            newer_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    store.gate_session_load(newer_id.clone());
    store.gate_session_load(older_id.clone());
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When the newer snapshot is released but the older snapshot remains gated.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    store.wait_for_session_load(&newer_id).await;
    store.release_session_load(&newer_id);
    let newer_visible =
        jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            state.read().session.contains(&newer_id)
        })
        .await;
    assert!(newer_visible);

    // Then only the first session is visible while the second read is pending.
    store.wait_for_session_load(&older_id).await;
    assert!(!state.read().session.contains(&older_id));
}

#[rstest::rstest]
#[tokio::test]
async fn startup_sessions_are_inserted_in_existing_recency_order() {
    // Given two persisted sessions with distinct recency timestamps.
    let older_id = SessionId::new();
    let newer_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            older_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            newer_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    let (harness, _state, _picker_cell, _actor_path) =
        controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        store.load_calls.load(Ordering::SeqCst) == 2
    })
    .await;

    // Then the store reads the snapshots in newest-first order.
    assert_eq!(
        store
            .requested_session_ids
            .lock()
            .expect("requested session IDs")
            .as_slice(),
        &[newer_id, older_id]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn startup_publishes_one_completion_event_per_loaded_session() {
    // Given two persisted sessions.
    let first_id = SessionId::new();
    let second_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            first_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            second_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    let (harness, _state, _picker_cell, _actor_path) =
        controlled_actor_fixture(store.clone()).await;
    let completed = harness.spawn_recorder::<SessionLoadCompleted>().await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let completed = await_recorded(&completed, 2, Duration::from_secs(1)).await;

    // Then one completion event is published for each session.
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].session_id, second_id);
    assert_eq!(completed[1].session_id, first_id);
}

#[rstest::rstest]
#[tokio::test]
async fn startup_hydration_clears_before_archived_tree_hydration() {
    // Given a persisted session and a gated archived-tree summary read.
    let session_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[(
        session_id.clone(),
        jiff::Timestamp::from_second(1).expect("valid timestamp"),
    )]));
    store.gate_tree_summary_load();
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When the unarchived session is inserted and tree hydration begins.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        state.read().session.contains(&session_id)
    })
    .await;
    store.wait_for_tree_summary_load().await;

    // Then the shared hydration projection is already inactive.
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn empty_startup_clears_hydration() {
    // Given a store with no unarchived sessions.
    let store = Arc::new(ControlledStartupStore::new(&[]));
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        store.unarchived_summary_calls.load(Ordering::SeqCst) > 0
            && !state.read().session.is_startup_hydrating()
    })
    .await;

    // Then hydration is inactive.
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn summary_query_failure_clears_hydration() {
    // Given a store whose unarchived summary query fails.
    let store = Arc::new(ControlledStartupStore::new(&[]));
    store.fail_summaries();
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration is attempted.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        store.unarchived_summary_calls.load(Ordering::SeqCst) > 0
            && !state.read().session.is_startup_hydrating()
    })
    .await;

    // Then hydration is inactive.
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn individual_load_failure_does_not_abort_remaining_startup_loads() {
    // Given one failed snapshot and one successful newer snapshot.
    let failed_id = SessionId::new();
    let loaded_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[
        (
            failed_id.clone(),
            jiff::Timestamp::from_second(1).expect("valid timestamp"),
        ),
        (
            loaded_id.clone(),
            jiff::Timestamp::from_second(2).expect("valid timestamp"),
        ),
    ]));
    store.fail_session(failed_id);
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store).await;

    // When startup hydration continues past the failed snapshot.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        state.read().session.contains(&loaded_id)
    })
    .await;

    // Then the successful session is still loaded and hydration is inactive.
    assert!(state.read().session.contains(&loaded_id));
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn startup_preserves_welcome_session_as_active() {
    // Given a persisted session and a running store actor.
    let persisted_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[(
        persisted_id.clone(),
        jiff::Timestamp::from_second(1).expect("valid timestamp"),
    )]));
    let (harness, state, _picker_cell, _actor_path) = controlled_actor_fixture(store).await;
    let welcome_id = state.read().session.active_session_id().clone();

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        state.read().session.contains(&persisted_id)
    })
    .await;

    // Then the persisted session is visible without changing the active welcome session.
    assert_eq!(state.read().session.active_session_id(), &welcome_id);
}

#[rstest::rstest]
#[tokio::test]
async fn startup_does_not_persist_hydrated_sessions() {
    // Given a persisted session and a controllable store.
    let persisted_id = SessionId::new();
    let store = Arc::new(ControlledStartupStore::new(&[(
        persisted_id.clone(),
        jiff::Timestamp::from_second(1).expect("valid timestamp"),
    )]));
    let (harness, _state, _picker_cell, _actor_path) =
        controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        store.unarchived_summary_calls.load(Ordering::SeqCst) == 1
    })
    .await;
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        store.all_summary_calls.load(Ordering::SeqCst) == 1
    })
    .await;

    // Then the startup path does not write the session back to storage.
    assert_eq!(store.save_calls.load(Ordering::SeqCst), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn load_completed_is_published_after_session_is_fully_initialized() {
    // Given a session persisted in the store and removed from the live map.
    let fixture = actor_fixture().await;
    let completed = fixture
        .harness
        .spawn_recorder::<SessionLoadCompleted>()
        .await;
    let session_id = jinn_core_types::SessionId::new();
    let mut stored = ChatSessionState::new();
    stored.set_session_id(session_id.clone());
    stored.set_model(jinn_core_types::ModelSelection::Single(
        "ollama/llama3".to_owned(),
    ));
    stored.push_entry(jinn_core_types::ChatEntry::user("loaded"));
    fixture
        .store
        .save(&stored.capture_snapshot())
        .await
        .expect("save session");
    {
        let mut state = fixture.state.write();
        state.session.remove(&session_id);
        state.session.begin_load(session_id.clone());
    }

    // When the load request is published.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;

    // Then the completion ID resolves to a fully initialized live session.
    let completed = await_recorded(&completed, 1, Duration::from_secs(1)).await;
    assert_eq!(completed[0].session_id, session_id);
    let state = fixture.state.read();
    let session = state.session.get(&session_id).expect("loaded session");
    assert_eq!(state.session.active_session_id(), &session_id);
    assert!(session.has_interacted());
}

#[rstest::rstest]
#[tokio::test]
async fn a_loaded_session_holds_the_load_guard_for_the_chat_log_measurement() {
    // Given a session persisted in the store and removed from the live map.
    let fixture = actor_fixture().await;
    let session_id = jinn_core_types::SessionId::new();
    let mut stored = ChatSessionState::new();
    stored.set_session_id(session_id.clone());
    stored.set_model(jinn_core_types::ModelSelection::Single(
        "ollama/llama3".to_owned(),
    ));
    stored.push_entry(jinn_core_types::ChatEntry::user("loaded"));
    fixture
        .store
        .save(&stored.capture_snapshot())
        .await
        .expect("save session");
    {
        let mut state = fixture.state.write();
        state.session.remove(&session_id);
        state.session.begin_load(session_id.clone());
    }

    // When the load request is published.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;
    // The store actor must have processed the command before the state
    // is read back. A fixed sleep could let the read happen before
    // the actor wrote it, and the assertion would then prove nothing.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the guard is still held, because the chat log has not been
    // measured yet — this fixture has no layout workers, which is exactly the
    // path the supervisor's deadline is the backstop for.
    assert!(
        fixture.state.read().session.is_loading(),
        "the load guard must outlive the disk read so the chat log can measure first"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn restored_session_is_marked_loaded() {
    // Given a session persisted to storage in the archived state.
    let fixture = actor_fixture().await;
    let session_id = SessionId::new();
    let mut stored = ChatSessionState::new();
    stored.set_session_id(session_id.clone());
    stored.set_model(jinn_core_types::ModelSelection::Single(
        "ollama/llama3".to_owned(),
    ));
    stored.set_session_state(SessionState::Archived);
    fixture
        .store
        .save(&stored.capture_snapshot())
        .await
        .expect("save session");
    fixture
        .store
        .set_archived(&session_id, true)
        .await
        .expect("archive session");

    // When the session is loaded back.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;
    let restored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|session| session.session_state() == SessionState::Loaded)
    })
    .await;

    // Then the in-memory session is no longer archived.
    assert!(
        restored,
        "a loaded session must not stay archived in memory"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn loaded_from_archive_appears_in_the_session_list() {
    // Given a session archived in storage and absent from the live map.
    let fixture = actor_fixture().await;
    let session_id = SessionId::new();
    let mut stored = ChatSessionState::new();
    stored.set_session_id(session_id.clone());
    stored.set_model(jinn_core_types::ModelSelection::Single(
        "ollama/llama3".to_owned(),
    ));
    stored.set_session_state(SessionState::Archived);
    stored.push_entry(jinn_core_types::ChatEntry::user("archived work"));
    fixture
        .store
        .save(&stored.capture_snapshot())
        .await
        .expect("save session");
    fixture
        .store
        .set_archived(&session_id, true)
        .await
        .expect("archive session");
    // Given the session is archived and out of the live map, which is the
    // state an archived session is actually in. A session still sitting in the
    // map is no longer re-read: the store actor recognises it as in memory and
    // only measures it, which would leave it Archived.
    assert!(
        fixture.state.read().session.get(&session_id).is_none(),
        "the archived session starts out of the map"
    );

    // When the session is loaded back.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            // No width on offer: the caller is not a view switching sessions,
            // so the store actor derives the one it would render at.
            content_width: None,
        })
        .await;

    // Then the session carries the state the sidebar lists.
    let listed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture.state.read().session.iter().any(|(id, session)| {
            id == &session_id && session.session_state() == SessionState::Loaded
        })
    })
    .await;

    // Then the sidebar's loaded-session filter includes it.
    assert!(
        listed,
        "a session loaded from the archive must be Loaded and listed again"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_reloaded_from_storage_can_be_archived_again() {
    // Given a session saved, archived, and then loaded back — the cycle a
    // user reaches by picking the same session out of the picker twice.
    let (fixture, session_id) = {
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut live = ChatSessionState::new();
        live.set_session_id(session_id.clone());
        live.set_title("lifecycle work".to_owned());
        live.mark_interacted();
        live.push_entry(jinn_core_types::ChatEntry::user("first pass"));
        fixture.state.with_session(|view| {
            view.session.map().insert(live.clone());
        });
        fixture
            .store
            .save(&live.capture_snapshot())
            .await
            .expect("save session");
        (fixture, session_id)
    };
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let archived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.contains(&session_id)
    })
    .await;
    assert!(archived, "the first archive should remove the session");
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;
    let reloaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|session| session.session_state() == SessionState::Loaded)
    })
    .await;
    assert!(reloaded, "the session should be live again after a reload");

    // When it is archived a second time.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then the second archive lands, instead of being refused as a stale write.
    let removed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.contains(&session_id)
    })
    .await;
    assert!(removed, "a reloaded session must be archivable again");
}

#[rstest::rstest]
#[tokio::test]
async fn loading_a_session_persists_its_reactivation() {
    // Given a session saved and left in the store, absent from the live map.
    let fixture = actor_fixture().await;
    let session_id = SessionId::new();
    let mut stored = ChatSessionState::new();
    stored.set_session_id(session_id.clone());
    stored.set_title("reactivated".to_owned());
    stored.mark_interacted();
    stored.push_entry(jinn_core_types::ChatEntry::user("work"));
    fixture
        .store
        .save(&stored.capture_snapshot())
        .await
        .expect("save session");
    let before = fixture
        .store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("stored session")
        .metadata
        .updated_at;

    // When the session is loaded back. Loading writes to the store on its own:
    // the reactivated session is stamped as touched, and the sidebar's recency
    // order is driven by that stamp.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;

    // Then the reactivation is written, instead of being dropped as stale.
    let persisted = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .store
            .load_session(&session_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|snapshot| snapshot.metadata.updated_at > before)
    })
    .await;

    assert!(
        persisted,
        "a reloaded session's own save must not be silently skipped"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn archiving_a_tree_archives_a_member_that_is_not_live() {
    // Given a parent with a persisted child, both written to the store more
    // than once and neither live — the shape a reopened app's sessions have.
    let fixture = actor_fixture().await;
    let parent_id = SessionId::new();
    let child_id = SessionId::new();
    let mut child = ChatSessionState::new_child(&parent_id, true);
    child.set_session_id(child_id.clone());
    child.mark_interacted();
    child.push_entry(jinn_core_types::ChatEntry::user("child work"));
    let mut parent = ChatSessionState::new();
    parent.set_session_id(parent_id.clone());
    parent.mark_interacted();
    parent.push_entry(jinn_core_types::ChatEntry::user("parent work"));
    for session in [&parent, &child] {
        for _ in 0..3 {
            fixture
                .store
                .save(&session.capture_snapshot())
                .await
                .expect("save session");
        }
    }

    // When the tree is archived from the parent.
    fixture
        .harness
        .publish(ArchiveSessionTree {
            root: parent_id.clone(),
        })
        .await;

    // Then the not-live child is archived durably, not skipped as a stale write.
    let archived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        let summaries = fixture
            .store
            .load_summaries()
            .await
            .expect("load summaries");
        summaries
            .iter()
            .all(|summary| summary.session_state == SessionState::Archived)
    })
    .await;
    assert!(
        archived,
        "a tree member absent from the live map must still be archived"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_failed_archive_raises_a_hint_naming_the_session() {
    // Given an active session and a store whose archive transaction will fail,
    // with the status bar's cell registered by the fixture — the slice is not
    // activated in this harness, and an unregistered cell would drop the hint
    // silently and make this assertion vacuous.
    let fixture = actor_fixture().await;
    let session_id = {
        let mut state = fixture.state.write();
        state.active_session_mut().mark_interacted();
        state
            .active_session_mut()
            .push_entry(jinn_core_types::ChatEntry::user("keep me"));
        state.session.active_session_id().clone()
    };
    fixture
        .store
        .pool()
        .execute("DROP TABLE token_ledger", vec![])
        .await
        .expect("drop token ledger");

    // When archiving the session.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let hinted = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture.status_bar.read().hint.is_some()
    })
    .await;

    // Then the failure is on screen, naming the session that would not archive.
    let hint = fixture.status_bar.read().hint.clone();
    assert!(hinted, "a failed archive must raise a status hint");
    assert!(
        hint.as_deref()
            .is_some_and(|hint| hint.contains(&session_id.to_string())),
        "the hint must name the session that failed to archive, got {hint:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persist_session_does_not_write_a_session_that_is_not_worth_saving() {
    // Given a running store actor and a brand new session: persistable in
    // principle, but carrying no turn and no children, so it is not yet
    // worth a row.
    let fixture = actor_fixture().await;
    let session_id = {
        let state = fixture.state.read();
        state.active_session().session_id().clone()
    };
    assert!(
        !fixture.state.read().active_session().is_persistable(),
        "a brand new session is not persistable — that is the premise"
    );

    // When PersistSession is published for it.
    fixture
        .harness
        .publish(PersistSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then nothing is written. A caller that marks a session as worth
    // keeping is what makes the save take effect; publishing the request
    // alone is not sufficient, and a test that only ever starts from an
    // already-interacted session cannot see the difference.
    // The store actor must have processed the command before the state
    // is read back. A fixed sleep could let the read happen before
    // the actor wrote it, and the assertion would then prove nothing.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;
    let saved = fixture
        .store
        .load_session(&session_id)
        .await
        .ok()
        .flatten()
        .is_some();
    assert!(
        !saved,
        "a session with nothing to keep must not reach the store"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persist_session_writes_interacted_session_to_store() {
    // Given a running store actor and an interacted session.
    let fixture = actor_fixture().await;
    let session_id = {
        let mut state = fixture.state.write();
        let session = state.active_session_mut();
        session.mark_interacted();
        session.push_entry(jinn_core_types::ChatEntry::user("persist me"));
        session.session_id().clone()
    };

    // When PersistSession is published.
    fixture
        .harness
        .publish(PersistSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then the full session reaches the store service.
    let saved = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .store
            .load_session(&session_id)
            .await
            .ok()
            .flatten()
            .is_some()
    })
    .await;
    assert!(
        saved,
        "PersistSession should write the session to the store"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persist_session_refreshes_the_library_cell_for_a_member() {
    // Given a running store actor whose session is saved and flagged as a
    // library member, so its summary row is in the cell under the old title.
    let fixture = actor_fixture().await;
    let session_id = {
        let mut state = fixture.state.write();
        let session = state.active_session_mut();
        session.mark_interacted();
        session.push_entry(jinn_core_types::ChatEntry::user("persist me"));
        session.set_in_library(true);
        session.session_id().clone()
    };
    let seed = fixture.store.load_session(&session_id).await.ok().flatten();
    if seed.is_none() {
        fixture
            .store
            .save(&fixture.state.read().active_session().capture_snapshot())
            .await
            .expect("seed member row");
    }
    fixture
        .store
        .set_in_library(&session_id, true)
        .await
        .expect("flag the member");
    fixture.harness.publish(LoadLibraryEntries).await;
    let seeded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .library_cell
            .read()
            .entries
            .iter()
            .any(|member| member.session_id == session_id)
    })
    .await;
    assert!(seeded, "precondition: the member is in the cell");

    // When the session is renamed and PersistSession is published — the
    // rename confirm's exact chain.
    let renamed = {
        let mut state = fixture.state.write();
        let session = state.active_session_mut();
        session.set_title("renamed member".to_owned());
        session.session_id().clone()
    };
    fixture
        .harness
        .publish(PersistSession {
            session_id: renamed,
        })
        .await;

    // Then the cell shows the new title without any section re-entry —
    // the persist itself scheduled the refresh.
    let retitle = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .library_cell
            .read()
            .entries
            .iter()
            .any(|member| member.title == "renamed member")
    })
    .await;
    assert!(
        retitle,
        "a persist of a member must refresh the library cell; entries: {:?}",
        fixture.library_cell.read().entries
    );
}

#[rstest::rstest]
#[tokio::test]
async fn archive_session_removes_session_from_state() {
    // Given a running store actor and an active persisted session.
    let fixture = actor_fixture().await;
    let session_id = {
        let mut state = fixture.state.write();
        state.active_session_mut().mark_interacted();
        state.session.active_session_id().clone()
    };

    // When ArchiveSession is published.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then the session is removed from shared state.
    let removed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.contains(&session_id)
    })
    .await;
    assert!(removed, "ArchiveSession should remove the archived session");
}

#[rstest::rstest]
#[tokio::test]
async fn archive_write_failure_leaves_session_live_and_active() {
    // Given an active session and a store whose archive transaction will fail
    // after the session row and history writes.
    let fixture = actor_fixture().await;
    let archived = fixture.harness.spawn_recorder::<SessionArchived>().await;
    let session_id = {
        let mut state = fixture.state.write();
        state.active_session_mut().mark_interacted();
        state
            .active_session_mut()
            .push_entry(jinn_core_types::ChatEntry::user("keep me"));
        state.session.active_session_id().clone()
    };
    fixture
        .store
        .pool()
        .execute("DROP TABLE token_ledger", vec![])
        .await
        .expect("drop token ledger");

    // When archiving the session.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    // The store actor must have processed the command before the state
    // is read back. A fixed sleep could let the read happen before
    // the actor wrote it, and the assertion would then prove nothing.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the failed transaction leaves the session live, active, and unclosed.
    let state = fixture.state.read();
    assert!(state.session.contains(&session_id));
    assert_eq!(state.session.active_session_id(), &session_id);
    assert!(archived.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn archive_session_publishes_session_archived_event() {
    // Given a running store actor and a recorder for the archive event.
    let fixture = actor_fixture().await;
    let archived = fixture.harness.spawn_recorder::<SessionArchived>().await;
    let session_id = {
        let mut state = fixture.state.write();
        state.active_session_mut().mark_interacted();
        state.session.active_session_id().clone()
    };

    // When ArchiveSession is published.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then SessionArchived is published for the archived session.
    let archived = await_recorded(&archived, 1, Duration::from_secs(1)).await;
    assert_eq!(archived[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_session_publishes_session_closed_event() {
    // Given a running store actor and a recorder for the close event.
    let fixture = actor_fixture().await;
    let closed = fixture.harness.spawn_recorder::<SessionClosed>().await;
    let session_id = {
        let mut state = fixture.state.write();
        state.active_session_mut().mark_interacted();
        state.session.active_session_id().clone()
    };

    // When ArchiveSession is published.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then SessionClosed is published for the archived session.
    let closed = await_recorded(&closed, 1, Duration::from_secs(1)).await;
    assert_eq!(closed[0].session_id, session_id);
}

// ---------------------------------------------------------------------------
// Chat log measurement of an in-memory session
// ---------------------------------------------------------------------------

/// A running store actor with a second hydrated session, ready to measure.
///
/// The session store actor is given the measurement recorder, so the layout
/// jobs it dispatches reach it; the workers themselves are not installed, so
/// the cache stays empty and what the measurement produced is observable.
async fn measure_fixture() -> (ActorFixture, Recorder<LayoutChatSession>, SessionId) {
    let fixture = actor_fixture().await;
    let jobs = fixture.harness.spawn_recorder::<LayoutChatSession>().await;
    let target_id = SessionId::new();
    {
        let mut state = fixture.state.write();
        let mut target = ChatSessionState::new();
        target.set_session_id(target_id.clone());
        target.push_entry(jinn_core_types::ChatEntry::user("from the sidebar"));
        state.session.insert(target);
        // The session on screen has a width the next frame will inherit.
        state.active_session_mut().set_content_width(72);
        state.session.begin_load(target_id.clone());
    }
    (fixture, jobs, target_id)
}

#[rstest::rstest]
#[tokio::test]
async fn measuring_an_in_memory_session_dispatches_a_layout_job() {
    // Given a hydrated session waiting to be measured.
    let (fixture, jobs, target_id) = measure_fixture().await;

    // When the measurement is requested.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;

    // Then the session's history is handed to the layout workers.
    let jobs = await_recorded(&jobs, 1, Duration::from_secs(2)).await;
    assert_eq!(jobs.len(), 1, "one session must produce one layout job");
    assert_eq!(jobs[0].session_id, target_id);
    // And the job carries the entries to measure.
    assert_eq!(jobs[0].entries.len(), 1);
    assert_eq!(jobs[0].entries[0].text(), "from the sidebar");
}

#[rstest::rstest]
#[tokio::test]
async fn a_layout_job_from_a_measurement_measures_at_the_width_on_screen() {
    // Given a hydrated session, and an on-screen session that last rendered
    // at 72 columns.
    let (fixture, jobs, target_id) = measure_fixture().await;

    // When the measurement is requested.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;

    // Then the job is measured at the width the next frame will use.
    //
    // The incoming session has never rendered, so its own width is stale;
    // measuring at that would throw the whole measurement away.
    let jobs = await_recorded(&jobs, 1, Duration::from_secs(2)).await;
    assert_eq!(jobs[0].content_width, 72);
}

#[rstest::rstest]
#[tokio::test]
async fn a_layout_job_from_a_measurement_makes_the_session_active() {
    // Given a hydrated session that is not yet active.
    let (fixture, jobs, target_id) = measure_fixture().await;
    let before = fixture.state.read().session.active_session_id().clone();

    // When the measurement is requested.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;
    await_recorded(&jobs, 1, Duration::from_secs(2)).await;

    // Then the completion actor will find it on screen.
    let state = fixture.state.read();
    assert_ne!(
        before, target_id,
        "the fixture must start on another session"
    );
    assert_eq!(state.session.active_session_id(), &target_id);
}

#[rstest::rstest]
#[tokio::test]
async fn measuring_an_absent_session_clears_the_load_guard() {
    // Given a session id that is not in the live map, with the guard held.
    let fixture = actor_fixture().await;
    let missing_id = SessionId::new();
    {
        let mut state = fixture.state.write();
        state.session.begin_load(missing_id.clone());
    }
    let jobs = fixture.harness.spawn_recorder::<LayoutChatSession>().await;

    // When the measurement is requested for it.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: missing_id.clone(),
            content_width: Some(72),
        })
        .await;
    // The store actor must have processed the command before the state
    // is read back. A fixed sleep could let the read happen before
    // the actor wrote it, and the assertion would then prove nothing.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the guard is cleared, so the user is not stranded on a spinner.
    assert!(
        !fixture.state.read().session.is_loading(),
        "a measurement that can never run must not hold the guard"
    );
    // And no work is dispatched for a session that does not exist.
    assert!(
        jobs.is_empty(),
        "an absent session must not produce a layout job"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn measuring_an_in_memory_session_never_reads_it_from_the_store() {
    // Given a hydrated session that is not persisted.
    let (fixture, jobs, target_id) = measure_fixture().await;

    // When the measurement is requested.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;
    await_recorded(&jobs, 1, Duration::from_secs(2)).await;

    // Then the store was never asked for it — the whole point of measuring an
    // in-memory session rather than routing through a load.
    let stored = fixture.store.load_session(&target_id).await.ok().flatten();
    assert!(
        stored.is_none(),
        "the session was never persisted, so no disk read could have served it"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_measured_request_clears_the_load_guard_end_to_end() {
    // Given the full layout subsystem, and an in-memory session to measure.
    let fixture = actor_fixture().await;
    jinn_chat_log_view::kernel_element::install_layout_actors(
        fixture.harness.system(),
        fixture.state.clone(),
    );
    let target_id = SessionId::new();
    {
        let mut state = fixture.state.write();
        let mut target = ChatSessionState::new();
        target.set_session_id(target_id.clone());
        for index in 0..40 {
            target.push_entry(jinn_core_types::ChatEntry::user(format!(
                "a reasonably long message number {index} that will wrap a few times"
            )));
        }
        state.session.insert(target);
        state.active_session_mut().set_content_width(72);
        state.session.begin_load(target_id.clone());
    }

    // When the measurement is requested.
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;

    // Then the guard is released by the real worker pool and completion actor.
    let released = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.is_loading()
    })
    .await;
    assert!(
        released,
        "the real layout pipeline must clear the guard it was asked to satisfy"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn measuring_after_the_frontend_switched_measures_at_a_usable_width() {
    // Given the full layout subsystem and an in-memory session.
    let fixture = actor_fixture().await;
    jinn_chat_log_view::kernel_element::install_layout_actors(
        fixture.harness.system(),
        fixture.state.clone(),
    );
    let target_id = SessionId::new();
    {
        let mut state = fixture.state.write();
        let mut target = ChatSessionState::new();
        target.set_session_id(target_id.clone());
        for index in 0..40 {
            target.push_entry(jinn_core_types::ChatEntry::user(format!(
                "a reasonably long message number {index} that will wrap a few times"
            )));
        }
        state.session.insert(target);
        // The session on screen last rendered at 72 columns.
        state.active_session_mut().set_content_width(72);
    }

    // When the frontend switches first, then asks for the measurement — which
    // is the order the sidebar activation uses.
    let jobs = fixture.harness.spawn_recorder::<LayoutChatSession>().await;
    fixture.state.write().session.set_active(target_id.clone());
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: target_id.clone(),
            content_width: Some(72),
        })
        .await;

    // Then the job is measured at the width the chat log is rendering at.
    let jobs = await_recorded(&jobs, 1, Duration::from_secs(2)).await;
    assert_eq!(jobs[0].content_width, 72);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_tree_aborted_by_busy_member_reports_failure_for_every_member() {
    // Given a parent session with one busy child, both live in the session map.
    let fixture = actor_fixture().await;
    let mut parent = ChatSessionState::new();
    let parent_session_id = parent.session_id().clone();
    let mut child = ChatSessionState::new();
    let child_id = child.session_id().clone();
    child.set_parent_session(parent_session_id.clone());
    child.begin_streaming();
    parent.set_parent_session(SessionId::new());
    {
        let mut state = fixture.state.write();
        state.session.insert(parent);
        state.session.insert(child);
    }
    let failures = fixture
        .harness
        .spawn_recorder::<SessionArchiveFailed>()
        .await;

    // When archiving the tree, whose own guard rejects the busy member.
    fixture
        .harness
        .publish(ArchiveSessionTree {
            root: parent_session_id.clone(),
        })
        .await;
    let reported = await_recorded(&failures, 2, Duration::from_secs(2)).await;

    // Then both members are reported, so both tinted rows are cleared.
    let mut ids = reported
        .iter()
        .map(|msg| msg.session_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    let mut expected = vec![parent_session_id, child_id];
    expected.sort();
    assert_eq!(ids, expected, "every tinted member must be cleared");
}

/// Activation is one command, and the store actor decides what it means.
///
/// A caller cannot: only the actor that owns the session map knows what is
/// loaded, and a caller that guesses pays for a redundant disk read of a
/// session it already had — which is what the picker's Enter did before the two
/// commands were merged.
mod activation_tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    /// A persisted session, out of the live map.
    async fn persisted_but_absent(fixture: &ActorFixture) -> jinn_core_types::SessionId {
        let session_id = jinn_core_types::SessionId::new();
        let mut stored = ChatSessionState::new();
        stored.set_session_id(session_id.clone());
        stored.set_model(jinn_core_types::ModelSelection::Single(
            "ollama/llama3".to_owned(),
        ));
        stored.push_entry(jinn_core_types::ChatEntry::user("work"));
        fixture
            .store
            .save(&stored.capture_snapshot())
            .await
            .expect("save session");
        fixture.state.write().session.remove(&session_id);
        session_id
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_an_absent_session_reads_storage() {
        // Given a session persisted but not in memory.
        let fixture = actor_fixture().await;
        let session_id = persisted_but_absent(&fixture).await;

        // When it is activated.
        fixture
            .harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: Some(60),
            })
            .await;
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture
                .state
                .read()
                .session
                .get(&session_id)
                .is_some_and(|s| s.session_state() == SessionState::Loaded)
        })
        .await;

        // Then it was read from disk and is now in memory.
        assert!(loaded, "an absent session must be read from storage");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_a_session_already_in_memory_skips_storage() {
        // Given a session sitting in the live map, and a store that counts the
        // reads it is asked for.
        let store = Arc::new(ControlledStartupStore::new(&[]));
        let (harness, state, _picker, actor_path) = controlled_actor_fixture(store.clone()).await;
        let session_id = jinn_core_types::SessionId::new();
        {
            let mut guard = state.write();
            let mut session = ChatSessionState::new();
            session.set_session_id(session_id.clone());
            session.push_entry(jinn_core_types::ChatEntry::user("already here"));
            guard.session.insert(session);
            guard.session.begin_load(session_id.clone());
        }
        let before = store.load_calls.load(std::sync::atomic::Ordering::SeqCst);

        // When it is activated with no width on offer, the way Discord asks.
        harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: None,
            })
            .await;
        // The store actor must have processed the command before the state
        // is read back. A fixed sleep could let the read happen before
        // the actor wrote it, and the assertion would then prove nothing.
        jinn_testutil::wait::await_inbox_drained(
            harness.system(),
            &actor_path,
            Duration::from_secs(10),
        )
        .await;

        // Then the store was never asked for it.
        let after = store.load_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after, before,
            "a session already in memory must not be re-read from storage"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_a_session_in_memory_arms_the_measurement() {
        // Given a session in memory with the load guard armed, as a caller does.
        let fixture = actor_fixture().await;
        let session_id = persisted_but_absent(&fixture).await;
        {
            let mut state = fixture.state.write();
            state.session.begin_load(session_id.clone());
        }

        // When it is activated.
        fixture
            .harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: Some(60),
            })
            .await;
        // The store actor must have processed the command before the state
        // is read back. A fixed sleep could let the read happen before
        // the actor wrote it, and the assertion would then prove nothing.
        jinn_testutil::wait::await_inbox_drained(
            fixture.harness.system(),
            &fixture.actor_path,
            Duration::from_secs(10),
        )
        .await;

        // Then the guard is still held: the in-memory path measures, and this
        // fixture has no layout workers, so the measurement never completes.
        // That it got that far is the assertion — the guard was not cleared on
        // arrival, which is what an unmeasured activation looks like.
        assert!(
            fixture.state.read().session.is_loading(),
            "an in-memory activation must measure rather than clear the guard"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_a_session_that_vanished_clears_the_guard() {
        // Given a load guard armed for a session that is not in memory, as it
        // would be after an eviction raced the activation.
        let fixture = actor_fixture().await;
        let session_id = jinn_core_types::SessionId::new();
        {
            let mut state = fixture.state.write();
            state.session.begin_load(session_id.clone());
        }

        // When it is activated.
        fixture
            .harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: Some(60),
            })
            .await;
        // The store actor must have processed the command before the state
        // is read back. A fixed sleep could let the read happen before
        // the actor wrote it, and the assertion would then prove nothing.
        jinn_testutil::wait::await_inbox_drained(
            fixture.harness.system(),
            &fixture.actor_path,
            Duration::from_secs(10),
        )
        .await;

        // Then the guard is released rather than left spinning on a measurement
        // that will never run.
        assert!(
            !fixture.state.read().session.is_loading(),
            "an activation that found nothing must release the guard"
        );
    }
}

// ---------------------------------------------------------------------------
// Whole-tree load (ctrl+enter)
// ---------------------------------------------------------------------------

/// A whole-tree load restores a family, not a node.
///
/// The tree is resolved from the store's summaries, so these tests cross the
/// whole boundary — a real SQLite store seeded with a five-level tree — and
/// watch what lands in the session map. Each test asserts one behavior of the
/// load; the fixtures are duplicated rather than shared so a failure names
/// the behavior that broke.
mod tree_load_tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    use jinn_session_store_msg::LoadSessionTree;

    /// One persisted session with `parent` (or none), saved to `store`.
    ///
    /// An entry is pushed so the snapshot has real history, and the write is
    /// repeated so the store's revision floor is above the snapshot's fresh
    /// capture counter — the same shape the fork tests seed.
    async fn seed(
        fixture: &ActorFixture,
        session_id: &SessionId,
        parent: Option<&SessionId>,
        entry_text: &str,
    ) {
        let mut session = match parent {
            Some(parent_id) => ChatSessionState::new_child(parent_id, true),
            None => ChatSessionState::new(),
        };
        session.set_session_id(session_id.clone());
        session.mark_interacted();
        session.push_entry(jinn_core_types::ChatEntry::user(entry_text));
        for _ in 0..3 {
            fixture
                .store
                .save(&session.capture_snapshot())
                .await
                .expect("save session");
        }
    }

    /// All five tree members live in the map as `Loaded`.
    fn all_loaded(fixture: &ActorFixture, ids: &[&SessionId]) -> bool {
        let state = fixture.state.read();
        ids.iter().all(|id| {
            state
                .session
                .get(id)
                .is_some_and(|s| s.session_state() == SessionState::Loaded)
        })
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_five_level_tree_loads_completely_with_the_picked_node_active() {
        // Given a five-level tree — grandparent → parent → self → child →
        // grandchild — every member only in the store.
        let fixture = actor_fixture().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;

        // When the tree load names the middle node.
        let completed = fixture
            .harness
            .spawn_recorder::<SessionLoadCompleted>()
            .await;
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let completed = await_recorded(&completed, 5, Duration::from_secs(2)).await;

        // Then all five are live, and the completion events name each member.
        let ids = [
            &grandparent_id,
            &parent_id,
            &self_id,
            &child_id,
            &grandchild_id,
        ];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;
        assert!(loaded, "every tree member must land live and Loaded");
        // And the picked node is the one on screen.
        assert_eq!(
            fixture.state.read().session.active_session_id(),
            &self_id,
            "the picked node must be the active session"
        );
        // And a completion was published per member — five, not four plus a
        // double for the picked node.
        let mut completed_ids = completed
            .iter()
            .map(|msg| msg.session_id.clone())
            .collect::<Vec<_>>();
        completed_ids.sort();
        let mut expected = ids.iter().map(|id| (*id).clone()).collect::<Vec<_>>();
        expected.sort();
        assert_eq!(completed_ids, expected);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_sibling_subtree_of_the_picked_node_loads_too() {
        // Given the five-level tree plus a second child of the picked node's
        // parent, with its own child — a subtree hanging off a sibling.
        let fixture = actor_fixture().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        let sibling_id = SessionId::new();
        let nephew_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;
        seed(&fixture, &sibling_id, Some(&parent_id), "sibling").await;
        seed(&fixture, &nephew_id, Some(&sibling_id), "nephew").await;

        // When the tree load names the middle node.
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let ids = [
            &grandparent_id,
            &parent_id,
            &self_id,
            &child_id,
            &grandchild_id,
            &sibling_id,
            &nephew_id,
        ];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;

        // Then the whole connected tree is live, not just the picked spine.
        assert!(
            loaded,
            "the sibling's subtree is part of the tree and must load too"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_archived_member_lands_loaded_and_unarchived() {
        // Given the five-level tree with the grandparent archived on disk.
        let fixture = actor_fixture().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;
        fixture
            .store
            .set_archived(&grandparent_id, true)
            .await
            .expect("archive grandparent");

        // When the tree load names the middle node.
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let ids = [
            &grandparent_id,
            &parent_id,
            &self_id,
            &child_id,
            &grandchild_id,
        ];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;
        assert!(loaded, "the archived member must land live and Loaded");

        // Then the store no longer holds it as archived either — a tree load
        // unarchives every member it restores, exactly as a single load does.
        let summaries = fixture.store.load_summaries().await.expect("summaries");
        let grandparent = summaries
            .iter()
            .find(|summary| summary.session_id == grandparent_id)
            .expect("grandparent summary");
        assert_eq!(
            grandparent.session_state,
            SessionState::Loaded,
            "a restored tree member must be unarchived on disk"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_member_already_live_keeps_its_in_memory_history() {
        // Given a five-level tree whose parent is already live, carrying an
        // entry the store has not seen.
        let fixture = actor_fixture().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;
        {
            let mut state = fixture.state.write();
            let mut live = ChatSessionState::new();
            live.set_session_id(parent_id.clone());
            live.mark_interacted();
            live.push_entry(jinn_core_types::ChatEntry::user("only in memory"));
            state.session.insert(live);
        }

        // When the tree load names the middle node.
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let ids = [
            &grandparent_id,
            &parent_id,
            &self_id,
            &child_id,
            &grandchild_id,
        ];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;
        assert!(loaded, "the rest of the tree must still land");

        // Then the live member was not re-read: its unsaved entry is intact.
        let texts: Vec<String> = fixture
            .state
            .read()
            .session
            .get(&parent_id)
            .expect("live parent")
            .history()
            .iter()
            .map(jinn_core_types::ChatEntry::text)
            .collect();
        assert!(
            texts.contains(&"only in memory".to_owned()),
            "a live member's in-memory history must survive the tree load, got {texts:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_picked_member_is_measured_and_background_members_are_not() {
        // Given a five-level tree, with a layout-job recorder on the bus.
        let fixture = actor_fixture().await;
        let jobs = fixture.harness.spawn_recorder::<LayoutChatSession>().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;

        // When the tree load names the middle node.
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let jobs = await_recorded(&jobs, 1, Duration::from_secs(2)).await;
        let ids = [
            &grandparent_id,
            &parent_id,
            &self_id,
            &child_id,
            &grandchild_id,
        ];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;
        assert!(loaded, "the tree must land before judging the jobs");

        // Then exactly the picked member was handed to the layout workers:
        // background members measure lazily on first activation, and the
        // completion actor would discard eager measurements for them anyway.
        assert_eq!(jobs.len(), 1, "only the picked member may dispatch layout");
        assert_eq!(jobs[0].session_id, self_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_member_missing_from_the_store_is_skipped_without_failing_the_rest() {
        // Given a five-level tree whose grandparent was deleted from the
        // store after the parent's parent link was written.
        let fixture = actor_fixture().await;
        let grandparent_id = SessionId::new();
        let parent_id = SessionId::new();
        let self_id = SessionId::new();
        let child_id = SessionId::new();
        let grandchild_id = SessionId::new();
        seed(&fixture, &grandparent_id, None, "grandparent").await;
        seed(&fixture, &parent_id, Some(&grandparent_id), "parent").await;
        seed(&fixture, &self_id, Some(&parent_id), "self").await;
        seed(&fixture, &child_id, Some(&self_id), "child").await;
        seed(&fixture, &grandchild_id, Some(&child_id), "grandchild").await;
        fixture
            .store
            .delete(&grandparent_id)
            .await
            .expect("delete grandparent");

        // When the tree load names the middle node.
        fixture
            .harness
            .publish(LoadSessionTree {
                session_id: self_id.clone(),
            })
            .await;
        let ids = [&parent_id, &self_id, &child_id, &grandchild_id];
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            all_loaded(&fixture, &ids)
        })
        .await;

        // Then the surviving members all land despite the gap.
        assert!(
            loaded,
            "a member missing from the store must not fail the rest of the load"
        );
        assert!(
            !fixture.state.read().session.contains(&grandparent_id),
            "a member the store does not have cannot land live"
        );
    }
}

// ---------------------------------------------------------------------------
// Working time across the store boundary
// ---------------------------------------------------------------------------

/// Wall time crosses the store as stamped snapshots, and comes back as
/// restored intervals.
///
/// Every durable copy of a session's working intervals is stamped by the
/// store actor at the moment of capture — the snapshot itself never carries
/// them — and a load hands the recorded intervals back to the monitor, the
/// cell's single writer. These tests watch the whole boundary against a real
/// SQLite store: what lands in the blob on archive, what lands in the cell on
/// load, and whether the reactivation write keeps what the load restored.
mod working_time_tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    use jinn_core_types::WorkingInterval;
    use jinn_work_time_msg::RestoreWorkingTime;

    const T0: i64 = 1_000_000_000;

    fn at(offset_secs: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(T0 + offset_secs).expect("valid timestamp")
    }

    /// A live, interacted session in the map, plus the 90s interval the
    /// work-time cell records for it.
    fn live_session_with_recorded_time(fixture: &ActorFixture) -> SessionId {
        let session_id = SessionId::new();
        let mut session = ChatSessionState::new();
        session.set_session_id(session_id.clone());
        session.mark_interacted();
        session.push_entry(jinn_core_types::ChatEntry::user("worked"));
        fixture.state.write().session.insert(session.clone());
        // The monitor's own write path, folded the way a turn's events land:
        // a stretch of recorded work, closed. Written through the cell exactly
        // as the monitor leaves it.
        fixture.work_time.update(|working| {
            working.begin_working(session_id.clone(), at(0));
            working.end_working(&session_id, at(90));
        });
        session_id
    }

    /// The stored snapshot's intervals, if the session is in the store.
    async fn stored_intervals(
        fixture: &ActorFixture,
        session_id: &SessionId,
    ) -> Option<Vec<WorkingInterval>> {
        fixture
            .store
            .load_session(session_id)
            .await
            .ok()
            .flatten()
            .map(|snapshot| snapshot.metadata.working_intervals)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn archiving_a_session_stores_its_recorded_intervals() {
        // Given a live session whose 90s of work the work-time cell records.
        let fixture = actor_fixture().await;
        let session_id = live_session_with_recorded_time(&fixture);

        // When the session is archived.
        fixture
            .harness
            .publish(ArchiveSession {
                session_id: session_id.clone(),
            })
            .await;
        let removed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !fixture.state.read().session.contains(&session_id)
        })
        .await;
        assert!(removed, "the archive should complete");

        // Then the stored blob carries the cell's record, not the empty list
        // a bare capture holds — otherwise the next load displays 00:00:00.
        let stored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            stored_intervals(&fixture, &session_id)
                .await
                .is_some_and(|intervals| {
                    jinn_core_types::total(&intervals, at(1000))
                        == jiff::SignedDuration::from_secs(90)
                })
        })
        .await;
        assert!(
            stored,
            "an archive must write the session's recorded working time, got {:?}",
            stored_intervals(&fixture, &session_id).await
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persisting_a_session_stores_its_recorded_intervals() {
        // Given a live session whose 90s of work the work-time cell records.
        let fixture = actor_fixture().await;
        let session_id = live_session_with_recorded_time(&fixture);

        // When the rename path persists the session — the `PersistSession`
        // publish that lands on the store actor's own save.
        fixture
            .harness
            .publish(PersistSession {
                session_id: session_id.clone(),
            })
            .await;

        // Then the stored blob carries the cell's record, the same stamp an
        // archive writes — the two actors share one stamped-save flow, so no
        // trigger can store an un-stamped snapshot.
        let stored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            stored_intervals(&fixture, &session_id)
                .await
                .is_some_and(|intervals| {
                    jinn_core_types::total(&intervals, at(1000))
                        == jiff::SignedDuration::from_secs(90)
                })
        })
        .await;
        assert!(
            stored,
            "a rename-persist must write the session's recorded working time, got {:?}",
            stored_intervals(&fixture, &session_id).await
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn archiving_a_session_that_never_worked_stores_no_intervals() {
        // Given a live session the cell has no record for.
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut session = ChatSessionState::new();
        session.set_session_id(session_id.clone());
        session.mark_interacted();
        session.push_entry(jinn_core_types::ChatEntry::user("never worked"));
        fixture.state.write().session.insert(session);

        // When the session is archived.
        fixture
            .harness
            .publish(ArchiveSession {
                session_id: session_id.clone(),
            })
            .await;
        let removed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !fixture.state.read().session.contains(&session_id)
        })
        .await;
        assert!(removed, "the archive should complete");

        // Then the blob stores none — zero is recorded as zero, not invented.
        let stored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            stored_intervals(&fixture, &session_id).await.is_some()
        })
        .await;
        assert!(stored, "the archive should have written the session");
        let intervals = stored_intervals(&fixture, &session_id)
            .await
            .expect("stored session");
        assert!(
            intervals.is_empty(),
            "a session that never worked must store no intervals, got {intervals:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn loading_a_session_restores_its_recorded_time_into_the_cell() {
        // Given an archived session whose snapshot carries a closed 90s interval.
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut stored = ChatSessionState::new();
        stored.set_session_id(session_id.clone());
        stored.mark_interacted();
        stored.push_entry(jinn_core_types::ChatEntry::user("earlier work"));
        let mut snapshot = stored.capture_snapshot();
        snapshot.metadata.working_intervals = vec![WorkingInterval::closed(at(0), at(90))];
        snapshot.metadata.updated_at = at(90);
        fixture.store.save(&snapshot).await.expect("save session");
        fixture
            .store
            .set_archived(&session_id, true)
            .await
            .expect("archive session");

        // When the session is loaded back.
        fixture
            .harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: Some(60),
            })
            .await;
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture
                .state
                .read()
                .session
                .get(&session_id)
                .is_some_and(|s| s.session_state() == SessionState::Loaded)
        })
        .await;
        assert!(loaded, "the session should load");

        // Then the monitor holds the recorded time again — the status bar's
        // source — rather than reading 00:00:00 for a session that worked.
        let restored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture.work_time.read().working_time(&session_id, at(1000))
                == jiff::SignedDuration::from_secs(90)
        })
        .await;
        assert!(
            restored,
            "a load must repopulate the work-time cell with the recorded intervals"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_reactivation_save_keeps_the_intervals_a_load_restored() {
        // Given an archived session whose snapshot carries a closed 90s interval.
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut stored = ChatSessionState::new();
        stored.set_session_id(session_id.clone());
        stored.mark_interacted();
        stored.push_entry(jinn_core_types::ChatEntry::user("earlier work"));
        let mut snapshot = stored.capture_snapshot();
        snapshot.metadata.working_intervals = vec![WorkingInterval::closed(at(0), at(90))];
        snapshot.metadata.updated_at = at(90);
        fixture.store.save(&snapshot).await.expect("save session");
        fixture
            .store
            .set_archived(&session_id, true)
            .await
            .expect("archive session");

        // When the session is loaded back, whose reactivation write lands
        // while the restore publish is still in flight to the monitor.
        fixture
            .harness
            .publish(SessionLoadRequested {
                session_id: session_id.clone(),
                content_width: Some(60),
            })
            .await;
        let loaded = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture
                .state
                .read()
                .session
                .get(&session_id)
                .is_some_and(|s| s.session_state() == SessionState::Loaded)
        })
        .await;
        assert!(loaded, "the session should load");

        // Then the stored blob still totals 90s — the write that made the
        // session live must not erase the record the load just read.
        let kept = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            stored_intervals(&fixture, &session_id)
                .await
                .is_some_and(|intervals| {
                    jinn_core_types::total(&intervals, at(1000))
                        == jiff::SignedDuration::from_secs(90)
                })
        })
        .await;
        assert!(
            kept,
            "a reactivation save must not wipe the stored working time, got {:?}",
            stored_intervals(&fixture, &session_id).await
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_later_persist_of_a_live_loaded_session_stamps_the_cell_it_holds() {
        // Given a loaded session whose recorded time now sits in the cell, and
        // whose live core is in the map so a persist has something to write.
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut live = ChatSessionState::new();
        live.set_session_id(session_id.clone());
        live.mark_interacted();
        live.push_entry(jinn_core_types::ChatEntry::user("earlier work"));
        fixture.state.write().session.insert(live);
        fixture
            .harness
            .publish(RestoreWorkingTime {
                session_id: session_id.clone(),
                intervals: vec![WorkingInterval::closed(at(0), at(90))],
                last_active_at: at(90),
            })
            .await;
        let restored = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture.work_time.read().working_time(&session_id, at(1000))
                == jiff::SignedDuration::from_secs(90)
        })
        .await;
        assert!(restored, "the cell should hold the restored time first");

        // When the session is persisted again — a session-turn save, the way
        // an ordinary turn boundary writes it.
        fixture
            .harness
            .publish(PersistSession {
                session_id: session_id.clone(),
            })
            .await;

        // Then the ordinary cell read stamps what the cell holds — the
        // override is the load's crutch only, not the persist path's.
        let stamped = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            stored_intervals(&fixture, &session_id)
                .await
                .is_some_and(|intervals| {
                    jinn_core_types::total(&intervals, at(1000))
                        == jiff::SignedDuration::from_secs(90)
                })
        })
        .await;
        assert!(
            stamped,
            "a persist after the restore landed must stamp the cell's record, got {:?}",
            stored_intervals(&fixture, &session_id).await
        );
    }
}

/// A fork hands the user a new session and has to leave nothing spinning.
///
/// The chat log's loading indication is the load guard, and a fork is the one
/// load whose guard and its measurement do not name the same session: the guard
/// is armed for the source, and what gets measured is the child the store actor
/// creates. Nothing about that is visible from either side of the store, so
/// these tests cross the whole boundary — a live source in the map, a real
/// SQLite store behind it — and watch what the guard does.
mod fork_tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use error_stack::Report;
    use jinn_session_state::{SessionSnapshot, SessionStoreError};
    use jinn_session_store_msg::SessionForkRequested;
    use std::sync::Mutex;

    /// How long a fork may take before the user would call it hung. Far below
    /// the layout deadline, which is the bound the fix is really about: a fork
    /// that waited for that would leave the spinner up for half a minute.
    const FORK_BUDGET: Duration = Duration::from_secs(2);

    /// A live, active source session with `entry_count` entries in memory.
    ///
    /// Nothing is written to the store: the entries the user can see are the
    /// only copy that exists, which is exactly the case a fork that re-read
    /// storage would silently drop.
    fn live_source(fixture: &ActorFixture, entry_count: usize) -> SessionId {
        let source_id = SessionId::new();
        let mut state = fixture.state.write();
        let mut source = ChatSessionState::new();
        source.set_session_id(source_id.clone());
        source.set_model(jinn_core_types::ModelSelection::Single(
            "ollama/llama3".to_owned(),
        ));
        for index in 0..entry_count {
            source.push_entry(jinn_core_types::ChatEntry::user(format!("message {index}")));
        }
        state.session.remove(&source_id);
        state.session.insert(source);
        state.session.set_active(source_id.clone());
        // The guard the route action arms for the session being acted on.
        state.session.begin_load(source_id.clone());
        source_id
    }

    /// The texts the source's history is displaying, in order.
    fn source_texts(state: &AppState, id: &SessionId) -> Vec<String> {
        state
            .session
            .get(id)
            .expect("live source")
            .history()
            .iter()
            .map(jinn_core_types::ChatEntry::text)
            .collect()
    }

    /// A live source whose store is `store`, and whose route action has armed
    /// the load guard.
    fn live_source_in(state: &State, source_id: &SessionId, entry_count: usize) {
        let mut guard = state.write();
        let mut source = ChatSessionState::new();
        source.set_session_id(source_id.clone());
        for index in 0..entry_count {
            source.push_entry(jinn_core_types::ChatEntry::user(format!("message {index}")));
        }
        guard.session.remove(source_id);
        guard.session.insert(source);
        guard.session.set_active(source_id.clone());
        guard.session.begin_load(source_id.clone());
    }

    /// A store that records every write and every read it is asked for.
    ///
    /// A real fork writes the source, then the child, and reads neither back,
    /// so a record of the traffic is the whole of what these tests need to
    /// watch. A write refused on demand is the write failure.
    struct ForkStore {
        inner: ControlledStartupStore,
        /// Every session this store was asked to write, with the id it was
        /// asked to write it under.
        stored: Mutex<Vec<(SessionId, SessionSnapshot)>>,
    }

    impl ForkStore {
        fn new() -> Self {
            Self {
                inner: ControlledStartupStore::new(&[]),
                stored: Mutex::new(Vec::new()),
            }
        }

        /// The snapshots this store has been asked to write.
        fn written(&self) -> Vec<(SessionId, SessionSnapshot)> {
            self.stored.lock().expect("written snapshots").clone()
        }
    }

    #[async_trait::async_trait]
    impl SessionStore for ForkStore {
        fn name(&self) -> &'static str {
            "fork"
        }

        async fn save(&self, snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
            self.inner.save_calls.fetch_add(1, Ordering::SeqCst);
            if self.inner.failed_saves.load(Ordering::SeqCst) {
                return Err(Report::new(SessionStoreError));
            }
            self.stored
                .lock()
                .expect("written snapshots")
                .push((snapshot.session_id().clone(), snapshot.clone()));
            Ok(())
        }

        async fn load_session(
            &self,
            session_id: &SessionId,
        ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
            self.inner.load_calls.fetch_add(1, Ordering::SeqCst);
            self.inner
                .requested_session_ids
                .lock()
                .expect("requested session IDs")
                .push(session_id.clone());
            Ok(self
                .stored
                .lock()
                .expect("written snapshots")
                .iter()
                .find(|(id, _)| id == session_id)
                .map(|(_, snapshot)| snapshot.clone()))
        }

        async fn load_summaries(
            &self,
        ) -> Result<Vec<jinn_session_store_msg::SessionSummary>, Report<SessionStoreError>>
        {
            Ok(Vec::new())
        }

        async fn delete(&self, _session_id: &SessionId) -> Result<(), Report<SessionStoreError>> {
            Ok(())
        }

        async fn fork(
            &self,
            _source_session_id: &SessionId,
            _at_ordinal: usize,
        ) -> Result<SessionId, Report<SessionStoreError>> {
            Ok(SessionId::new())
        }

        async fn set_archived(
            &self,
            _session_id: &SessionId,
            _archived: bool,
        ) -> Result<(), Report<SessionStoreError>> {
            Ok(())
        }

        async fn set_archived_many(
            &self,
            _session_ids: &[SessionId],
            _archived: bool,
        ) -> Result<(), Report<SessionStoreError>> {
            Ok(())
        }

        async fn set_in_library(
            &self,
            _session_id: &SessionId,
            _in_library: bool,
        ) -> Result<(), Report<SessionStoreError>> {
            Ok(())
        }

        async fn load_unarchived_summaries(
            &self,
        ) -> Result<Vec<jinn_session_store_msg::SessionSummary>, Report<SessionStoreError>>
        {
            Ok(Vec::new())
        }

        async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
            Ok(Vec::new())
        }

        async fn reindex_session_chunk(
            &self,
            _session_id: &SessionId,
            _max_entries: usize,
        ) -> Result<bool, Report<SessionStoreError>> {
            Ok(true)
        }

        async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>> {
            Ok(0)
        }

        async fn search(
            &self,
            _params: jinn_session_store_msg::SearchParams,
        ) -> Result<jinn_session_store_msg::SearchOutcome, Report<SessionStoreError>> {
            Ok(jinn_session_store_msg::SearchOutcome {
                total_matches: 0,
                per_session: Vec::new(),
                hits: Vec::new(),
            })
        }

        async fn fetch_window(
            &self,
            _session_id: &SessionId,
            _anchor: &jinn_kernel::protocol::ChatEntryId,
            _context: usize,
        ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>>
        {
            Ok(None)
        }

        async fn fetch_tail(
            &self,
            _session_id: &SessionId,
            _limit: usize,
        ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>>
        {
            Ok(None)
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_leaves_the_chat_log_loading_indication_behind() {
        // Given a live source session the user is looking at, and the guard
        // its route action armed.
        let fixture = actor_fixture().await;
        let source_id = live_source(&fixture, 3);

        // When they fork from the last entry.
        fixture
            .harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 2,
            })
            .await;

        // Then the child is on screen and nothing is loading any more.
        let arrived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture.state.read().session.active_session_id() != &source_id
        })
        .await;
        assert!(arrived, "the fork should switch to the child it created");
        let released = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !fixture.state.read().session.is_loading()
        })
        .await;
        assert!(
            released,
            "a completed fork must not leave the chat log's loading indication up"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_settles_within_its_own_budget_not_the_layout_deadline() {
        // Given a source session large enough that its measurement is worth
        // handing off, and the full layout subsystem installed.
        let fixture = actor_fixture().await;
        jinn_chat_log_view::kernel_element::install_layout_actors(
            fixture.harness.system(),
            fixture.state.clone(),
        );
        let source_id = live_source(&fixture, 40);

        // When they fork from the last entry.
        let forked_at = std::time::Instant::now();
        fixture
            .harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 39,
            })
            .await;
        let released = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !fixture.state.read().session.is_loading()
        })
        .await;

        // Then the indication is down, and it came down in seconds rather than
        // at the thirty-second deadline that used to be the only backstop.
        assert!(released, "the fork must not wait on a layout worker");
        assert!(
            forked_at.elapsed() < FORK_BUDGET,
            "a fork must settle in {FORK_BUDGET:?}, not at the layout deadline"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_does_not_read_the_source_back_from_the_store() {
        // Given a live source session, and a store that records every read it
        // is asked for.
        let store = Arc::new(ForkStore::new());
        let (harness, state, _picker, _actor_path) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        live_source_in(&state, &source_id, 3);

        // When they fork from the last entry.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 2,
            })
            .await;
        let done = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            state.read().session.active_session_id() != &source_id
        })
        .await;
        assert!(done, "the fork should complete");

        // Then the source was never read from storage — the history it is
        // showing is already in the process.
        assert!(
            !store
                .inner
                .requested_session_ids
                .lock()
                .expect("requested session IDs")
                .contains(&source_id),
            "a fork must not pay a disk read of a session it already has in memory"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_carries_the_source_s_unsaved_entries_into_the_child() {
        // Given a source with four entries on screen and nothing written.
        let fixture = actor_fixture().await;
        let source_id = live_source(&fixture, 4);

        // When they fork from the third.
        fixture
            .harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 2,
            })
            .await;
        let arrived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture.state.read().session.active_session_id() != &source_id
        })
        .await;
        assert!(arrived, "the fork should switch to the child it created");

        // Then the child holds the source's entries through the fork point.
        let state = fixture.state.read();
        let child_id = state.session.active_session_id().clone();
        let child = state.session.get(&child_id).expect("live child");
        let texts = child
            .history()
            .iter()
            .map(jinn_core_types::ChatEntry::text)
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            vec!["message 0", "message 1", "message 2"],
            "an entry the user can see but has not saved is still history the child owes"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_writes_the_child_it_persists_to_the_store() {
        // Given a live source with three entries, and a store that records
        // every write it is asked for.
        let store = Arc::new(ForkStore::new());
        let (harness, state, _picker, _actor_path) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        live_source_in(&state, &source_id, 3);

        // When they fork from the second entry.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 1,
            })
            .await;
        let written = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !store.written().is_empty()
        })
        .await;
        assert!(written, "the fork should write the child it created");

        // Then what landed is the fork of the source it was taken from. The
        // fork also writes the source itself, first, so the child is the one
        // write that names this session as a parent.
        let child = store
            .written()
            .into_iter()
            .find(|(_, snapshot)| snapshot.parent_session() == &Some(source_id.clone()))
            .map(|(_, snapshot)| snapshot)
            .expect("the child write");
        assert_eq!(child.fork_ordinal(), Some(1));
        assert_eq!(child.entries.len(), 2);
        assert_eq!(child.entries[1].text(), "message 1");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_leaves_the_source_s_own_history_untouched() {
        // Given a live source with three entries on screen.
        let fixture = actor_fixture().await;
        let source_id = live_source(&fixture, 3);
        let before = source_texts(&fixture.state.read(), &source_id);

        // When they fork from the second entry.
        fixture
            .harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 1,
            })
            .await;
        let arrived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            fixture.state.read().session.active_session_id() != &source_id
        })
        .await;
        assert!(arrived, "the fork should switch to the child it created");

        // Then the source still shows what it showed.
        assert_eq!(source_texts(&fixture.state.read(), &source_id), before);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_of_a_source_that_is_not_in_memory_leaves_the_loading_indication_down() {
        // Given a load guard armed for a session that has since left the map.
        let store = Arc::new(ForkStore::new());
        let (harness, state, _picker, _actor_path) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        {
            let mut guard = state.write();
            guard.session.begin_load(source_id.clone());
        }

        // When they fork it.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 0,
            })
            .await;
        let released = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !state.read().session.is_loading()
        })
        .await;

        // Then nothing is left spinning.
        assert!(
            released,
            "a fork with nothing to fork must not strand the loading indication"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_fork_the_store_will_not_write_leaves_the_loading_indication_down() {
        // Given a source session, and a store refusing every write.
        let store = Arc::new(ForkStore::new());
        store.inner.fail_saves();
        let (harness, state, _picker, _actor_path) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        live_source_in(&state, &source_id, 1);

        // When they fork.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 0,
            })
            .await;
        let attempted =
            jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
                store.inner.save_calls.load(Ordering::SeqCst) > 1
            })
            .await;
        assert!(attempted, "the fork should have tried to write the child");
        let released = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
            !state.read().session.is_loading()
        })
        .await;

        // Then nothing is left spinning, and the source is still on screen.
        assert!(
            released,
            "a fork that could not be written must not strand the loading indication"
        );
        assert_eq!(state.read().session.active_session_id(), &source_id);
    }
}

#[rstest::rstest]
#[tokio::test]
async fn set_library_writes_in_memory_flag_when_loaded() {
    // Given an actor whose session is live in the session map and saved
    // to the store.
    let (fixture, session_id) = {
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut live = ChatSessionState::new();
        live.set_session_id(session_id.clone());
        live.set_title("to shelve".to_owned());
        live.mark_interacted();
        fixture.state.with_session(|view| {
            view.session.map().insert(live.clone());
        });
        fixture
            .store
            .save(&live.capture_snapshot())
            .await
            .expect("save session");
        (fixture, session_id)
    };

    // When the membership command lands.
    fixture
        .harness
        .publish(SetSessionLibrary {
            session_id: session_id.clone(),
            in_library: true,
        })
        .await;
    let flagged = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(jinn_session_state::ChatSessionState::in_library)
    })
    .await;

    // Then the live session's flag is set...
    assert!(
        flagged,
        "the store actor must set the in-memory flag on a loaded session"
    );
    // ...and the flag is durable.
    let stored = fixture
        .store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");
    assert!(
        stored.metadata.in_library,
        "the column write must land, not just the memory write"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn clearing_library_clears_the_in_memory_flag() {
    // Given an actor whose session is live in the session map and saved
    // as a library member.
    let (fixture, session_id) = {
        let fixture = actor_fixture().await;
        let session_id = SessionId::new();
        let mut live = ChatSessionState::new();
        live.set_session_id(session_id.clone());
        live.set_title("shelved".to_owned());
        live.mark_interacted();
        live.set_in_library(true);
        fixture.state.with_session(|view| {
            view.session.map().insert(live.clone());
        });
        fixture
            .store
            .save(&live.capture_snapshot())
            .await
            .expect("save session");
        (fixture, session_id)
    };

    // When the membership-clear command lands.
    fixture
        .harness
        .publish(SetSessionLibrary {
            session_id: session_id.clone(),
            in_library: false,
        })
        .await;
    let cleared = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|session| !session.in_library())
    })
    .await;

    // Then the flag clears in memory too.
    assert!(cleared, "clearing membership must reach the live session");
}

#[rstest::rstest]
#[tokio::test]
async fn library_cell_lists_members_from_summaries() {
    // Given an actor with one saved, archived member and one plain session —
    // membership is orthogonal to archived state, so the archived one lists.
    let fixture = {
        let fixture = actor_fixture().await;
        for (title, member, archive) in [
            ("shelved member", true, true),
            ("plain session", false, false),
        ] {
            let session_id = SessionId::new();
            let mut live = ChatSessionState::new();
            live.set_session_id(session_id.clone());
            live.set_title(title.to_owned());
            live.mark_interacted();
            if member {
                live.set_in_library(true);
            }
            if archive {
                live.set_session_state(SessionState::Archived);
            }
            fixture
                .store
                .save(&live.capture_snapshot())
                .await
                .expect("save session");
        }
        fixture
    };

    // When the library load lands.
    fixture.harness.publish(LoadLibraryEntries).await;
    let listed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.library_entries().is_empty()
    })
    .await;

    // Then exactly the member lists, archived or not.
    assert!(listed, "the library cell must fill from the summaries");
    let entries = fixture.library_entries();
    assert_eq!(entries.len(), 1, "only members list, got: {entries:?}");
    assert_eq!(entries[0].title, "shelved member");
    assert!(
        entries[0].in_library,
        "the summary must carry the membership flag"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn library_cell_fills_at_startup() {
    // Given an actor with one saved library member and one plain session,
    // and no `LoadLibraryEntries` published — the section has never been
    // entered.
    let fixture = {
        let fixture = actor_fixture().await;
        for (title, member) in [("shelved member", true), ("plain session", false)] {
            let session_id = SessionId::new();
            let mut live = ChatSessionState::new();
            live.set_session_id(session_id.clone());
            live.set_title(title.to_owned());
            live.mark_interacted();
            if member {
                live.set_in_library(true);
            }
            fixture
                .store
                .save(&live.capture_snapshot())
                .await
                .expect("save session");
        }
        fixture
    };

    // When the environment-loaded event fires — the store actor's startup
    // path.
    fixture
        .harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let listed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.library_entries().is_empty()
    })
    .await;

    // Then the cell holds the member without anyone entering the section.
    assert!(
        listed,
        "startup must fill the library cell so members are visible without \
         visiting the section"
    );
    let entries = fixture.library_entries();
    assert_eq!(entries.len(), 1, "only members list, got: {entries:?}");
    assert_eq!(entries[0].title, "shelved member");
}

// ── The Sessions list is never empty ─────────────────────────────────────
//
// A store-actor mutation that would leave no listed session — a library move
// of the last listed one, a close/archive of it while only members remain —
// first inserts a fresh seeded session, so the sidebar's Sessions list never
// renders its empty placeholder through normal operation.

/// Seeds the persisted preferences the seeded builder reads, and returns the
/// model selection those preferences carry.
fn seed_persisted_preferences(fixture: &ActorFixture) -> jinn_core_types::ModelSelection {
    let model = jinn_core_types::ModelSelection::Single("ollama/llama3".to_owned());
    let mut app_state = fixture.app_state_storage.read();
    app_state.last_model = Some(model.clone());
    app_state.reasoning_effort = Some(jinn_core_types::ReasoningEffort::High);
    fixture
        .app_state_storage
        .save(&app_state)
        .expect("persist preferences");
    model
}

/// Inserts one live listed session, saves it to the store, and returns its id.
async fn insert_listed_session(fixture: &ActorFixture, title: &str) -> SessionId {
    let session_id = SessionId::new();
    let mut live = ChatSessionState::new();
    live.set_session_id(session_id.clone());
    live.set_title(title.to_owned());
    live.mark_interacted();
    live.push_entry(jinn_core_types::ChatEntry::user("work"));
    fixture.state.with_session(|view| {
        view.session.map().insert(live.clone());
    });
    fixture
        .store
        .save(&live.capture_snapshot())
        .await
        .expect("save session");
    session_id
}

/// Marks every map resident except `keep` a library member.
fn flag_all_but_one_as_members(fixture: &ActorFixture, keep: &SessionId) {
    fixture.state.with_session(|view| {
        let members: Vec<SessionId> = view
            .session
            .map()
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| id != keep)
            .collect();
        for id in members {
            view.session
                .map()
                .get_mut(&id)
                .expect("resident still in the map")
                .set_in_library(true);
        }
    });
}

/// Counts the sessions the sidebar's Sessions list would show.
fn listed_count(fixture: &ActorFixture) -> usize {
    fixture
        .state
        .read()
        .session
        .iter()
        .filter(|(_, session)| session.is_listed())
        .count()
}

#[rstest::rstest]
#[tokio::test]
async fn library_move_of_the_last_listed_session_inserts_a_fresh_one() {
    // Given a fixture whose only listed session is about to move into the
    // library, with persisted preferences for the seeded builder to carry.
    let fixture = actor_fixture().await;
    let model = seed_persisted_preferences(&fixture);
    let session_id = insert_listed_session(&fixture, "the only listed one").await;
    flag_all_but_one_as_members(&fixture, &session_id);

    // When the membership command moves it into the library.
    fixture
        .harness
        .publish(SetSessionLibrary {
            session_id: session_id.clone(),
            in_library: true,
        })
        .await;
    let moved = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(jinn_session_state::ChatSessionState::in_library)
    })
    .await;
    assert!(moved, "the move must land before the repair is checked");

    // Then a fresh session keeps the list non-empty, carrying the persisted
    // model selection and reasoning effort.
    let state = fixture.state.read();
    assert!(
        state.session.iter().any(|(_, session)| session.is_listed()),
        "the Sessions list must never be empty after a library move"
    );
    let fresh = state
        .session
        .iter()
        .map(|(_, session)| session)
        .find(|session| session.is_listed())
        .expect("a listed session exists");
    assert_eq!(fresh.profile().model, model);
    assert_eq!(
        fresh.profile().reasoning_effort,
        Some(jinn_core_types::ReasoningEffort::High)
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_library_move_that_leaves_others_listed_inserts_nothing() {
    // Given two listed sessions beyond the fixture's opening resident, and
    // the opening resident flagged into the library so the two are the only
    // listed ones.
    let fixture = actor_fixture().await;
    let first = insert_listed_session(&fixture, "first").await;
    let second = insert_listed_session(&fixture, "second").await;
    let opening = {
        let state = fixture.state.read();
        state.session.active_session_id().clone()
    };
    flag_all_but_two_as_members(&fixture, &first, &second);

    // When only the first moves into the library.
    fixture
        .harness
        .publish(SetSessionLibrary {
            session_id: first.clone(),
            in_library: true,
        })
        .await;
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the list still shows the second and the map gained no session.
    let state = fixture.state.read();
    assert_eq!(listed_count(&fixture), 1);
    assert!(state.session.contains(&second));
    assert_eq!(state.session.session_count(), 3);
    assert!(state.session.contains(&opening), "no session was inserted");
}

/// Marks every map resident a library member except `first` and `second`.
fn flag_all_but_two_as_members(fixture: &ActorFixture, first: &SessionId, second: &SessionId) {
    fixture.state.with_session(|view| {
        let members: Vec<SessionId> = view
            .session
            .map()
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| id != first && id != second)
            .collect();
        for id in members {
            view.session
                .map()
                .get_mut(&id)
                .expect("resident still in the map")
                .set_in_library(true);
        }
    });
}

#[rstest::rstest]
#[tokio::test]
async fn a_library_move_repair_keeps_the_active_session_on_the_moved_member() {
    // Given a fixture whose only listed session is active and about to move.
    let fixture = actor_fixture().await;
    let session_id = insert_listed_session(&fixture, "being read").await;
    fixture.state.with_session(|view| {
        view.session.map().set_active(session_id.clone());
    });
    flag_all_but_one_as_members(&fixture, &session_id);

    // When the move lands.
    fixture
        .harness
        .publish(SetSessionLibrary {
            session_id: session_id.clone(),
            in_library: true,
        })
        .await;
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the active session is still the moved member: moving it to the
    // Library is not a navigation act.
    assert_eq!(
        fixture.state.read().session.active_session_id(),
        &session_id
    );
}

#[rstest::rstest]
#[tokio::test]
async fn archive_of_the_last_listed_session_inserts_a_fresh_active_one() {
    // Given a fixture whose only listed session is the active one, and a
    // recorder for the removal announcement.
    let fixture = actor_fixture().await;
    let removed = fixture
        .harness
        .spawn_recorder::<jinn_session_msg::SessionRemoved>()
        .await;
    let model = seed_persisted_preferences(&fixture);
    let session_id = insert_listed_session(&fixture, "last visible").await;
    fixture.state.with_session(|view| {
        view.session.map().set_active(session_id.clone());
    });
    flag_all_but_one_as_members(&fixture, &session_id);

    // When the archive command lands.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let announcements = await_recorded(&removed, 1, Duration::from_secs(2)).await;
    assert_eq!(announcements.len(), 1);

    // Then a fresh session is listed, is the active session, and carries the
    // persisted model selection.
    let state = fixture.state.read();
    assert!(
        !state.session.contains(&session_id),
        "the archived session must be gone from the map"
    );
    let fresh = state
        .session
        .iter()
        .map(|(_, session)| session)
        .find(|session| session.is_listed())
        .expect("the repair must leave a listed session");
    assert_eq!(state.session.active_session_id(), fresh.session_id());
    assert_eq!(fresh.profile().model, model);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_that_leaves_others_listed_inserts_nothing_and_keeps_active() {
    // Given three listed sessions beyond the fixture's opening resident —
    // the opening resident is flagged into the library, so the map has
    // listed residents for its own retarget to pick and the repair must not
    // fire.
    let fixture = actor_fixture().await;
    let active = insert_listed_session(&fixture, "active").await;
    let _other_a = insert_listed_session(&fixture, "other a").await;
    let _other_b = insert_listed_session(&fixture, "other b").await;
    let opening = {
        let state = fixture.state.read();
        state.session.active_session_id().clone()
    };
    {
        fixture.state.with_session(|view| {
            view.session
                .map()
                .get_mut(&opening)
                .expect("opening resident")
                .set_in_library(true);
        });
    }
    fixture.state.with_session(|view| {
        view.session.map().set_active(active.clone());
    });

    // When the active session is archived.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: active.clone(),
        })
        .await;
    let removed = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.contains(&active)
    })
    .await;
    assert!(removed, "the archived session must leave the map");

    // Then nothing was inserted: the two remaining listed sessions cover
    // the list. (Where the map's own retarget lands the active id among its
    // residents is the map's documented nondeterministic choice, not this
    // repair's subject.)
    let state = fixture.state.read();
    assert_eq!(state.session.session_count(), 3);
    assert_eq!(listed_count(&fixture), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn a_tree_archive_of_members_and_listed_sessions_never_publishes_into_an_empty_list() {
    // Given a parent whose child is a library member: archiving the tree
    // removes the parent first, then the child — and between the two
    // removals only the repair insert can keep the list non-empty.
    let fixture = actor_fixture().await;
    let parent_id = insert_listed_session(&fixture, "tree parent").await;
    let child_id = SessionId::new();
    let mut child = ChatSessionState::new_child(&parent_id, true);
    child.set_session_id(child_id.clone());
    child.set_title("member child".to_owned());
    child.mark_interacted();
    child.set_in_library(true);
    fixture.state.with_session(|view| {
        view.session.map().insert(child);
    });
    // The map's opening resident is also flagged, so the parent is the last
    // listed session the tree archive removes.
    flag_all_but_one_as_members(&fixture, &parent_id);
    fixture.state.with_session(|view| {
        view.session.map().set_active(parent_id.clone());
    });

    // When the tree archive lands, each member is announced separately.
    let removed = fixture
        .harness
        .spawn_recorder::<jinn_session_msg::SessionRemoved>()
        .await;
    fixture
        .harness
        .publish(ArchiveSessionTree {
            root: parent_id.clone(),
        })
        .await;
    let announcements = await_recorded(&removed, 2, Duration::from_secs(2)).await;
    assert_eq!(announcements.len(), 2, "each member is announced once");

    // Then the fresh session exists by the time the last announcement is
    // processed, and it is listed and active.
    let state = fixture.state.read();
    assert!(
        !state.session.contains(&parent_id) && !state.session.contains(&child_id),
        "both tree members must be gone"
    );
    let fresh = state
        .session
        .iter()
        .map(|(_, session)| session)
        .find(|session| session.is_listed())
        .expect("a listed session survives the tree archive");
    assert_eq!(state.session.active_session_id(), fresh.session_id());
}

#[rstest::rstest]
#[tokio::test]
async fn a_repair_insert_publishes_no_session_created_and_writes_no_persistence() {
    // Given a fixture whose archive will trigger the repair, with a recorder
    // for the creation announcement.
    let fixture = actor_fixture().await;
    let created = fixture
        .harness
        .spawn_recorder::<jinn_session_lifecycle_msg::SessionCreated>()
        .await;
    let session_id = insert_listed_session(&fixture, "triggers repair").await;
    flag_all_but_one_as_members(&fixture, &session_id);

    // When the archive that empties the visible list lands.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let archived = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !fixture.state.read().session.contains(&session_id)
    })
    .await;
    assert!(archived, "the archive must land");

    // Then the repair inserted in memory only: nothing announced the new
    // session as created, and no snapshot for it exists in the store.
    let created_messages = await_recorded(&created, 0, Duration::from_millis(200)).await;
    assert!(
        created_messages.is_empty(),
        "a repair insert must not publish SessionCreated"
    );
    let fresh_id = {
        let state = fixture.state.read();
        state
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.is_listed()))
            .find(|(_, listed)| *listed)
            .map(|(id, _)| id)
    };
    let fresh_id = fresh_id.expect("a listed session exists");
    let stored = fixture
        .store
        .load_session(&fresh_id)
        .await
        .expect("store read");
    assert!(
        stored.is_none(),
        "a repair-inserted session must not be persisted"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_repair_publishes_enablement_only_when_the_seed_auto_enables_servers() {
    // Given a fixture with an auto-enabled MCP server configured, whose
    // archive will trigger the repair.
    let fixture = actor_fixture().await;
    let enablement = fixture
        .harness
        .spawn_recorder::<jinn_mcp_msg::McpEnablementChanged>()
        .await;
    let servers: std::collections::BTreeMap<_, _> = [(
        "alpha".to_owned(),
        jinn_preferences_config::schemas::mcp::McpServerConfig {
            auto_enable: true,
            ..Default::default()
        },
    )]
    .into();
    fixture
        .services_config()
        .put::<jinn_preferences_config::schemas::mcp::McpServersConfig>(
            &jinn_preferences_config::schemas::mcp::McpServersConfig(servers),
        )
        .expect("write mcp config");
    let session_id = insert_listed_session(&fixture, "triggers seed").await;
    flag_all_but_one_as_members(&fixture, &session_id);

    // When the archive that empties the visible list lands.
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let announcements = await_recorded(&enablement, 1, Duration::from_secs(2)).await;

    // Then an enablement event names the repair-inserted session: the fresh
    // seed is live, listed, and carries the auto-enabled server. (The map's
    // own replacement path announces its built session separately; that
    // pre-existing publish is not this test's subject.)
    let listed_ids: std::collections::BTreeSet<SessionId> = {
        let state = fixture.state.read();
        state
            .session
            .iter()
            .filter(|(_, session)| session.is_listed())
            .map(|(id, _)| id.clone())
            .collect()
    };
    assert!(
        announcements
            .iter()
            .any(|event| listed_ids.contains(&event.session_id)),
        "the repair's enablement must name a live listed session, got {announcements:?}"
    );
    let repaired = announcements
        .iter()
        .find(|event| listed_ids.contains(&event.session_id))
        .expect("one event names the listed session");
    let enabled: std::collections::BTreeSet<_> = ["alpha".to_owned()].into();
    assert_eq!(repaired.enabled, enabled);
}
