//! Tests for the attendant key actions — `N` (new attendant) and `R` (re-run).
//!
//! `handle_new_attendant` is the only production caller of
//! `ChatSessionState::new_attendant`, so the creation path's persistability
//! is proven here against the way production actually builds the session.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::sync::{Arc, Mutex};

use error_stack::Report;
use jinn_kernel::common::app_state::AppState;
use jinn_session_state::{SessionSnapshot, SessionStore, SessionStoreError};
use jinn_slices::ConfigLayer;

use super::attendant_actions::{handle_new_attendant, handle_rerun_attendant};

/// A recording store that honors the same `persist` gate the real SQLite
/// store applies — a non-persistable snapshot is dropped without a write.
///
/// Mirroring the gate is the whole point: a stub that recorded every snapshot
/// would report success with the production bug fully intact.
#[derive(Debug, Default)]
struct RecordingStore {
    saved: Mutex<Vec<SessionSnapshot>>,
}

impl RecordingStore {
    fn saved_for(&self, session_id: &jinn_core_types::SessionId) -> Option<SessionSnapshot> {
        self.saved
            .lock()
            .unwrap()
            .iter()
            .find(|snapshot| &snapshot.metadata.session_id == session_id)
            .cloned()
    }
}

#[async_trait::async_trait]
impl SessionStore for RecordingStore {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn save(&self, snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
        // Mirrors the store's own gate. The store actor applies a second
        // one — it drops a session that is not persistable — before the
        // snapshot is ever built, which the test that exercises the real
        // handler covers end to end.
        if !snapshot.metadata.persist {
            return Ok(());
        }
        self.saved.lock().unwrap().push(snapshot.clone());
        Ok(())
    }

    async fn load_summaries(
        &self,
    ) -> Result<Vec<jinn_session_store_msg::SessionSummary>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn load_session(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn delete(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &jinn_core_types::SessionId,
        _at_ordinal: usize,
    ) -> Result<jinn_core_types::SessionId, Report<SessionStoreError>> {
        Ok(jinn_core_types::SessionId::new())
    }

    async fn set_archived(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[jinn_core_types::SessionId],
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<jinn_session_store_msg::SessionSummary>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<Vec<jinn_core_types::SessionId>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &jinn_core_types::SessionId,
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
        _session_id: &jinn_core_types::SessionId,
        _anchor: &jinn_core_types::ChatEntryId,
        _context: usize,
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _limit: usize,
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }
}

/// An empty config layer — the creation path reads per-session defaults
/// from it, and none are needed to exercise persistability.
fn empty_config() -> ConfigLayer {
    jinn_config::testutil::config_layer("")
}

/// State with the sessions section focused and its first row highlighted,
/// which is what both `N` and `R` require of the cursor.
fn state_with_selected_session() -> AppState {
    state_with_selected_row(0)
}

/// State with the sessions section focused and `row` highlighted.
fn state_with_selected_row(row: usize) -> AppState {
    let state = AppState::default_with_scope_focus();
    let id = crate::sections::sessions::state::sorted_open_sessions(&state)
        .get(row)
        .map(|entry| entry.id.clone());
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    state
        .frontend
        .update_sections(|s| s.sessions.selected_id = id);
    state
}

/// How many `CancelTurn` messages a result publishes.
fn cancel_count(result: &jinn_slices::route::RouteResult) -> usize {
    result
        .message_names
        .iter()
        .filter(|name| name.ends_with("CancelTurn"))
        .count()
}

/// A composed attendant with a seeded run, parented to `parent`.
///
/// Built from a real parent session so the parent link — which is what the
/// cascade walk follows — is the one the production path would produce.
/// Composition ends here because every `R` test is about a runnable
/// attendant; the refusal is covered by its own test.
fn reset_attendant(
    parent: &jinn_session_state::ChatSessionState,
) -> jinn_session_state::ChatSessionState {
    let mut attendant = jinn_session_state::ChatSessionState::new_attendant(parent, true);
    attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
    attendant.set_attendant_is_prepping(false);
    attendant.set_seed_template("verify: <prior report>".to_owned());
    attendant.append_attendant_report("a finding".to_owned());
    attendant
}

/// A busy session the cascade can reach as a `task`-spawned subagent.
///
/// Built through `new_child` so it carries the `Subagent` origin the walk
/// recurses on, and registered in the spawn registry, which is where the
/// cascade looks for in-flight subagents — the session map's walk only
/// surfaces attendants.
fn busy_subagent(parent: &jinn_core_types::SessionId) -> jinn_session_state::ChatSessionState {
    let mut session = jinn_session_state::ChatSessionState::new_child(parent, true);
    session.set_title("zzz-subagent".to_owned());
    session.begin_streaming();
    session
}

/// A busy session reached as a nested attendant, which the session-map
/// source of the cascade walk finds.
fn busy_nested_attendant(
    parent: &jinn_session_state::ChatSessionState,
) -> jinn_session_state::ChatSessionState {
    let mut nested = reset_attendant(parent);
    nested.set_title("zzz-nested".to_owned());
    nested.begin_streaming();
    nested
}

#[rstest::rstest]
#[tokio::test]
async fn created_attendant_reaches_the_store() {
    // Given a store that drops any snapshot the production store would also drop.
    let store = Arc::new(RecordingStore::default());
    let service = jinn_session_state::SessionStoreService::new(store.clone());

    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();
    let parent_id = state.session.active_session_id().clone();

    // When creating an attendant with `N` and saving it as the handler asks.
    let result = handle_new_attendant(&mut state, &empty_config());
    let attendant_id = state.session.active_session_id().clone();
    let snapshot = state
        .session
        .get(&attendant_id)
        .expect("attendant exists")
        .capture_snapshot();
    service.save(&snapshot).await.expect("snapshot saves");

    // Then the store holds the attendant, parented to the session it came from.
    let saved = store
        .saved_for(&attendant_id)
        .expect("the attendant was written to the store");
    assert_eq!(saved.metadata.parent_session, Some(parent_id));
    // And the handler still asks for the save.
    assert!(
        result.message_names.contains(&"PersistSession"),
        "the creation path must still request a save, published: {:?}",
        result.message_names
    );
}

/// A sink that records the session ids of every `PersistSession` published.
#[derive(Default)]
struct PersistedCollector {
    ids: std::sync::Mutex<Vec<String>>,
}

impl jinn_slices::route_publish::PublishSink for PersistedCollector {
    fn publish_schema(
        &self,
        _schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    ) {
        if name.ends_with("PersistSession") {
            let id = payload["session_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            self.ids.lock().expect("collector lock").push(id);
        }
    }
}

#[rstest::rstest]
#[test]
fn creating_an_attendant_makes_its_parent_worth_saving() {
    // Given a brand new session that has never been persisted. A fresh
    // session is not written on creation, so it exists only in memory —
    // and that is the state every new session starts in.
    let mut state = state_with_selected_session();
    let parent_id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        state.session.insert(parent);
        state.session.set_active(parent_id.clone());
        // The cursor names the session these tests act on, so it has to be
        // repointed at the parent they just introduced.
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(parent_id.clone()));
        parent_id
    };

    // When an attendant is created from it.
    handle_new_attendant(&mut state, &empty_config());

    // Then the parent is worth saving. The store drops any snapshot for a
    // session it does not consider persistable, so a request to save an
    // unwritten parent is discarded before it reaches the database — the
    // request is not enough on its own.
    let parent = state
        .session
        .get(&parent_id)
        .expect("the parent still exists");
    assert!(
        parent.is_persistable(),
        "a session with an attendant is no longer an empty draft"
    );
}

#[rstest::rstest]
#[test]
fn creating_an_attendant_also_saves_a_parent_that_was_never_saved() {
    // Given a sink recording what the handler publishes.
    let collector = PersistedCollector::default();

    // Given a brand new session that has never been persisted. A fresh
    // session is not written on creation, so it exists only in memory —
    // and that is the state every new session starts in.
    let mut state = state_with_selected_session();
    let parent_id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        state.session.insert(parent);
        state.session.set_active(parent_id.clone());
        // The cursor names the session these tests act on, so it has to be
        // repointed at the parent they just introduced.
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(parent_id.clone()));
        parent_id
    };

    // When an attendant is created from it.
    let result = handle_new_attendant(&mut state, &empty_config());
    let attendant_id = state.session.active_session_id().clone();
    for message in result.messages {
        message(&collector as &dyn jinn_slices::route_publish::PublishSink);
    }

    // Then the parent is saved alongside the child. The child names the
    // parent in its own row, so writing the child while the parent is
    // unwritten is an attendant pointing at a session that does not exist:
    // a tree with a hole where the trunk should be, invisible until
    // something walks up to the parent.
    let saved = collector.ids.lock().expect("collector lock").clone();
    assert!(
        saved.contains(&attendant_id.to_string()),
        "the attendant itself must still be saved: {saved:?}"
    );
    assert!(
        saved.contains(&parent_id.to_string()),
        "the parent must be saved too, or the child is orphaned: {saved:?}"
    );
}

#[rstest::rstest]
#[test]
fn rerun_cancels_the_attendants_busy_descendants() {
    // Given an idle attendant — the only kind `R` accepts — with a busy
    // nested attendant and a busy subagent beneath it. The attendant is
    // titled and made active so row 0 resolves to it whatever the creation
    // order happens to be.
    let mut state = state_with_selected_row(0);
    let attendant_id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut attendant = reset_attendant(&parent);
        attendant.set_title("aaa-attendant".to_owned());
        let attendant_id = attendant.session_id().clone();

        let nested = busy_nested_attendant(&attendant);
        let subagent = busy_subagent(&attendant_id);
        // A subagent is only discoverable through the spawn registry; the
        // session map's walk only surfaces attendants.
        state
            .task_spawns
            .register(attendant_id.clone(), subagent.session_id().clone());

        state.session.insert(subagent);
        state.session.insert(nested);
        state.session.insert(attendant);
        state.session.set_active(attendant_id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(attendant_id.clone()));
        attendant_id
    };
    // Guard the fixture: `R` acts on the cursor, and the test only means
    // something if the cursor is the attendant rather than a descendant.
    let highlighted = state
        .frontend
        .with_sections(|s| s.sessions.selected_id.clone(), || None);
    assert_eq!(
        highlighted.as_ref(),
        Some(&attendant_id),
        "fixture must put the cursor on the attendant, not a descendant"
    );

    // When `R` re-runs the attendant.
    let result = handle_rerun_attendant(&mut state);

    // Then both descendants are cancelled. They exist only to answer the
    // question this attendant is re-asking, so a stale turn under either is
    // answering a question nobody asked any more.
    assert_eq!(
        cancel_count(&result),
        2,
        "the nested attendant and the subagent must both be cancelled, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn rerun_does_not_cancel_below_a_fork() {
    // Given an idle attendant with a fork beneath it holding a busy
    // grandchild — the same shape as above, but through a fork.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut attendant = reset_attendant(&parent);
        attendant.set_title("aaa-attendant".to_owned());
        let attendant_id = attendant.session_id().clone();
        let mut fork = jinn_session_state::ChatSessionState::new();
        fork.set_title("mmm-fork".to_owned());
        fork.restore_parent_session(Some(attendant_id.clone()));
        let fork_id = fork.session_id().clone();
        let grandchild = busy_subagent(&fork_id);
        // The walk reaches the fork through the registry, then stops: a fork
        // is an independent thread with no standing to be descended into.
        state
            .task_spawns
            .register(attendant_id.clone(), fork_id.clone());
        state
            .task_spawns
            .register(fork_id, grandchild.session_id().clone());
        state.session.insert(grandchild);
        state.session.insert(fork);
        state.session.insert(attendant);
        state.session.set_active(attendant_id);
    }

    // When `R` re-runs the attendant.
    let result = handle_rerun_attendant(&mut state);

    // Then nothing is cancelled. A plain idle session is not an attendant
    // and not a recursable origin, so the walk stops before publishing.
    assert_eq!(
        cancel_count(&result),
        0,
        "the walk must stop at a fork, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_composing_attendant_is_still_refused() {
    // Given an attendant still composing its instructions, so `R` is
    // refused however the trigger is set.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        // Left in prep mode, which is the state `N` creates an attendant in.
        let attendant = jinn_session_state::ChatSessionState::new_attendant(&parent, true);
        state.session.insert(attendant);
    }

    // When `R` re-runs it.
    let result = handle_rerun_attendant(&mut state);

    // Then nothing is dispatched or cancelled, and the reason is surfaced
    // as a system line rather than a silent no-op.
    assert_eq!(cancel_count(&result), 0);
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("PushChatEntry")),
        "a refused rerun must say why, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn created_attendant_is_persistable() {
    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();

    // When creating an attendant with `N`.
    let _result = handle_new_attendant(&mut state, &empty_config());

    // Then the new attendant is persistable.
    let attendant_id = state.session.active_session_id().clone();
    assert!(
        state
            .session
            .get(&attendant_id)
            .expect("attendant exists")
            .is_persistable(),
        "an attendant created by `N` must be persistable"
    );
}
