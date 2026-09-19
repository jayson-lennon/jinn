//! The compaction actor — runs the compaction worker on manual triggers.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's
//! `jinn.context-curation` topic. On `TriggerCompaction` (published by
//! the `/compact` and `/compact-all` intent paths) it runs
//! [`CompactionWorker::evaluate_for_session`], publishes the resulting
//! mutations as `SubmitHistoryMutations`, and pushes feedback system
//! entries for queued/skipped/failed outcomes.
//!
//! This is a direct port of the kernel `CompactionTriggerActor` (kameo)
//! to the trouper runtime; the handle body is unchanged.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_context_curation_msg::TriggerCompaction;
use jinn_core_types::ChatEntry;
use jinn_domain::common::actor_deps::BusPublish;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::common::state::State;
use jinn_session_history_msg::{PushChatEntry, SubmitHistoryMutations};

use crate::compaction_worker::{CompactionTrigger, CompactionWorker};

/// The compaction actor's static path.
pub const COMPACTION_PATH: &str = "context-curation-compaction";

/// Dependencies for spawning a [`CompactionActor`].
#[derive(Clone)]
pub struct CompactionActorDeps {
    /// Application-wide runtime services (LLM factory, bus, config store).
    pub services: Services,
    /// Shared application state (session reads under brief locks).
    pub state: State,
    /// Tokio runtime handle for `spawn_blocking` serialization.
    pub handle: tokio::runtime::Handle,
    /// The compaction system prompt loaded at startup.
    pub compaction_prompt: String,
}

/// Runs the compaction worker on `TriggerCompaction` commands.
pub struct CompactionActor {
    services: Services,
    state: State,
    worker: CompactionWorker,
}

impl ServiceActor for CompactionActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects deps via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("CompactionActor is spawned via start_with"),
        )
    }
}

impl BusPublish for CompactionActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl CompactionActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the context-curation topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, deps: CompactionActorDeps) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(COMPACTION_PATH))
            .start_with({
                move || {
                    let services = deps.services.clone();
                    let state = deps.state.clone();
                    let handle = deps.handle.clone();
                    let compaction_prompt = deps.compaction_prompt.clone();
                    Box::pin(async move {
                        let worker = CompactionWorker::new(
                            services.clone(),
                            handle,
                            state.clone(),
                            jinn_domain::common::tcaps::mint::mint_session_cap(),
                            compaction_prompt,
                        );
                        Ok(Self {
                            services,
                            state,
                            worker,
                        })
                    })
                }
            })
            .handles::<TriggerCompaction>()
            .start()
    }

    /// Handle `TriggerCompaction` — run the worker and submit mutations
    /// (plus feedback entries).
    pub async fn handle_trigger_compaction(&self, payload: &TriggerCompaction) {
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

        let trigger = CompactionTrigger {
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
                    .state
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

impl MsgHandler<TriggerCompaction> for CompactionActor {
    async fn handle(&mut self, msg: TriggerCompaction, _ctx: &mut MsgCtx<'_>) {
        self.handle_trigger_compaction(&msg).await;
    }
}
