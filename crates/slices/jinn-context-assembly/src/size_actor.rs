//! Context size actor - recalculates active session context size after changes.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's
//! `jinn.context-assembly` topic (fed by the kernel bridge's forward
//! routes). It folds the events that affect context size and runs
//! `assemble_prompt()` to update `cached_context_size` for the active
//! session. Uses eager recalculation — each event triggers an immediate
//! assembly.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::inputs_snapshot::assemble_via_service;
use crate::inputs_snapshot::build_assembly_inputs as build_assembly_inputs_snapshot;
use jinn_context_assembly_msg::ContextOverrideChanged;
use jinn_kernel::common::state::State;
use jinn_kernel::protocol::system::ActiveSessionChanged;
use jinn_session_history_msg::ChatEntryPinChanged;
use jinn_session_history_msg::HistoryAppended;
use jinn_session_store_msg::SessionLoadCompleted;
use tracing::error;

/// The live `handoff_compaction.enabled` flag, for callers building
/// assembly inputs without the config layer in hand.
fn handoff_enabled(services: &jinn_kernel::Services) -> bool {
    services
        .config
        .get::<jinn_preferences_config::schemas::HandoffCompactionConfig>()
        .is_ok_and(|config| config.enabled)
}

/// The context size actor's static trouper path.
pub const CONTEXT_SIZE_PATH: &str = "context-size";

/// Recalculates context size for the active session after context-affecting changes.
///
/// Folds the events that change what's included in the assembled prompt
/// (history additions, context overrides, pin changes, session switches)
/// and updates `cached_context_size` so the status bar stays accurate.
pub struct ContextSizeActor {
    /// Shared application state.
    state: State,
    /// Runtime services (the trouper system for assembly asks).
    services: jinn_kernel::common::services::Services,
}

impl ServiceActor for ContextSizeActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle and
        // services via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ContextSizeActor is spawned via init_with"),
        )
    }
}

impl ContextSizeActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the context-assembly topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(
        system: &ActorSystem,
        state: State,
        services: jinn_kernel::common::services::Services,
    ) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(CONTEXT_SIZE_PATH))
            .init_with({
                move || {
                    let state = state.clone();
                    let services = services.clone();
                    Box::pin(async move { Ok(Self { state, services }) })
                }
            })
            .handles::<HistoryAppended>()
            .handles::<ContextOverrideChanged>()
            .handles::<ActiveSessionChanged>()
            .handles::<ChatEntryPinChanged>()
            .handles::<SessionLoadCompleted>()
            .start()
    }

    /// Recalculate context size for the active session.
    ///
    /// The CPU-intensive `assemble_prompt` call is moved into
    /// `tokio::task::spawn_blocking` to avoid consuming the async worker's
    /// coop budget during startup bursts.
    ///
    /// The session's size generation is snapshotted alongside the assembly
    /// inputs and re-checked before the write: a compaction apply
    /// invalidates the cache and bumps the generation while this task is
    /// in flight, and a recalc that snapshotted before the fold must not
    /// re-poison the cache with the pre-fold total after it. A moved
    /// generation discards the write — the fold's own events schedule a
    /// fresh recalc, so the next honest number lands anyway.
    pub(crate) async fn recalculate(&self) {
        let (session_id, generation_snapshot) = {
            let state = self.state.read();
            (
                state.session.active_session_id().clone(),
                state
                    .try_session(state.session.active_session_id())
                    .map(jinn_session_state::ChatSessionState::size_generation),
            )
        };

        let state_clone = self.state.clone();
        let services_clone = self.services.clone();
        let id_for_blocking = session_id.clone();
        let result = async {
            let inputs = {
                let guard = state_clone.read();
                let mut inputs = build_assembly_inputs_snapshot(&guard, &id_for_blocking);
                inputs.handoff_enabled = handoff_enabled(&services_clone);
                inputs
            };
            assemble_via_service(&self.services, inputs).await
        }
        .await;

        match result {
            Ok(prompt) => {
                let Some(snapshotted) = generation_snapshot else {
                    return;
                };
                // The staleness window is the whole ask above: a fold's
                // synchronous drain ran on this session while the assembly
                // was in flight, invalidating the cache and bumping the
                // generation. The re-read below sees the moved value, and
                // the write is discarded — the pre-fold total never lands.
                // The fold's own HistoryAppended event schedules a fresh
                // recalc, so the next honest number arrives anyway.
                let stale = {
                    let state = self.state.read();
                    state
                        .try_session(&session_id)
                        .is_some_and(|session| session.size_generation_moved_since(snapshotted))
                };
                if stale {
                    tracing::debug!(
                        session_id = %session_id,
                        "context-size recalculate superseded by a compaction invalidation; discarding write"
                    );
                    return;
                }
                let assembled_tokens = prompt.estimated_tokens();
                let system_tokens = prompt.system_tokens();
                self.state.with_session(|view| {
                    if let Some(session) = view.session.map().get_mut(&session_id) {
                        session.set_context_sizes(assembled_tokens, system_tokens);
                    }
                });
            }
            Err(join_err) => {
                error!(error = %join_err, "context-size recalculate task failed");
            }
        }
    }
}

impl MsgHandler<HistoryAppended> for ContextSizeActor {
    async fn handle(&mut self, _msg: &HistoryAppended, _ctx: &mut MsgCtx<'_>) {
        self.recalculate().await;
    }
}

impl MsgHandler<ContextOverrideChanged> for ContextSizeActor {
    async fn handle(&mut self, _msg: &ContextOverrideChanged, _ctx: &mut MsgCtx<'_>) {
        self.recalculate().await;
    }
}

impl MsgHandler<ActiveSessionChanged> for ContextSizeActor {
    async fn handle(&mut self, _msg: &ActiveSessionChanged, _ctx: &mut MsgCtx<'_>) {
        self.recalculate().await;
    }
}

impl MsgHandler<ChatEntryPinChanged> for ContextSizeActor {
    async fn handle(&mut self, _msg: &ChatEntryPinChanged, _ctx: &mut MsgCtx<'_>) {
        self.recalculate().await;
    }
}

impl MsgHandler<SessionLoadCompleted> for ContextSizeActor {
    async fn handle(&mut self, _msg: &SessionLoadCompleted, _ctx: &mut MsgCtx<'_>) {
        self.recalculate().await;
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
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::protocol::ChatEntry;
    use jinn_session_state::ChatSessionState;

    async fn test_actor() -> ContextSizeActor {
        let services = jinn_kernel::Services::new_fake().await;
        {
            // Spawn the service directly: this crate IS the slice under test.
            let _ = crate::service::spawn(&services.trouper_system);
        }
        ContextSizeActor {
            state: State::new(AppState::default()),
            services,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_updates_context_size_for_active_session() {
        // Given an actor with a session that has history.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write();
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
            let mut state = actor.state.write();
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
            let mut state = actor.state.write();
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

    #[rstest::rstest]
    #[tokio::test]
    async fn recalculate_writes_the_system_share_alongside_the_total() {
        // Given an actor with a session that has history.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello world"));
            state.session.active_session_id().clone()
        };

        // When recalculating.
        actor.recalculate().await;

        // Then both halves of the size pair are set, and the work size is
        // the total minus the system share — the gate's input is defined.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let total = session.context_size().expect("total set");
        let system = session
            .work_size()
            .map(|work| total - work)
            .expect("work size defined");
        assert!(system > 0, "system share should be positive");
        assert!(
            system < total,
            "system share must be part of the total: {system} vs {total}"
        );
        assert_eq!(
            session.work_size(),
            Some(total.saturating_sub(system)),
            "work size is total minus system"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_write_after_a_compaction_invalidation_is_discarded() {
        // Given an actor whose session carries a pending fold batch: the
        // recalc snapshots the generation, the assembly ask runs, and the
        // fold invalidates the cache while that ask is in flight.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            let entry = ChatEntry::user("work that will fold");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.queue_mutations(vec![jinn_core_types::HistoryMutation::SetContextOverride {
                entry_id,
                value: jinn_core_types::ContextOverride::ForcedExclude,
                source: jinn_core_types::ChangeSource::Worker {
                    name: "handoff-compaction".to_owned(),
                },
            }]);
            state.session.active_session_id().clone()
        };
        let invalidate_during_ask = {
            let state = actor.state.clone();
            let fold_session = session_id.clone();
            async move {
                // Yield until the actor's snapshot is taken and the ask is
                // underway, then run the fold's synchronous drain.
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                state.with_session(|view| {
                    let _ = view
                        .session
                        .map()
                        .get_or_create(&fold_session)
                        .drain_and_apply_pending_mutations();
                });
            }
        };
        let ((), ()) = tokio::join!(actor.recalculate(), invalidate_during_ask);

        // When the recalc finishes against the moved generation.
        // (Both tasks completed above.)

        // Then the write is discarded: the cache stays invalidated — the
        // pre-fold total never re-poisons it.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        assert!(
            session.context_size().is_none(),
            "stale write must not land"
        );
        assert_eq!(session.work_size(), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_write_at_an_unmoved_generation_lands() {
        // Given an actor whose recalc runs with no invalidation in flight.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello world"));
            state.session.active_session_id().clone()
        };

        // When the recalc completes.
        actor.recalculate().await;

        // Then the honest sizes land.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        assert!(session.context_size().is_some(), "fresh write should land");
        assert!(session.work_size().is_some());
    }
}
