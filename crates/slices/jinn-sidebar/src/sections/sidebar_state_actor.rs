//! Sidebar state actor — keeps sidebar cursor in sync after session close.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's `jinn.sidebar`
//! topic (fed by the kernel bridge's forward routes). It folds
//! [`SessionClosed`] into the sidebar cursor: clamps `selected_index`
//! and `scroll_offset` so they never point past the end of the sessions
//! list.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::sections::sessions;
use jinn_domain::common::state::State;
use jinn_domain::feat::session::protocol::session_closed::SessionClosed;

/// The sidebar state actor's static trouper path.
pub const SIDEBAR_STATE_PATH: &str = "sidebar-state";

/// The sidebar slice's crossing topic (`jinn.sidebar`): kernel session
/// events forward onto it for the slice's actors.
#[must_use]
pub fn sidebar_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.sidebar")
}

/// Actor that adjusts sidebar cursor state in response to session close.
///
/// Holds the shared [`State`] handle and the two write capabilities —
/// injected at spawn via `start_with` (they cannot ride trouper's JSON
/// args).
pub struct SidebarStateActor {
    state: State,
    session_cap: jinn_domain::common::tcaps::session::SessionCap,
    frontend_cap: jinn_domain::common::tcaps::frontend::FrontendCap,
}

impl ServiceActor for SidebarStateActor {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle and
        // capabilities via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("SidebarStateActor is spawned via start_with"),
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
            .start_with({
                move || {
                    let state = state.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                            frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                        })
                    })
                }
            })
            .handles::<SessionClosed>()
            .start()
    }

    /// Reconcile sidebar cursor and active session after a session is closed.
    fn handle_session_closed(&self, _payload: &SessionClosed) {
        self.state
            .with_session_sidebar(&self.session_cap, &self.frontend_cap, |view| {
                sessions::reconcile_split(view.session.map(), view.frontend);
            });
    }
}

impl MsgHandler<SessionClosed> for SidebarStateActor {
    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_closed(&msg);
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
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::state::State;
    use jinn_domain::feat::session::chat_session::ChatSessionState;

    fn test_actor() -> SidebarStateActor {
        SidebarStateActor {
            state: State::new(AppState::default_with_scope_focus()),
            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clamps_selected_index_after_session_removed() {
        // Given a sidebar actor with three sessions and cursor at index 2.
        let actor = test_actor();
        let removed_id = {
            let mut state = actor.state.write_test_no_cap();
            // Remove default session so we control exact count.
            let default_id = state.session.active_session_id().clone();
            state.session.remove_without_replacement(&default_id);

            let s1 = ChatSessionState::new();
            let s2 = ChatSessionState::new();
            let s3 = ChatSessionState::new();
            let id3 = s3.session_id().clone();
            state.session.insert(s1);
            state.session.insert(s2);
            state.session.insert(s3);
            state.session.set_active(id3.clone());
            state
                .frontend
                .update_sections(|s| s.sessions.selected_index = Some(2));
            id3
        };

        // Simulate the session being removed (as the session actor would do).
        {
            let mut state = actor.state.write_test_no_cap();
            state.session.remove_without_replacement(&removed_id);
        }

        // When handling SessionClosed.
        let payload = jinn_domain::feat::session::protocol::session_closed::SessionClosed {
            session_id: removed_id,
        };
        actor.handle_session_closed(&payload);

        // Then selected_index is clamped to 1 (max valid index).
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(1)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handles_removal_of_last_session_cursor_at_zero() {
        // Given a sidebar actor with one session and cursor at 0.
        let actor = test_actor();
        let removed_id = {
            let mut state = actor.state.write_test_no_cap();
            let id = state.session.active_session_id().clone();
            state
                .frontend
                .update_sections(|s| s.sessions.selected_index = Some(0));
            id
        };

        // Simulate session close + new session creation (as session actor would do).
        {
            let mut state = actor.state.write_test_no_cap();
            state
                .session
                .remove_and_replace(&removed_id, ChatSessionState::new());
        }

        // When handling SessionClosed.
        let payload = jinn_domain::feat::session::protocol::session_closed::SessionClosed {
            session_id: removed_id,
        };
        actor.handle_session_closed(&payload);

        // Then cursor stays at 0.
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(0)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_stays_when_index_still_valid() {
        // Given a sidebar actor with three sessions and cursor at index 0.
        let actor = test_actor();
        let removed_id = {
            let mut state = actor.state.write_test_no_cap();
            let s1 = ChatSessionState::new();
            let s2 = ChatSessionState::new();
            let s3 = ChatSessionState::new();
            let id3 = s3.session_id().clone();
            state.session.insert(s1);
            state.session.insert(s2);
            state.session.insert(s3);
            state
                .frontend
                .update_sections(|s| s.sessions.selected_index = Some(0));
            id3
        };

        // Simulate removal of the last session (cursor at 0 is still valid).
        {
            let mut state = actor.state.write_test_no_cap();
            state.session.remove_without_replacement(&removed_id);
        }

        // When handling SessionClosed.
        let payload = jinn_domain::feat::session::protocol::session_closed::SessionClosed {
            session_id: removed_id,
        };
        actor.handle_session_closed(&payload);

        // Then cursor stays at 0.
        let state = actor.state.read();
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(0)
        );
    }
}
