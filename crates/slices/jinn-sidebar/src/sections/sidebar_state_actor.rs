//! Sidebar state actor — owns the sidebar's own state transitions.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's `jinn.sidebar`
//! topic (fed by the kernel bridge's forward routes). It folds
//! [`SessionRemoved`] into the sidebar cursor and active session, and it owns
//! the session preview's lifecycle: arming a render, completing it, and giving
//! up on it when the deadline passes.
//!
//! The preview lives here rather than in an actor of its own because the
//! sidebar sections' cell is already reachable from this actor's [`State`], and
//! a second actor would need its own spawn, its own deps, and its own readiness
//! point to write the same cell.

use std::sync::Arc;
use std::time::Duration;

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::sections::sessions;
use jinn_chat_log_view_msg::{ArmPreviewDeadline, SessionPreviewRendered};
use jinn_kernel::common::state::State;
use jinn_session_lifecycle_msg::SessionTeardownFinished;
use jinn_session_msg::{SessionArchiveFailed, SessionRemoved};
use jinn_session_store_msg::SessionLibraryMembershipChanged;

/// The sidebar state actor's static trouper path.
pub const SIDEBAR_STATE_PATH: &str = "sidebar-state";

/// How long a preview render may run before it is abandoned.
///
/// The chat log's own deadline is 30 seconds, which is generous for a five-entry
/// preview and would leave a spinner up far too long if a worker wedged.
pub const PREVIEW_DEADLINE: Duration = Duration::from_secs(5);

/// Returns the bots cursor to the row its bot occupied, when that
/// bot is the one that was removed.
///
/// The bots section draws two screen lines per bot, so the row is
/// an index into the section's list — the same unit the section navigates by.
/// A cursor left naming a removed bot resolves to nothing at all, which
/// costs the section its selection band and gives the document no cursor row
/// to scroll by, so this restores it from the row captured before the removal.
fn repair_bot_cursor(
    session: &mut jinn_session_state::SessionMap,
    frontend: &mut jinn_kernel::state::frontend_state::FrontendState,
    removed_id: &jinn_core_types::SessionId,
) {
    let cursor = frontend.with_sections(|sections| sections.bot.selected_id.clone(), || None);
    let names_removed = cursor.as_ref() == Some(removed_id);
    if !names_removed {
        return;
    }
    let count = crate::sections::bots_section::row_count_split(session, frontend);
    if count == 0 {
        return;
    }
    let row = crate::sections::capture_rows::restored_bot_row_split(session, frontend);
    let target = jinn_bot::section_rows::bot_rows_split(session, frontend)
        .get(row)
        .map(|entry| entry.session_id.clone());
    if let Some(target) = target {
        frontend.update_sections(|sections| sections.bot.selected_id = Some(target));
    }
}

/// Actor that adjusts sidebar cursor state in response to session close.
///
/// Holds the shared [`State`] handle, injected at spawn via `init_with`
/// because it cannot ride trouper's JSON args.
pub struct SidebarStateActor {
    state: State,
}

impl ServiceActor for SidebarStateActor {
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle via
        // `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("SidebarStateActor is spawned via init_with"),
        )
    }
}

impl SidebarStateActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the sidebar topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, state: State) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(SIDEBAR_STATE_PATH))
            .init_with({
                move || {
                    let state = state.clone();
                    Box::pin(async move { Ok(Self { state }) })
                }
            })
            .handles::<SessionRemoved>()
            .handles::<SessionArchiveFailed>()
            .handles::<SessionTeardownFinished>()
            // A library move hides the moved session from the Sessions list
            // while leaving it in memory, so the same reconciliation the
            // removal path runs is what lands the cursor on a row that still
            // exists. The store actor publishes this event after every
            // membership write; nothing else declares it.
            .handles::<SessionLibraryMembershipChanged>()
            // NOT `handles::<PreviewSessionRequested>`: that message is a
            // *command*, so trouper routes it to exactly one handler, and a
            // command declared here joins the round-robin with the preview
            // workers. Requests this actor picked were consumed by a handler
            // that only arms a deadline and never renders, so they vanished
            // silently and their popups spun forever. The deadline is armed on
            // the keyboard path instead, where the request is built and no
            // competing handler can intercept it.
            .handles::<SessionPreviewRendered>()
            // The deadline this actor arms for every preview it handles. The
            // layout supervisor subscribes to it; without this declaration the
            // bus's flush gate drops the message, no deadline is ever armed, and
            // a preview whose render never returns spins forever.
            .emits::<ArmPreviewDeadline>()
            .start()
    }

    /// Reconcile sidebar cursor and active session after a session is removed.
    fn handle_session_removed(&self, payload: &SessionRemoved) {
        self.state.with_session_sidebar(|view| {
            sessions::state::clear_in_flight(view.frontend, &payload.session_id);
            sessions::state::repair_visual_parents_after_removal(
                view.session.map(),
                view.frontend,
                &payload.session_id,
                payload.removed_parent.as_ref(),
            );
            repair_bot_cursor(view.session.map(), view.frontend, &payload.session_id);
            sessions::reconcile_split(view.session.map(), view.frontend, payload.was_active);
        });
    }

    /// Land the Sessions cursor after a library membership change.
    ///
    /// Moving a session into the library hides its row from the Sessions list
    /// while the session itself stays live in the map — a cursor left naming
    /// it resolves to nothing, which costs the section its selection band.
    /// The membership flag is already flipped by the time this runs (the store
    /// actor publishes after its write), so re-running the removal path's
    /// reconciliation places the cursor on the row now sitting where the moved
    /// row was, exactly as an archive would. Removing a member re-adds a row
    /// and no cursor is lost, so there is nothing to repair.
    fn handle_library_membership_changed(&self, payload: &SessionLibraryMembershipChanged) {
        if !payload.in_library {
            return;
        }
        self.state.with_session_sidebar(|view| {
            sessions::reconcile_split(view.session.map(), view.frontend, false);
        });
    }

    /// Completes a preview render, ignoring a result for a superseded request.
    fn complete_preview(
        &self,
        session_id: jinn_core_types::SessionId,
        generation: u64,
        signature: u64,
        content_width: u16,
        lines: Arc<Vec<ratatui::text::Line<'static>>>,
    ) {
        self.state.read().frontend.update_sections(|s| {
            s.sessions
                .preview
                .complete(session_id, generation, signature, content_width, lines);
        });
    }

    /// Stops indicating an archive that did not complete, leaving the row live.
    fn handle_session_archive_failed(&self, payload: &SessionArchiveFailed) {
        self.state.with_session_sidebar(|view| {
            sessions::state::clear_in_flight(view.frontend, &payload.session_id);
        });
    }

    /// Stops indicating a teardown that failed, leaving the row live.
    ///
    /// A *successful* teardown deliberately keeps the tint: it is immediately
    /// followed by the archive write, and that write is the work worth showing.
    fn handle_session_teardown_finished(&self, payload: &SessionTeardownFinished) {
        if payload.error.is_none() {
            return;
        }
        self.state.with_session_sidebar(|view| {
            sessions::state::clear_in_flight(view.frontend, &payload.session_id);
        });
    }
}

impl MsgHandler<SessionRemoved> for SidebarStateActor {
    async fn handle(&mut self, msg: &SessionRemoved, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_removed(msg);
    }
}

impl MsgHandler<SessionLibraryMembershipChanged> for SidebarStateActor {
    async fn handle(&mut self, msg: &SessionLibraryMembershipChanged, _ctx: &mut MsgCtx<'_>) {
        self.handle_library_membership_changed(msg);
    }
}

impl MsgHandler<SessionArchiveFailed> for SidebarStateActor {
    async fn handle(&mut self, msg: &SessionArchiveFailed, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_archive_failed(msg);
    }
}

impl MsgHandler<SessionTeardownFinished> for SidebarStateActor {
    async fn handle(&mut self, msg: &SessionTeardownFinished, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_teardown_finished(msg);
    }
}

impl MsgHandler<SessionPreviewRendered> for SidebarStateActor {
    async fn handle(&mut self, msg: &SessionPreviewRendered, _ctx: &mut MsgCtx<'_>) {
        // Destructured into fields rather than handed whole: this crate does not
        // depend on the message crate, and a bus type would leak a dependency
        // the sidebar's own state has no use for.
        self.complete_preview(
            msg.session_id.clone(),
            msg.generation,
            msg.signature,
            msg.content_width,
            Arc::clone(&msg.lines),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_session_state::ChatSessionState;

    /// Marks a session in flight and returns the actor plus the session id.
    fn actor_with_in_flight_session() -> (SidebarStateActor, jinn_core_types::SessionId) {
        let actor = test_actor();
        let id = jinn_core_types::SessionId::new();
        actor
            .state
            .write()
            .frontend
            .update_sections(|s| s.sessions.begin_in_flight(std::slice::from_ref(&id)));
        (actor, id)
    }

    /// Whether the given session is still marked in flight.
    fn still_in_flight(actor: &SidebarStateActor, id: &jinn_core_types::SessionId) -> bool {
        actor
            .state
            .read()
            .frontend
            .with_sections(|s| s.sessions.is_in_flight(id), || false)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_removed_clears_in_flight() {
        // Given a session marked in flight.
        let (actor, id) = actor_with_in_flight_session();

        // When handling SessionRemoved for it.
        actor.handle_session_removed(&SessionRemoved {
            session_id: id.clone(),
            removed_parent: None,
            was_active: true,
        });

        // Then the in-flight mark is cleared.
        assert!(!still_in_flight(&actor, &id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn archive_failure_clears_in_flight() {
        // Given a session marked in flight.
        let (actor, id) = actor_with_in_flight_session();

        // When handling a SessionArchiveFailed for it.
        actor.handle_session_archive_failed(&SessionArchiveFailed {
            session_id: id.clone(),
            error: "write failed".to_owned(),
        });

        // Then the in-flight mark is cleared.
        assert!(!still_in_flight(&actor, &id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn failed_teardown_clears_in_flight() {
        // Given a session marked in flight.
        let (actor, id) = actor_with_in_flight_session();

        // When handling a teardown that reported an error.
        actor.handle_session_teardown_finished(&SessionTeardownFinished {
            session_id: id.clone(),
            error: Some("script failed".to_owned()),
        });

        // Then the in-flight mark is cleared.
        assert!(!still_in_flight(&actor, &id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn successful_teardown_keeps_in_flight() {
        // Given a session marked in flight.
        let (actor, id) = actor_with_in_flight_session();

        // When handling a teardown that reported no error.
        actor.handle_session_teardown_finished(&SessionTeardownFinished {
            session_id: id.clone(),
            error: None,
        });

        // Then the mark survives, because the archive write follows it.
        assert!(still_in_flight(&actor, &id));
    }

    fn test_actor() -> SidebarStateActor {
        SidebarStateActor {
            state: State::new(AppState::default_with_scope_focus()),
        }
    }

    /// The row the sessions cursor is drawn on, resolved through the same list
    /// the rest of the slice reads.
    fn cursor_row(actor: &SidebarStateActor) -> Option<usize> {
        let state = actor.state.read();
        let id = state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None)?;
        crate::sections::sessions::state::sorted_open_sessions(&state)
            .iter()
            .position(|entry| entry.id == id)
    }

    /// Builds `count` extra sessions and returns their ids in the order the
    /// list draws them.
    ///
    /// Row order, not insertion order: roots sort newest-first, so a fixture
    /// that assumed otherwise would address a different session than the row
    /// it named, and the test would pass without ever exercising the cursor.
    fn insert_sessions(actor: &SidebarStateActor, count: usize) -> Vec<jinn_core_types::SessionId> {
        let mut state = actor.state.write();
        let default_id = state.session.active_session_id().clone();
        state.session.remove_without_replacement(&default_id);
        for index in 0..count {
            let mut session = ChatSessionState::new();
            session.set_title(format!("session {index}"));
            state.session.insert(session);
        }
        let ordered: Vec<jinn_core_types::SessionId> =
            crate::sections::sessions::state::sorted_open_sessions(&state)
                .into_iter()
                .map(|entry| entry.id)
                .collect();
        // The removal fixtures take the map's only session out before adding
        // their own, so the active id still names a session that is gone.
        if let Some(first) = ordered.first() {
            state.session.set_active(first.clone());
        }
        ordered
    }

    /// Puts the cursor on the listed row, having first recorded that row the
    /// way the pre-render pass does every frame.
    fn place_cursor_on_row(actor: &SidebarStateActor, row: usize) {
        let state = actor.state.write();
        let id = crate::sections::sessions::state::sorted_open_sessions(&state)
            .get(row)
            .map(|entry| entry.id.clone())
            .expect("the row under test exists in the list it indexes");
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(id));
        crate::sections::capture_rows::capture_sessions_cursor_row(&state);
    }

    /// Removes a session the way the store does, optionally leaving the map's
    /// active id on `retarget_to_row`, and handles the event that follows.
    ///
    /// Returns the active session as it stands afterwards, so a test can prove the
    /// actor moved it rather than leaving the map's own choice alone.
    fn active_after_removal(
        actor: &SidebarStateActor,
        removed_id: &jinn_core_types::SessionId,
        was_active: bool,
        retarget_to_row: Option<usize>,
    ) -> Option<jinn_core_types::SessionId> {
        {
            let mut state = actor.state.write();
            state.session.remove_without_replacement(removed_id);
            let retarget = retarget_to_row.and_then(|row| {
                crate::sections::sessions::state::sorted_open_sessions(&state)
                    .get(row)
                    .map(|entry| entry.id.clone())
            });
            if let Some(retarget) = retarget {
                state.session.set_active(retarget);
            }
        }
        actor.handle_session_removed(&jinn_session_msg::SessionRemoved {
            session_id: removed_id.clone(),
            removed_parent: None,
            was_active,
        });
        let state = actor.state.read();
        Some(state.session.active_session_id().clone())
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_keeps_its_row_when_the_session_it_names_is_removed() {
        // Given four listed sessions with the cursor on the middle one, and
        // that row captured from the last rendered frame.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 4);
        place_cursor_on_row(&actor, 1);

        // When the session the cursor names is removed.
        active_after_removal(&actor, &ids[1], false, None);

        // Then the cursor is on the row the removed session occupied, which
        // now holds the session that shifted up into it.
        assert_eq!(cursor_row(&actor), Some(1));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_moves_to_the_new_last_row_when_removed_from_the_last_one() {
        // Given three listed sessions with the cursor on the last one, and
        // that row captured from the last rendered frame.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 3);
        place_cursor_on_row(&actor, 2);

        // When that last session is removed.
        active_after_removal(&actor, &ids[2], false, None);

        // Then the cursor clamps to the new last row rather than jumping to
        // the top of the list.
        assert_eq!(cursor_row(&actor), Some(1));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_cursor_becomes_active_when_the_session_it_names_was_active() {
        // Given four listed sessions with the cursor on row 1, which is also
        // the active session.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 4);
        {
            let mut state = actor.state.write();
            state.session.set_active(ids[1].clone());
        }
        place_cursor_on_row(&actor, 1);

        // When it is removed and the map's own retarget leaves the active id
        // on the last row — neither where the cursor lands nor the top of the
        // list, so a pass here cannot come from the cursor happening to agree.
        let active = active_after_removal(&actor, &ids[1], true, Some(2));

        // Then the active session is the row the cursor landed on, so the
        // selection band and the active indicator agree.
        let state = actor.state.read();
        let cursor = state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None);
        assert_eq!(active, cursor, "the cursor's new row becomes active");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_active_session_is_unchanged_when_another_session_is_removed() {
        // Given four listed sessions with the cursor on the last one, and a
        // different session active.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 4);
        {
            let mut state = actor.state.write();
            state.session.set_active(ids[0].clone());
        }
        place_cursor_on_row(&actor, 3);

        // When the session the cursor names is removed — one the user was not
        // reading.
        let active = active_after_removal(&actor, &ids[3], false, None);

        // Then the user stays in the conversation they were reading, even
        // though the cursor sits somewhere else.
        assert_eq!(active, Some(ids[0].clone()));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_falls_back_to_the_replacement_when_the_session_it_names_is_replaced() {
        // Given a sidebar actor whose only session the cursor names.
        let actor = test_actor();
        let removed_id = {
            let mut state = actor.state.write();
            let id = state.session.active_session_id().clone();
            state
                .frontend
                .update_sections(|s| s.sessions.selected_id = Some(id.clone()));
            id
        };

        // When that session is replaced by a fresh one.
        {
            let mut state = actor.state.write();
            let replacement = ChatSessionState::new();
            let replacement_id = replacement.session_id().clone();
            state.session.remove_and_replace(&removed_id, replacement);
            state.session.set_active(replacement_id);
        }
        actor.handle_session_removed(&jinn_session_msg::SessionRemoved {
            session_id: removed_id.clone(),
            removed_parent: None,
            was_active: true,
        });

        // Then the cursor names the session that replaced it. The old identity
        // named nothing that exists, and with no row captured there is nothing
        // to restore either — so it falls to the row now drawn first.
        let state = actor.state.read();
        let cursor = state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None)
            .expect("the replacement session is listed");
        assert_ne!(
            cursor, removed_id,
            "the cursor must not still name the old session"
        );
        assert!(
            state.session.contains(&cursor),
            "the cursor must name a session that exists"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_is_cleared_when_nothing_is_listed_anymore() {
        // Given a sidebar actor whose only listed session the cursor names.
        let actor = test_actor();
        let removed_id = {
            let mut state = actor.state.write();
            let id = state.session.active_session_id().clone();
            state.session.remove_without_replacement(&id);
            state
                .frontend
                .update_sections(|s| s.sessions.selected_id = Some(id.clone()));
            id
        };

        // When it goes and nothing takes its place.
        actor.handle_session_removed(&jinn_session_msg::SessionRemoved {
            session_id: removed_id,
            removed_parent: None,
            was_active: true,
        });

        // Then there is no cursor, because there is no session to name.
        let state = actor.state.read();
        assert!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_id.clone(), || None)
                .is_none(),
            "an empty list has no session for the cursor to name"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_is_untouched_when_another_session_is_removed() {
        // Given a sidebar actor with three sessions, cursor on the first.
        let actor = test_actor();
        let (removed_id, cursor_id) = {
            let mut state = actor.state.write();
            let s1 = ChatSessionState::new();
            let s2 = ChatSessionState::new();
            let s3 = ChatSessionState::new();
            let first = s1.session_id().clone();
            let third = s3.session_id().clone();
            state.session.insert(s1);
            state.session.insert(s2);
            state.session.insert(s3);
            state
                .frontend
                .update_sections(|s| s.sessions.selected_id = Some(first.clone()));
            (third, first)
        };

        // When a different session is removed.
        {
            let mut state = actor.state.write();
            state.session.remove_without_replacement(&removed_id);
        }
        actor.handle_session_removed(&jinn_session_msg::SessionRemoved {
            session_id: removed_id,
            removed_parent: None,
            was_active: false,
        });

        // Then the cursor still names the session it always did. With an
        // index this was a clamp that could quietly move the cursor; with an
        // identity, a removal elsewhere cannot reach it.
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_id.clone(), || None),
            Some(cursor_id)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_bot_cursor_keeps_its_row_when_that_bot_is_removed() {
        // Given a parent with three bots, the cursor on the middle one
        // and that row captured from the last rendered frame.
        let actor = test_actor();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        let bot_ids: Vec<_> = {
            let mut state = actor.state.write();
            let default_id = state.session.active_session_id().clone();
            state.session.remove_without_replacement(&default_id);
            state.session.insert(parent.clone());
            (0..3)
                .map(|index| {
                    let mut bot = ChatSessionState::new_bot(&parent, true);
                    bot.set_title(format!("bot {index}"));
                    let id = bot.session_id().clone();
                    state.session.insert(bot);
                    id
                })
                .collect()
        };
        {
            let mut state = actor.state.write();
            state.session.set_active(parent_id.clone());
        }
        {
            let mut state = actor.state.write();
            state
                .frontend
                .update_sections(|s| s.bot.selected_id = Some(bot_ids[1].clone()));
            crate::sections::capture_rows::capture_bot_cursor_row(&state);
        }

        // When that bot is removed.
        {
            let mut state = actor.state.write();
            state.session.remove_without_replacement(&bot_ids[1]);
        }
        actor.handle_session_removed(&jinn_session_msg::SessionRemoved {
            session_id: bot_ids[1].clone(),
            removed_parent: Some(parent_id),
            was_active: false,
        });

        // Then the cursor names the bot now on the row it occupied.
        let state = actor.state.read();
        let cursor = state
            .frontend
            .with_sections(|s| s.bot.selected_id.clone(), || None);
        let rows = jinn_bot::section_rows::bot_rows(&state);
        let index = rows
            .iter()
            .position(|row| Some(&row.session_id) == cursor.as_ref())
            .expect("the cursor names a listed bot");
        assert_eq!(index, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn library_move_lands_sessions_cursor_on_a_listed_row() {
        // Given four listed sessions with the cursor on the second one, and
        // that row captured from the last rendered frame.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 4);
        place_cursor_on_row(&actor, 1);

        // When the session the cursor names moves into the library — the
        // flag flipped in the map first, the event second, the way the
        // store actor publishes it.
        {
            let mut state = actor.state.write();
            state
                .session
                .get_mut(&ids[1])
                .expect("fixture session")
                .set_in_library(true);
        }
        actor.handle_library_membership_changed(&SessionLibraryMembershipChanged {
            session_id: ids[1].clone(),
            in_library: true,
        });

        // Then the cursor sits on the row the moved session occupied, which
        // now holds the session that shifted up into it.
        assert_eq!(cursor_row(&actor), Some(1));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn library_removal_leaves_the_sessions_cursor_alone() {
        // Given four listed sessions with the cursor on the second one.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 4);
        place_cursor_on_row(&actor, 1);

        // When a *different* session's membership clears — the cursor's own
        // session never left the list, so there is nothing to repair.
        actor.handle_library_membership_changed(&SessionLibraryMembershipChanged {
            session_id: ids[3].clone(),
            in_library: false,
        });

        // Then the cursor still names the session it named.
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_id.clone(), || None),
            Some(ids[1].clone())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_cursor_lands_on_the_repair_inserted_session_when_the_last_listed_row_moves() {
        // Given the fixture's single listed session with the cursor on it —
        // the move-about-to-empty-the-list shape — and the store's repair
        // having inserted a fresh session, the way the store actor runs it:
        // flag flipped and fresh session inserted before the event publishes.
        let actor = test_actor();
        let ids = insert_sessions(&actor, 1);
        place_cursor_on_row(&actor, 0);
        let repair_id = {
            let mut state = actor.state.write();
            let mut fresh = ChatSessionState::new();
            fresh.set_title("the fresh one".to_owned());
            state.session.insert(fresh.clone());
            fresh.session_id().clone()
        };

        // When the move and its announcement land.
        {
            let mut state = actor.state.write();
            state
                .session
                .get_mut(&ids[0])
                .expect("fixture session")
                .set_in_library(true);
        }
        actor.handle_library_membership_changed(&SessionLibraryMembershipChanged {
            session_id: ids[0].clone(),
            in_library: true,
        });

        // Then the cursor names the repair-inserted session — the only row
        // the list has. Without the insert the list would be empty and the
        // reconciliation would clear the cursor to nothing.
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_id.clone(), || None),
            Some(repair_id)
        );
    }
}
