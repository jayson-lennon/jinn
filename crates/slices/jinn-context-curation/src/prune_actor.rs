//! The prune actor — evaluates the enabled auto-prune strategies.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's
//! `jinn.context-curation` topic (fed by the kernel bridge's forward
//! routes). On each `HistoryAppended` it takes a brief read lock, clones
//! the session history into an `Arc<[ChatEntry]>` (the allocation the old
//! kernel `HistorySnapshotActor` used to fan out to N kameo workers —
//! dissolved into this actor), then evaluates each strategy outside any
//! lock and publishes every non-empty mutation batch as
//! `SubmitHistoryMutations`.
//!
//! Per-strategy enablement is a construction-time decision: the wiring
//! helper builds the worker list from user preferences, so a disabled
//! strategy never reaches this actor.

use std::collections::HashSet;
use std::sync::Arc;

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::HistoryMutation;
use jinn_core_types::{ChatEntry, SessionId};
use jinn_domain::common::actor_deps::BusPublish;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::common::state::State;
use jinn_session_history_msg::{HistoryAppended, SubmitHistoryMutations};

use crate::worker::HistoryWorker;

/// The prune actor's static path.
pub const PRUNE_PATH: &str = "context-curation-prune";

/// Evaluates the enabled auto-prune strategies on history snapshots.
pub struct PruneActor {
    /// Shared application state (snapshot reads).
    state: State,
    /// Application-wide runtime services (bus publish).
    services: Services,
    workers: Vec<Box<dyn HistoryWorker>>,
    /// Sessions currently being evaluated — skips a re-trigger while one
    /// snapshot's strategies are still running.
    in_flight: HashSet<SessionId>,
}

impl ServiceActor for PruneActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects state and workers via
        // `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("PruneActor is spawned via start_with"),
        )
    }
}

impl BusPublish for PruneActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl PruneActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the context-curation topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(
        system: &ActorSystem,
        state: State,
        services: Services,
        workers: Vec<Box<dyn HistoryWorker>>,
    ) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(PRUNE_PATH))
            .start_with({
                move || {
                    let state = state.clone();
                    let services = services.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            services,
                            workers,
                            in_flight: HashSet::new(),
                        })
                    })
                }
            })
            .handles::<HistoryAppended>()
            .start()
    }

    /// Handle `HistoryAppended` — snapshot the history, run the
    /// strategies, publish their mutation batches.
    pub async fn handle_history_appended(&mut self, payload: &HistoryAppended) {
        if self.workers.is_empty() {
            return;
        }
        // Skip if this session's strategies are still evaluating.
        if self.in_flight.contains(&payload.session_id) {
            tracing::debug!(
                session_id = %payload.session_id,
                "prune evaluation already in flight, skipping"
            );
            return;
        }

        // Brief read lock → clone history into Arc → drop lock.
        let history: Arc<[ChatEntry]> = {
            let state = self.state.read();
            let Some(session) = state.session.get(&payload.session_id) else {
                tracing::debug!(
                    session_id = %payload.session_id,
                    "session not found, skipping prune evaluation"
                );
                return;
            };
            Arc::from(session.history().to_vec())
        };

        self.in_flight.insert(payload.session_id.clone());
        for worker in &self.workers {
            tracing::debug!(
                worker = worker.name(),
                session_id = %payload.session_id,
                entries = history.len(),
                "evaluating history snapshot"
            );
            let mutations = worker
                .evaluate(&payload.session_id, Arc::clone(&history))
                .await;
            if mutations.is_empty() {
                continue;
            }
            tracing::debug!(
                worker = worker.name(),
                session_id = %payload.session_id,
                count = mutations.len(),
                "prune strategy produced mutations"
            );
            self.publish_mutations(&payload.session_id, mutations).await;
        }
        self.in_flight.remove(&payload.session_id);
    }

    async fn publish_mutations(&self, session_id: &SessionId, mutations: Vec<HistoryMutation>) {
        self.publish(SubmitHistoryMutations {
            session_id: session_id.clone(),
            mutations,
        })
        .await;
    }
}

impl MsgHandler<HistoryAppended> for PruneActor {
    async fn handle(&mut self, msg: HistoryAppended, _ctx: &mut MsgCtx<'_>) {
        self.handle_history_appended(&msg).await;
    }
}

#[cfg(test)]
#[path = "prune_actor_tests.rs"]
mod tests;
