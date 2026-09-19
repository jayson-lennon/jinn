//! The context-curation slice — what the LLM sees each turn.
//!
//! Owns the curation workers: the auto-prune strategies (exclude
//! stale/redundant entries) and the compaction worker (summarize old
//! history into one checkpoint entry). Both produce `HistoryMutation`
//! batches and publish them as `SubmitHistoryMutations`; the kernel
//! session actor's accumulation gate stays the sole applier.
//!
//! Two trouper [`ServiceActor`]s, fed by the `jinn.context-curation`
//! forward route: the prune actor (evaluates the enabled strategies on
//! each `HistoryAppended`, snapshotting the history internally — the
//! old `HistorySnapshotActor`/`HistorySnapshotReady` fan-out dissolved
//! into it) and the compaction actor (runs `CompactionWorker` on
//! `TriggerCompaction`).
//!
//! Kernel dependency (see Cargo.toml): the compaction worker reads
//! through tcaps (State + SessionCap) and consumes the kernel
//! token_estimator, granted at slice activation.

pub mod compaction_actor;
pub mod compaction_algorithm;
pub mod compaction_serializer;
pub mod compaction_worker;
pub mod prune_actor;
pub mod strategies;
pub mod worker;

pub use strategies::min_age;

use trouper::schema::Schema;

use jinn_context_curation_msg::curation_topic;
use jinn_slices::SliceHost;

/// Activates the slice: spawns the prune + compaction actors on trouper
/// and subscribes them to the [`curation_topic`] (the readiness point),
/// and stages the slice's forward routes.
///
/// Composition drains the staged routes after activation (the wiring
/// helper's `finalize` + drain pattern).
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    prune_workers: Vec<Box<dyn worker::HistoryWorker>>,
    compaction_deps: compaction_actor::CompactionActorDeps,
) {
    let prune_path = prune_actor::PruneActor::spawn(
        host.system(),
        compaction_deps.state.clone(),
        compaction_deps.services.clone(),
        prune_workers,
    );
    host.subscribe_service(&prune_path, &curation_topic())
        .expect("prune actor subscribes to the context-curation topic");
    let compaction_path = compaction_actor::CompactionActor::spawn(host.system(), compaction_deps);
    host.subscribe_service(&compaction_path, &curation_topic())
        .expect("compaction actor subscribes to the context-curation topic");

    host.forward::<jinn_session_history_msg::HistoryAppended, _>(curation_topic(), || {
        jinn_session_history_msg::HistoryAppended::schema_def()
    });
    host.forward::<jinn_context_curation_msg::TriggerCompaction, _>(curation_topic(), || {
        jinn_context_curation_msg::TriggerCompaction::schema_def()
    });
}
