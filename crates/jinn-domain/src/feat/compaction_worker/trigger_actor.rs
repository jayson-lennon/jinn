//! Compaction trigger actor - handles `/compact` and `/compact-all` commands.
//!
//! Receives `TriggerCompaction` commands, runs the compaction worker,
//! and submits mutations via `SubmitHistoryMutations`.
//! Pushes system messages for user feedback (queued, skipped, failed).

use crate::common::actor_deps::{ActorDeps, BusPublish};
use jinn_session_history_msg::PushChatEntry;
use crate::feat::compaction_worker::worker::CompactionWorker;
use crate::protocol::ChatEntry;
use crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations;
use crate::feat::session::protocol::trigger_compaction::TriggerCompaction;
use kameo::prelude::{Actor, ActorRef, Context, Message};

/// Actor that handles manual compaction triggers.
///
/// Subscribes to `TriggerCompaction` commands (from `/compact` and `/compact-all`).
/// Runs the compaction worker and submits mutations.
pub struct CompactionTriggerActor {
    deps: ActorDeps,
    worker: CompactionWorker,
}

/// Dependencies for spawning a [`CompactionTriggerActor`].
#[derive(Clone)]
pub struct CompactionTriggerActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// The compaction worker.
    pub worker: CompactionWorker,
}

impl Actor for CompactionTriggerActor {
    type Args = CompactionTriggerActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<TriggerCompaction>())
            .await;

        Ok(Self {
            deps: args.deps,
            worker: args.worker,
        })
    }
}

impl Message<TriggerCompaction> for CompactionTriggerActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: TriggerCompaction,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handle_trigger_compaction(&msg).await;
    }
}

impl BusPublish for CompactionTriggerActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

impl CompactionTriggerActor {
    async fn handle_trigger_compaction(&self, payload: &TriggerCompaction) {
        tracing::info!(
            session_id = %payload.session_id,
            compact_all = payload.compact_all,
            "manual compaction triggered"
        );

        // Always push immediate "queued" feedback.
        self.publish(PushChatEntry {
            session_id: payload.session_id.clone(),
            entry: ChatEntry::system("⏳ Compacting context..."),
        })
        .await;

        let trigger = crate::feat::compaction_worker::worker::CompactionTrigger {
            session_id: payload.session_id.clone(),
            compact_all: payload.compact_all,
        };

        match self.worker.evaluate_for_session(&trigger).await {
            Ok(mutations) if !mutations.is_empty() => {
                tracing::info!(
                    session_id = %payload.session_id,
                    count = mutations.len(),
                    "compaction trigger produced mutations"
                );

                self.publish(SubmitHistoryMutations {
                    session_id: payload.session_id.clone(),
                    mutations,
                })
                .await;
            }
            Ok(_) => {
                // Empty mutations - nothing to compact.
                let reserve = self
                    .worker
                    .state()
                    .read()
                    .frontend
                    .preferences
                    .compaction
                    .reserve_tokens;
                let msg = format!(
                    "⚠ Compaction skipped: recent conversation fits within reserve ({reserve} tokens)."
                );
                tracing::info!(
                    session_id = %payload.session_id,
                    "compaction produced no mutations (nothing to compact)"
                );
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: ChatEntry::system(&msg),
                })
                .await;
            }
            Err(e) => {
                tracing::error!(
                    session_id = %payload.session_id,
                    error = %e,
                    "compaction failed"
                );
                let msg = format!("⚠ Compaction failed: {e}");
                self.publish(PushChatEntry {
                    session_id: payload.session_id.clone(),
                    entry: ChatEntry::system(&msg),
                })
                .await;
            }
        }
    }
}
