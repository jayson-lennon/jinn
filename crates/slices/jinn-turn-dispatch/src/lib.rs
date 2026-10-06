//! The turn-dispatch slice — the queue actor that owns turn draining and
//! dispatch.
//!
//! Hosts the trouper [`ServiceActor`] queue actor (the sole consumer of
//! the session turn dispatch queue). It folds the kernel
//! `SessionPhaseChanged` event (an `→ Idle` transition pops the queue)
//! and the slice-owned [`DispatchTurn`] command (idle-direct sends,
//! resume tails, stall-retry re-dispatches — each already prepared and
//! eligibility-checked by the session actor) into one dispatch body:
//! drain steering, normalize loop layout, begin the turn's phase writes,
//! ask the context-assembly service, and publish `SendToLlmProvider` on
//! the kernel topic.
//!
//! Kernel dependency (see Cargo.toml): the queue actor uses shared
//! [`jinn_kernel::common::state::State`] and consumes session vocabulary.

pub mod dispatch;
pub mod queue_actor;

use jinn_slices::SliceHost;

pub use jinn_turn_dispatch_msg::DispatchTurn;

/// Activates the slice: spawns the queue actor on trouper (its
/// `.subscribe` declarations are the readiness point). The kernel
/// `SessionPhaseChanged` event and the slice-owned [`DispatchTurn`]
/// command arrive by schema broadcast.
///
/// # Panics
///
/// Panics are none today — spawn owns the declaration; a failed
/// declaration would abort launch rather than leave the turn queue
/// silently unconsumed.
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    state: jinn_kernel::common::state::State,
    services: jinn_kernel::Services,
) {
    let _queue_path = queue_actor::QueueActor::spawn(host.system(), state, services);
}
