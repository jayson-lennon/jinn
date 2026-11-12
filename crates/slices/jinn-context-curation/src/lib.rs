//! The context-curation slice — what the LLM sees each turn.
//!
//! Owns the auto-prune strategies (exclude stale/redundant entries).
//! They produce `HistoryMutation` batches and publish them as
//! `SubmitHistoryMutations`; the kernel session actor's accumulation
//! gate stays the sole applier.
//!
//! One trouper [`ServiceActor`], fed by the `jinn.context-curation`
//! forward route: the prune actor (evaluates the enabled strategies on
//! each `HistoryAppended`, snapshotting the history internally).
//!
//! Kernel dependency (see Cargo.toml): the prune workers read through
//! `State` and consume the kernel token estimator, granted at slice
//! activation.

pub mod prune_actor;
pub mod strategies;
pub mod worker;

pub use strategies::min_age;

use jinn_slices::SliceHost;

/// Activates the slice: spawns the prune actor on trouper (its
/// `.subscribe` declarations are the readiness point).
///
/// # Panics
///
/// Panics if any worker's trouper spawn or topic subscription fails —
/// it is a wiring bug that must abort composition.
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    prune_workers: Vec<Box<dyn worker::HistoryWorker>>,
    state: jinn_kernel::common::state::State,
    services: jinn_kernel::Services,
) {
    let prune_path = prune_actor::PruneActor::spawn(host.system(), state, services, prune_workers);
    drop(prune_path);
}
