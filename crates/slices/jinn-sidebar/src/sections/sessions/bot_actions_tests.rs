//! Tests for the bot key actions — `b` (new bot), `B` (standalone bot),
//! and `R` (re-run).
//!
//! `handle_new_bot_of_session` is the only production caller of
//! `ChatSessionState::new_bot`, so the creation path's persistability
//! is proven here against the way production actually builds the session.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::sync::{Arc, Mutex};

use error_stack::Report;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::ChatEntry;
use jinn_session_state::{SessionSnapshot, SessionStore, SessionStoreError};
use jinn_slices::ConfigLayer;

use super::bot_actions::{
    handle_new_bot_of_session, handle_new_standalone_bot, handle_rerun_bot, handle_reset_bot,
    handle_toggle_library, validate_library_add,
};

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

    async fn set_in_library(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _in_library: bool,
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

/// A composed bot with a seeded run, parented to `parent`.
///
/// Built from a real parent session so the parent link — which is what the
/// cascade walk follows — is the one the production path would produce.
/// Composition ends here because every `R` test is about a runnable
/// bot; the refusal is covered by its own test.
fn reset_bot(
    parent: &jinn_session_state::ChatSessionState,
) -> jinn_session_state::ChatSessionState {
    let mut bot = jinn_session_state::ChatSessionState::new_bot(parent, true);
    bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
    bot.set_bot_is_prepping(false);
    bot.set_seed_template("verify: <prior_report>".to_owned());
    bot.append_bot_report("a finding".to_owned());
    bot
}

/// A busy session the cascade can reach as a `spawn_subagent`-spawned subagent.
///
/// Built through `new_child` so it carries the `Subagent` origin the walk
/// recurses on, and registered in the spawn registry, which is where the
/// cascade looks for in-flight subagents — the session map's walk only
/// surfaces bots.
fn busy_subagent(parent: &jinn_core_types::SessionId) -> jinn_session_state::ChatSessionState {
    let mut session = jinn_session_state::ChatSessionState::new_child(parent, true);
    session.set_title("zzz-subagent".to_owned());
    session.begin_streaming();
    session
}

/// A busy session reached as a nested bot, which the session-map
/// source of the cascade walk finds.
fn busy_nested_bot(
    parent: &jinn_session_state::ChatSessionState,
) -> jinn_session_state::ChatSessionState {
    let mut nested = reset_bot(parent);
    nested.set_title("zzz-nested".to_owned());
    nested.begin_streaming();
    nested
}

#[rstest::rstest]
#[tokio::test]
async fn created_bot_reaches_the_store() {
    // Given a store that drops any snapshot the production store would also drop.
    let store = Arc::new(RecordingStore::default());
    let service = jinn_session_state::SessionStoreService::new(store.clone());

    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();
    let parent_id = state.session.active_session_id().clone();

    // When creating a bot with `N` and saving it as the handler asks.
    let result = handle_new_bot_of_session(&mut state, &empty_config());
    let bot_id = state.session.active_session_id().clone();
    let snapshot = state
        .session
        .get(&bot_id)
        .expect("bot exists")
        .capture_snapshot();
    service.save(&snapshot).await.expect("snapshot saves");

    // Then the store holds the bot, parented to the session it came from.
    let saved = store
        .saved_for(&bot_id)
        .expect("the bot was written to the store");
    assert_eq!(saved.metadata.parent_session, Some(parent_id));
    // And the handler still asks for the save.
    assert!(
        result.message_names.contains(&"PersistSession"),
        "the creation path must still request a save, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn standalone_bot_has_no_parent_and_an_empty_seed() {
    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();
    let source_id = state.session.active_session_id().clone();

    // When creating a standalone bot with `B`.
    let _result = handle_new_standalone_bot(&mut state, &empty_config());
    let bot_id = state.session.active_session_id().clone();

    // Then a new bot is active, not the source session.
    assert_ne!(bot_id, source_id, "`B` must activate the new bot");
    let bot = state.session.get(&bot_id).expect("bot exists");
    // And no parent link exists — the source was the environment only.
    assert!(
        bot.parent_session().is_none(),
        "a standalone bot must not carry a parent"
    );
    // And its seed template starts empty: its instructions come from the
    // user, typed after the spawn.
    assert_eq!(bot.seed_template(), "");
    // And it is still persistable, like any `b` bot.
    assert!(bot.is_persistable(), "a standalone bot must be persistable");
}

#[rstest::rstest]
fn standalone_bot_does_not_persist_its_source() {
    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();
    let source_id = state.session.active_session_id().clone();

    // When creating a standalone bot with `B`.
    let result = handle_new_standalone_bot(&mut state, &empty_config());

    // Then the PersistSession it asks for exists (the bot is written),
    // and the source session was never marked interacted — the only way
    // this path writes the source is that mark, and `B` skips it.
    assert!(
        result.message_names.contains(&"PersistSession"),
        "the bot itself must be persisted, published: {:?}",
        result.message_names
    );
    let source = state.session.get(&source_id).expect("source is live");
    assert!(
        !source.is_persistable(),
        "the source session must not have been marked by a standalone spawn"
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
fn creating_an_bot_makes_its_parent_worth_saving() {
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

    // When a bot is created from it.
    handle_new_bot_of_session(&mut state, &empty_config());

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
        "a session with a bot is no longer an empty draft"
    );
}

#[rstest::rstest]
#[test]
fn creating_an_bot_also_saves_a_parent_that_was_never_saved() {
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

    // When a bot is created from it.
    let result = handle_new_bot_of_session(&mut state, &empty_config());
    let bot_id = state.session.active_session_id().clone();
    for message in result.messages {
        message(&collector as &dyn jinn_slices::route_publish::PublishSink);
    }

    // Then the parent is saved alongside the child. The child names the
    // parent in its own row, so writing the child while the parent is
    // unwritten is a bot pointing at a session that does not exist:
    // a tree with a hole where the trunk should be, invisible until
    // something walks up to the parent.
    let saved = collector.ids.lock().expect("collector lock").clone();
    assert!(
        saved.contains(&bot_id.to_string()),
        "the bot itself must still be saved: {saved:?}"
    );
    assert!(
        saved.contains(&parent_id.to_string()),
        "the parent must be saved too, or the child is orphaned: {saved:?}"
    );
}

#[rstest::rstest]
#[test]
fn rerun_cancels_the_bots_busy_descendants() {
    // Given an idle bot — the only kind `R` accepts — with a busy
    // nested bot and a busy subagent beneath it. The bot is
    // titled and made active so row 0 resolves to it whatever the creation
    // order happens to be.
    let mut state = state_with_selected_row(0);
    let bot_id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-bot".to_owned());
        let bot_id = bot.session_id().clone();

        let nested = busy_nested_bot(&bot);
        let subagent = busy_subagent(&bot_id);
        // A subagent is only discoverable through the spawn registry; the
        // session map's walk only surfaces bots.
        state
            .task_spawns
            .register(bot_id.clone(), subagent.session_id().clone());

        state.session.insert(subagent);
        state.session.insert(nested);
        state.session.insert(bot);
        state.session.set_active(bot_id.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id.clone()));
        bot_id
    };
    // Guard the fixture: `R` acts on the cursor, and the test only means
    // something if the cursor is the bot rather than a descendant.
    let highlighted = state
        .frontend
        .with_sections(|s| s.sessions.selected_id.clone(), || None);
    assert_eq!(
        highlighted.as_ref(),
        Some(&bot_id),
        "fixture must put the cursor on the bot, not a descendant"
    );

    // When `R` re-runs the bot.
    let result = handle_rerun_bot(&mut state);

    // Then both descendants are cancelled. They exist only to answer the
    // question this bot is re-asking, so a stale turn under either is
    // answering a question nobody asked any more.
    assert_eq!(
        cancel_count(&result),
        2,
        "the nested bot and the subagent must both be cancelled, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn rerun_does_not_cancel_below_a_fork() {
    // Given an idle bot with a fork beneath it holding a busy
    // grandchild — the same shape as above, but through a fork.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-bot".to_owned());
        let bot_id = bot.session_id().clone();
        let mut fork = jinn_session_state::ChatSessionState::new();
        fork.set_title("mmm-fork".to_owned());
        fork.restore_parent_session(Some(bot_id.clone()));
        let fork_id = fork.session_id().clone();
        let grandchild = busy_subagent(&fork_id);
        // The walk reaches the fork through the registry, then stops: a fork
        // is an independent thread with no standing to be descended into.
        state.task_spawns.register(bot_id.clone(), fork_id.clone());
        state
            .task_spawns
            .register(fork_id, grandchild.session_id().clone());
        state.session.insert(grandchild);
        state.session.insert(fork);
        state.session.insert(bot);
        state.session.set_active(bot_id);
    }

    // When `R` re-runs the bot.
    let result = handle_rerun_bot(&mut state);

    // Then nothing is cancelled. A plain idle session is not a bot
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
fn rerun_on_a_composing_bot_is_still_refused() {
    // Given a bot still composing its instructions, so `R` is
    // refused however the trigger is set.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        // Left in prep mode, which is the state `N` creates a bot in.
        let bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        state.session.insert(bot);
    }

    // When `R` re-runs it.
    let result = handle_rerun_bot(&mut state);

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
fn created_bot_is_persistable() {
    // Given the sessions section focused with a session highlighted.
    let mut state = state_with_selected_session();

    // When creating a bot with `N`.
    let _result = handle_new_bot_of_session(&mut state, &empty_config());

    // Then the new bot is persistable.
    let bot_id = state.session.active_session_id().clone();
    assert!(
        state
            .session
            .get(&bot_id)
            .expect("bot exists")
            .is_persistable(),
        "a bot created by `N` must be persistable"
    );
}

#[rstest::rstest]
#[test]
fn move_to_library_rejected_for_parented_bot() {
    // Given the sessions section focused on a bot that has a parent.
    let state = {
        let mut state = state_with_selected_row(0);
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        bot.set_title("aaa-parented-bot".to_owned());
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id));
        state
    };

    // When validating the move into the library.
    let result = validate_library_add(&state);

    // Then the parented bot is refused — shelved in the library while
    // archived, a ParentCompleted bot would silently never fire.
    assert!(matches!(
        result,
        Err(super::bot_actions::LibraryError::BlockedParentedBot)
    ));
}

#[rstest::rstest]
#[test]
fn move_to_library_for_a_parented_bot_publishes_nothing() {
    // Given the sessions section focused on a bot that has a parent.
    let mut state = {
        let mut state = state_with_selected_row(0);
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        bot.set_title("aaa-parented-bot".to_owned());
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id));
        state
    };

    // When the toggle is pressed.
    let toggle = handle_toggle_library(&mut state);

    // Then nothing is published — validation refuses the move before any
    // command can form.
    assert!(
        toggle.message_names.is_empty(),
        "a refused move must publish nothing, published: {:?}",
        toggle.message_names
    );
}

#[rstest::rstest]
#[test]
fn move_to_library_allows_parented_non_bot() {
    // Given the sessions section focused on a fork (a session with a parent
    // that is not a bot).
    let state = {
        let mut state = state_with_selected_row(0);
        let source = jinn_session_state::ChatSessionState::new();
        let mut fork = jinn_session_state::ChatSessionState::new();
        fork.restore_parent_session(Some(source.session_id().clone()));
        fork.set_title("mmm-fork".to_owned());
        let fork_id = fork.session_id().clone();
        state.session.insert(fork);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(fork_id));
        state
    };

    // When validating the move into the library.
    let result = validate_library_add(&state);

    // Then it is allowed — only a bot with a parent is blocked.
    assert!(
        result.is_ok(),
        "a parented non-bot must be allowed into the library: {result:?}"
    );
}

#[rstest::rstest]
#[test]
fn move_to_library_publishes_the_membership_command() {
    // Given the sessions section focused on a fork (a session with a parent
    // that is not a bot).
    let mut state = {
        let mut state = state_with_selected_row(0);
        let source = jinn_session_state::ChatSessionState::new();
        let mut fork = jinn_session_state::ChatSessionState::new();
        fork.restore_parent_session(Some(source.session_id().clone()));
        fork.set_title("mmm-fork".to_owned());
        let fork_id = fork.session_id().clone();
        state.session.insert(fork);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(fork_id));
        state
    };

    // When the toggle is pressed.
    let toggle = handle_toggle_library(&mut state);

    // Then the library command is published.
    assert!(
        toggle
            .message_names
            .iter()
            .any(|name| name.ends_with("SetSessionLibrary")),
        "the move must publish SetSessionLibrary, published: {:?}",
        toggle.message_names
    );
}

#[rstest::rstest]
#[test]
fn reset_key_force_excludes_non_pinned_context() {
    // Given an idle bot with a pinned entry and two ordinary ones.
    let mut state = state_with_selected_row(0);
    let bot_id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-reset-bot".to_owned());
        let pinned = bot.push_entry(ChatEntry::user("pinned instruction"));
        bot.pin_entry(
            &bot.history()[pinned].id.clone(),
            jinn_core_types::PinPosition::Top,
        );
        bot.push_entry(ChatEntry::assistant("unpinned one"));
        bot.push_entry(ChatEntry::user("unpinned two"));
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id.clone()));
        bot_id
    };

    // When `R` resets the bot.
    let _result = handle_reset_bot(&mut state);

    // Then exactly the two non-pinned entries are force-excluded from
    // context, the pin staying out of the exclusion set.
    let session = state.session.get(&bot_id).expect("bot");
    let excluded = session
        .history()
        .iter()
        .filter(|e| e.context_override() == jinn_core_types::ContextOverride::ForcedExclude)
        .count();
    assert_eq!(
        excluded, 2,
        "exactly the two non-pinned entries must be force-excluded"
    );
}

#[rstest::rstest]
#[test]
fn reset_key_dispatches_nothing() {
    // Given an idle bot with a pinned entry and two ordinary ones.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-reset-bot".to_owned());
        let pinned = bot.push_entry(ChatEntry::user("pinned instruction"));
        bot.pin_entry(
            &bot.history()[pinned].id.clone(),
            jinn_core_types::PinPosition::Top,
        );
        bot.push_entry(ChatEntry::assistant("unpinned one"));
        bot.push_entry(ChatEntry::user("unpinned two"));
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id));
    }

    // When `R` resets the bot.
    let result = handle_reset_bot(&mut state);

    // Then nothing is dispatched — no EnqueueUserMessage (the rerun path
    // would publish it for this same state), no CancelTurn.
    assert!(
        !result
            .message_names
            .iter()
            .any(|name| name.ends_with("EnqueueUserMessage")),
        "reset must not dispatch, published: {:?}",
        result.message_names
    );
    assert_eq!(cancel_count(&result), 0);
}

#[rstest::rstest]
#[test]
fn reset_key_persists_the_exclusions() {
    // Given an idle bot with a pinned entry and two ordinary ones.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-reset-bot".to_owned());
        let pinned = bot.push_entry(ChatEntry::user("pinned instruction"));
        bot.pin_entry(
            &bot.history()[pinned].id.clone(),
            jinn_core_types::PinPosition::Top,
        );
        bot.push_entry(ChatEntry::assistant("unpinned one"));
        bot.push_entry(ChatEntry::user("unpinned two"));
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(bot_id));
    }

    // When `R` resets the bot.
    let result = handle_reset_bot(&mut state);

    // Then the change is persisted.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("PersistSession")),
        "the exclusions must be persisted, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn reset_key_blocked_while_bot_busy() {
    // Given a bot that is mid-turn.
    let mut state = state_with_selected_row(0);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let mut bot = reset_bot(&parent);
        bot.set_title("aaa-busy-bot".to_owned());
        bot.begin_streaming();
        state.session.insert(bot);
    }

    // When `R` resets it.
    let result = handle_reset_bot(&mut state);

    // Then the refusal is surfaced as a system line, nothing is excluded,
    // and nothing is dispatched.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("PushChatEntry")),
        "a busy bot's reset must say why, published: {:?}",
        result.message_names
    );
    assert_eq!(cancel_count(&result), 0);
}

#[rstest::rstest]
#[test]
fn reset_key_noop_on_non_bot() {
    // Given the cursor on an ordinary session.
    let mut state = state_with_selected_row(0);
    {
        let mut plain = jinn_session_state::ChatSessionState::new();
        plain.set_title("aaa-plain".to_owned());
        plain.push_entry(ChatEntry::user("hello"));
        state.session.insert(plain);
    }

    // When `R` is pressed.
    let result = handle_reset_bot(&mut state);

    // Then nothing is mutated or published beyond the reason line.
    assert!(
        !result
            .message_names
            .iter()
            .any(|name| name.ends_with("PersistSession")),
        "a non-bot must not be persisted, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[test]
fn rerun_key_moves_from_r_to_g() {
    // Given the sidebar's real route rows attached to a route table.
    let routes = jinn_slices::route::KeyRoutes::new();
    crate::key_routes::attach_sidebar_rows(&routes);
    let sessions_scope = jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id();
    let actions_at = |routes: &jinn_slices::route::KeyRoutes, key: &str| {
        routes
            .rows()
            .iter()
            .filter(|r| r.scope == sessions_scope && r.key == key)
            .filter_map(|r| match &r.outcome {
                jinn_slices::route::RouteOutcome::Action { action, .. } => Some(*action),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // When looking up what `R` and `G` bind to in the sessions scope.
    let at_r = actions_at(&routes, "R");
    let at_g = actions_at(&routes, "G");

    // Then `R` no longer re-runs, `G` does, and `R` resets instead.
    assert!(
        !at_r.contains(&"session-rerun-bot"),
        "the old R-rerun binding must be gone, R binds: {at_r:?}"
    );
    assert!(
        at_g.contains(&"session-rerun-bot"),
        "G must carry the rerun, G binds: {at_g:?}"
    );
    assert!(
        at_r.contains(&"session-reset-bot"),
        "R must carry the reset, R binds: {at_r:?}"
    );
}
