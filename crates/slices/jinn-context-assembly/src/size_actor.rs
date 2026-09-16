//! Context size actor - recalculates active session context size after changes.
//!
//! Subscribes to events that affect context size and runs `assemble_prompt()`
//! to update `cached_context_size` for the active session. Uses eager
//! recalculation — each event triggers an immediate assembly.

use jinn_domain::common::actor_deps::{ActorDeps, BusPublish};
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::common::state::State;
use jinn_domain::feat::context::protocol::event::ChatEntryPinChanged;
use jinn_domain::feat::context::protocol::event::ContextOverrideChanged;
use jinn_domain::feat::context::snapshot::{assemble_via_service, build_assembly_inputs};
use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;
use jinn_domain::feat::session::protocol::history_appended::HistoryAppended;
use jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use jinn_domain::protocol::system::ActiveSessionChanged;
use kameo::actor::ActorRef;
use kameo::prelude::{Context, Message};
use tracing::error;

/// Recalculates context size for the active session after context-affecting changes.
///
/// Subscribes to events that change what's included in the assembled prompt
/// (history additions, context overrides, pin changes, session switches)
/// and updates `cached_context_size` so the status bar stays accurate.
pub struct ContextSizeActor {
    /// Shared application state.
    state: State,
    /// Token counter for prompt assembly.
    counter: TiktokenCounter,
    /// Bus for message routing.
    bus: BusService,
    /// Authority to write assembled context size into sessions.
    session_cap: jinn_domain::common::tcaps::session::SessionCap,
    /// Runtime services (the trouper system for assembly asks).
    services: jinn_domain::common::services::Services,
}

/// Dependencies for [`ContextSizeActor`].
#[derive(Clone)]
pub struct ContextSizeActorDeps {
    /// Common actor dependencies.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Token counter for prompt assembly.
    pub counter: TiktokenCounter,
    /// Authority to write assembled context size into sessions.
    pub session_cap: jinn_domain::common::tcaps::session::SessionCap,
}

impl BusPublish for ContextSizeActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl kameo::Actor for ContextSizeActor {
    type Args = ContextSizeActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.clone().recipient::<HistoryAppended>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<ContextOverrideChanged>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<ActiveSessionChanged>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<ChatEntryPinChanged>())
            .await;
        args.deps
            .subscribe(actor_ref.recipient::<SessionLoadCompleted>())
            .await;

        Ok(Self {
            state: args.state,
            counter: args.counter,
            bus: args.deps.services.bus.clone(),
            session_cap: args.session_cap,
            services: args.deps.services.clone(),
        })
    }
}

impl Message<HistoryAppended> for ContextSizeActor {
    type Reply = ();

    async fn handle(&mut self, _msg: HistoryAppended, _ctx: &mut Context<Self, Self::Reply>) {
        self.recalculate().await;
    }
}

impl Message<ContextOverrideChanged> for ContextSizeActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: ContextOverrideChanged,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        self.recalculate().await;
    }
}

impl Message<ActiveSessionChanged> for ContextSizeActor {
    type Reply = ();

    async fn handle(&mut self, _msg: ActiveSessionChanged, _ctx: &mut Context<Self, Self::Reply>) {
        self.recalculate().await;
    }
}

impl Message<ChatEntryPinChanged> for ContextSizeActor {
    type Reply = ();

    async fn handle(&mut self, _msg: ChatEntryPinChanged, _ctx: &mut Context<Self, Self::Reply>) {
        self.recalculate().await;
    }
}

impl Message<SessionLoadCompleted> for ContextSizeActor {
    type Reply = ();

    async fn handle(&mut self, _msg: SessionLoadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.recalculate().await;
    }
}

impl ContextSizeActor {
    /// Recalculate context size for the active session.
    ///
    /// The CPU-intensive `assemble_prompt` call is moved into
    /// `tokio::task::spawn_blocking` to avoid consuming the async worker's
    /// coop budget during startup bursts.
    async fn recalculate(&self) {
        let session_id = {
            let state = self.state.read();
            state.session.active_session_id().clone()
        };

        let state_clone = self.state.clone();
        let _counter = self.counter;
        let id_for_blocking = session_id.clone();
        let result = async {
            let inputs = {
                let guard = state_clone.read();
                build_assembly_inputs(&guard, &id_for_blocking)
            };
            assemble_via_service(&self.services, inputs)
                .await
                .map(|prompt| prompt.estimated_tokens())
        }
        .await;

        match result {
            Ok(assembled_tokens) => {
                let session_id = session_id.clone();
                self.state.with_session(&self.session_cap, |view| {
                    if let Some(session) = view.session.map().get_mut(&session_id) {
                        session.set_context_size(assembled_tokens);
                    }
                });
            }
            Err(join_err) => {
                error!(error = %join_err, "context-size recalculate task failed");
            }
        }
    }
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
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::feat::session::chat_session::ChatSessionState;
    use jinn_domain::protocol::ChatEntry;

    async fn test_actor() -> ContextSizeActor {
        let harness = jinn_domain::common::bus::test_harness::TestHarness::new().await;
        let mut services = jinn_domain::Services::new_fake().await;
        {
            // Spawn the service directly: this crate IS the slice under test.
            crate::service::spawn(&services.trouper_system);
        }
        ContextSizeActor {
            state: State::new(AppState::default()),
            counter: TiktokenCounter::o200k_base(),
            bus: harness.bus(),
            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            services,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_updates_context_size_for_active_session() {
        // Given an actor with a session that has history.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello world"));
            state.session.active_session_id().clone()
        };

        // When recalculating.
        actor.recalculate().await;

        // Then context_size is set to a positive value.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let ctx_size = session.context_size().expect("context size should be set");
        assert!(
            ctx_size > 0,
            "context_size should be positive, got {ctx_size}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_updates_after_entry_added() {
        // Given an actor with empty session.
        let actor = test_actor().await;
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        // Recalculate with empty history.
        actor.recalculate().await;
        let size_before = {
            let state = actor.state.read();
            state
                .session
                .get(&session_id)
                .expect("session")
                .context_size()
                .expect("should be set")
        };

        // When adding an entry and recalculating.
        {
            let mut state = actor.state.write_test_no_cap();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("a long message that adds tokens"));
        }
        actor.recalculate().await;

        // Then context_size increased.
        let size_after = {
            let state = actor.state.read();
            state
                .session
                .get(&session_id)
                .expect("session")
                .context_size()
                .expect("should be set")
        };
        assert!(
            size_after > size_before,
            "context_size should increase after adding entry: {size_after} vs {size_before}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_handles_empty_session_gracefully() {
        // Given an actor with empty session.
        let actor = test_actor().await;

        // When recalculating.
        actor.recalculate().await;

        // Then context_size is set (system prompt only).
        let state = actor.state.read();
        let ctx_size = state
            .active_session()
            .context_size()
            .expect("should be set even for empty session");
        // Should have some tokens from the system prompt / env context.
        assert!(
            ctx_size > 0,
            "context_size should be positive even for empty session, got {ctx_size}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_only_updates_active_session() {
        // Given an actor with two sessions.
        let actor = test_actor().await;
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        {
            let mut state = actor.state.write_test_no_cap();
            state.session.insert(second);
            // Active session has history, second does not.
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
        }

        // When recalculating.
        actor.recalculate().await;

        // Then active session has context_size set, second does not.
        let state = actor.state.read();
        assert!(
            state.active_session().context_size().is_some(),
            "active session should have context_size"
        );
        assert!(
            state
                .session
                .get(&second_id)
                .expect("second")
                .context_size()
                .is_none(),
            "non-active session should not have context_size set"
        );
    }
}
