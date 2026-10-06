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
    ArchiveSession, ArchiveSessionTree, LoadSessionPickerEntries, PersistSession,
    SessionLoadCompleted, SessionLoadRequested, SessionState,
};
use jinn_testutil::bus_harness::{Recorder, TestHarness, await_recorded};

use crate::session_store_actor::{SessionStoreActor, SessionStoreActorDeps};
use crate::session_store_tests_support::{ControlledStartupStore, poll_until};
use crate::sqlite::SqliteSessionStore;

struct ActorFixture {
    _dir: tempfile::TempDir,
    harness: TestHarness,
    state: State,
    store: Arc<SqliteSessionStore>,
    /// The status bar's cell, taken from the *same* `Slices` the actor holds.
    ///
    /// `HarnessServices::services` mints a fresh `Services` — and so a fresh
    /// registry — on every call, so a test cannot reach the actor's cells
    /// through the harness. The cell is registered on the actor's own
    /// `Services` before the spawn and carried out here for reading.
    status_bar: jinn_slices::cell::TypedCell<jinn_status_bar_msg::StatusBarState>,
}

async fn actor_fixture() -> ActorFixture {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Arc::new(
        SqliteSessionStore::new_in(dir.path())
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
    let state = State::new(AppState::default());
    let _actor = SessionStoreActor::spawn(
        harness.system(),
        SessionStoreActorDeps {
            services,
            state: state.clone(),
            session_picker_cell: session_picker_cell.clone(),
        },
    );
    ActorFixture {
        _dir: dir,
        harness,
        state,
        store,
        status_bar,
    }
}

type ControlledFixture = (
    TestHarness,
    State,
    jinn_slices::cell::TypedCell<jinn_session_store_msg::SessionPickerState>,
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
    let state = State::new(AppState::default());
    let _actor = SessionStoreActor::spawn(
        harness.system(),
        SessionStoreActorDeps {
            services,
            state: state.clone(),
            session_picker_cell: session_picker_cell.clone(),
        },
    );
    (harness, state, session_picker_cell)
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
    let (harness, state, picker_cell) = controlled_actor_fixture(store.clone()).await;

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
    let picker_served = poll_until(|| async { !picker_cell.read().tree.items().is_empty() }).await;

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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When the ungated session's read completes while the other is still out.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let loaded = poll_until(|| async { state.read().session.contains(&loaded_id) }).await;

    // Then hydration is still active: one completion is not all of them.
    assert!(loaded, "the ungated session should have loaded");
    assert!(
        state.read().session.is_startup_hydrating(),
        "hydration must stay active while a read is still outstanding"
    );

    // And when the last read is released, the flag clears.
    store.release_session_load(&gated_id);
    let cleared = poll_until(|| async { !state.read().session.is_startup_hydrating() }).await;
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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store).await;

    // When startup hydration runs to completion.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    let member_frozen =
        poll_until(|| async { state.read().session.frozen_nodes().contains_key(&member_id) }).await;

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
    let (harness, _state, _picker_cell) = controlled_actor_fixture(store.clone()).await;
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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When the newer snapshot is released but the older snapshot remains gated.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    store.wait_for_session_load(&newer_id).await;
    store.release_session_load(&newer_id);
    let newer_visible = poll_until(|| async { state.read().session.contains(&newer_id) }).await;
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
    let (harness, _state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async { store.load_calls.load(Ordering::SeqCst) == 2 }).await;

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
    let (harness, _state, _picker_cell) = controlled_actor_fixture(store.clone()).await;
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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When the unarchived session is inserted and tree hydration begins.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async { state.read().session.contains(&session_id) }).await;
    store.wait_for_tree_summary_load().await;

    // Then the shared hydration projection is already inactive.
    assert!(!state.read().session.is_startup_hydrating());
}

#[rstest::rstest]
#[tokio::test]
async fn empty_startup_clears_hydration() {
    // Given a store with no unarchived sessions.
    let store = Arc::new(ControlledStartupStore::new(&[]));
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async {
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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration is attempted.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async {
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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store).await;

    // When startup hydration continues past the failed snapshot.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async { state.read().session.contains(&loaded_id) }).await;

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
    let (harness, state, _picker_cell) = controlled_actor_fixture(store).await;
    let welcome_id = state.read().session.active_session_id().clone();

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async { state.read().session.contains(&persisted_id) }).await;

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
    let (harness, _state, _picker_cell) = controlled_actor_fixture(store.clone()).await;

    // When startup hydration completes.
    harness
        .publish(EnvironmentLoaded {
            config: empty_providers_config(),
        })
        .await;
    poll_until(|| async { store.unarchived_summary_calls.load(Ordering::SeqCst) == 1 }).await;
    poll_until(|| async { store.all_summary_calls.load(Ordering::SeqCst) == 1 }).await;

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
    tokio::time::sleep(Duration::from_millis(200)).await;

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
    let restored = poll_until(|| async {
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
    let listed = poll_until(|| async {
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
    fixture
        .harness
        .publish(ArchiveSession {
            session_id: session_id.clone(),
        })
        .await;
    let archived =
        poll_until(|| async { !fixture.state.read().session.contains(&session_id) }).await;
    assert!(archived, "the first archive should remove the session");
    fixture
        .harness
        .publish(SessionLoadRequested {
            session_id: session_id.clone(),
            content_width: Some(60),
        })
        .await;
    let reloaded = poll_until(|| async {
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
    let removed =
        poll_until(|| async { !fixture.state.read().session.contains(&session_id) }).await;
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
    let persisted = poll_until(|| async {
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
    let archived = poll_until(|| async {
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
    let hinted = poll_until(|| async { fixture.status_bar.read().hint.is_some() }).await;

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
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
    let saved = poll_until(|| async {
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
    let removed =
        poll_until(|| async { !fixture.state.read().session.contains(&session_id) }).await;
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
    tokio::time::sleep(Duration::from_millis(150)).await;

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
    tokio::time::sleep(Duration::from_millis(200)).await;

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
    let released = poll_until(|| async { !fixture.state.read().session.is_loading() }).await;
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
        let loaded = poll_until(|| async {
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
        let (harness, state, _picker) = controlled_actor_fixture(store.clone()).await;
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
        tokio::time::sleep(Duration::from_millis(200)).await;

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
        tokio::time::sleep(Duration::from_millis(200)).await;

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
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Then the guard is released rather than left spinning on a measurement
        // that will never run.
        assert!(
            !fixture.state.read().session.is_loading(),
            "an activation that found nothing must release the guard"
        );
    }
}

// ---------------------------------------------------------------------------
// Forking from a chat log entry
// ---------------------------------------------------------------------------

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
        let arrived =
            poll_until(|| async { fixture.state.read().session.active_session_id() != &source_id })
                .await;
        assert!(arrived, "the fork should switch to the child it created");
        let released = poll_until(|| async { !fixture.state.read().session.is_loading() }).await;
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
        let released = poll_until(|| async { !fixture.state.read().session.is_loading() }).await;

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
        let (harness, state, _picker) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        live_source_in(&state, &source_id, 3);

        // When they fork from the last entry.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 2,
            })
            .await;
        let done =
            poll_until(|| async { state.read().session.active_session_id() != &source_id }).await;
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
        let arrived =
            poll_until(|| async { fixture.state.read().session.active_session_id() != &source_id })
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
        let (harness, state, _picker) = controlled_actor_fixture(store.clone()).await;
        let source_id = SessionId::new();
        live_source_in(&state, &source_id, 3);

        // When they fork from the second entry.
        harness
            .publish(SessionForkRequested {
                source_session_id: source_id.clone(),
                at_ordinal: 1,
            })
            .await;
        let written = poll_until(|| async { !store.written().is_empty() }).await;
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
        let arrived =
            poll_until(|| async { fixture.state.read().session.active_session_id() != &source_id })
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
        let (harness, state, _picker) = controlled_actor_fixture(store.clone()).await;
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
        let released = poll_until(|| async { !state.read().session.is_loading() }).await;

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
        let (harness, state, _picker) = controlled_actor_fixture(store.clone()).await;
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
            poll_until(|| async { store.inner.save_calls.load(Ordering::SeqCst) > 1 }).await;
        assert!(attempted, "the fork should have tried to write the child");
        let released = poll_until(|| async { !state.read().session.is_loading() }).await;

        // Then nothing is left spinning, and the source is still on screen.
        assert!(
            released,
            "a fork that could not be written must not strand the loading indication"
        );
        assert_eq!(state.read().session.active_session_id(), &source_id);
    }
}
