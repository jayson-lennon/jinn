//! Generic `HistoryWorkerActor` - wraps any [`HistoryWorker`] as a kameo actor.
//!
//! Subscribes to [`HistorySnapshotReady`] events. On each event:
//! 2. Receives shared `Arc<[ChatEntry]>` (O(1) clone)
//! 3. Spawns a tokio task for `worker.evaluate(history_snapshot).await`
//! 4. If mutations are produced, publishes [`SubmitHistoryMutations`]
use std::collections::HashSet;
use std::sync::Arc;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::feat::session::protocol::history_snapshot_ready::HistorySnapshotReady;
use crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations;
use crate::protocol::ChatEntry;
use crate::protocol::SessionId;
use kameo::prelude::{Actor, ActorRef, Context, Message};

/// A generic actor that wraps a [`HistoryWorker`] implementation.
///
/// Each worker instance runs as its own actor with its own tokio task.
/// Workers receive [`HistorySnapshotReady`] events, evaluate their heuristic,
/// and submit mutations by publishing to the message bus.
pub struct HistoryWorkerActor<H: HistoryWorker> {
    deps: ActorDeps,
    worker: H,
    /// Sessions currently being evaluated - prevents concurrent compaction.
    in_flight: HashSet<SessionId>,
}

#[derive(Clone)]
pub struct HistoryWorkerActorDeps<H: HistoryWorker + Clone> {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// The worker heuristic implementation.
    pub worker: H,
}

impl<H: HistoryWorker + Clone + Send + 'static> Actor for HistoryWorkerActor<H> {
    type Args = HistoryWorkerActorDeps<H>;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<HistorySnapshotReady>())
            .await;

        Ok(Self {
            deps: args.deps,
            worker: args.worker,
            in_flight: HashSet::new(),
        })
    }
}

impl<H: HistoryWorker + Clone + Send + 'static> Message<HistorySnapshotReady>
    for HistoryWorkerActor<H>
{
    type Reply = ();

    async fn handle(&mut self, msg: HistorySnapshotReady, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_snapshot_ready(&msg).await;
    }
}

impl<H: HistoryWorker + Clone> BusPublish for HistoryWorkerActor<H> {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl<H: HistoryWorker + Clone> HistoryWorkerActor<H> {
    pub(crate) async fn handle_snapshot_ready(&mut self, event: &HistorySnapshotReady) {
        tracing::debug!(
            worker = self.worker.name(),
            session_id = %event.session_id,
            "HistorySnapshotReady received"
        );

        // Skip if already evaluating this session.
        if self.in_flight.contains(&event.session_id) {
            tracing::info!(
                session_id = %event.session_id,
                "compaction already in flight, skipping"
            );
            return;
        }

        // O(1) Arc clone — shared with all other workers.
        let history_snapshot: Arc<[ChatEntry]> = event.history.clone();

        // Mark session as in-flight.
        self.in_flight.insert(event.session_id.clone());

        // Run heuristic evaluation outside any lock (async).
        let mutations = self
            .worker
            .evaluate(&event.session_id, history_snapshot)
            .await;

        // Clear in-flight marker.
        self.in_flight.remove(&event.session_id);

        if mutations.is_empty() {
            return;
        }

        tracing::debug!(
            worker = self.worker.name(),
            session_id = %event.session_id,
            count = mutations.len(),
            "history worker produced mutations"
        );

        // Submit mutations via bus.
        self.publish(SubmitHistoryMutations {
            session_id: event.session_id.clone(),
            mutations,
        })
        .await;
    }
}
