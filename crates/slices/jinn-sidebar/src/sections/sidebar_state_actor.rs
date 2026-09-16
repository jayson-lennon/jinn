//! Sidebar state actor — keeps sidebar cursor in sync after session close.
//!
//! Subscribes to [`SessionClosed`] events and clamps the sidebar's
//! `selected_index` and `scroll_offset` so they never point past the end
//! of the sessions list.

use kameo::actor::ActorRef;
use kameo::prelude::{Context, Message};

use crate::sections::sessions;
use jinn_domain::common::actor_deps::ActorDeps;
use jinn_domain::common::state::State;
use jinn_domain::feat::session::protocol::session_closed::SessionClosed;

/// Actor that adjusts sidebar cursor state in response to session close.
pub struct SidebarStateActor {
    state: State,
    session_cap: jinn_domain::common::tcaps::session::SessionCap,
    frontend_cap: jinn_domain::common::tcaps::frontend::FrontendCap,
}

/// Dependencies for [`SidebarStateActor`].
#[derive(Clone)]
pub struct SidebarStateActorDeps {
    /// Common actor dependencies (services + bus).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Capability for session writes (active session reconciliation).
    pub session_cap: jinn_domain::common::tcaps::session::SessionCap,
    /// Capability for frontend writes (sidebar cursor state).
    pub frontend_cap: jinn_domain::common::tcaps::frontend::FrontendCap,
}

impl kameo::Actor for SidebarStateActor {
    type Args = SidebarStateActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<SessionClosed>())
            .await;

        Ok(Self {
            state: args.state,
            session_cap: args.session_cap,
            frontend_cap: args.frontend_cap,
        })
    }
}

impl Message<SessionClosed> for SidebarStateActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_session_closed(&msg);
    }
}

impl SidebarStateActor {
    /// Reconcile sidebar cursor and active session after a session is closed.
    fn handle_session_closed(&self, _payload: &SessionClosed) {
        self.state
            .with_session_sidebar(&self.session_cap, &self.frontend_cap, |view| {
                sessions::reconcile_split(view.session.map(), view.frontend);
            });
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
