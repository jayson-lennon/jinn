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
//! the kameo bus.
//!
//! Kernel dependency (see Cargo.toml): the queue actor writes through
//! tcaps (State + SessionCap) and consumes session vocabulary, granted at
//! activation.

pub mod bridge;
pub mod queue_actor;

use trouper::schema::Schema;

use jinn_slices::SliceHost;

pub use jinn_turn_dispatch_msg::DispatchTurn;
pub use jinn_turn_dispatch_msg::turn_dispatch_topic;

/// Activates the slice: spawns the queue actor on trouper and subscribes
/// it to the [`turn_dispatch_topic`] (the readiness point), then stages
/// the slice's two forward routes — the kernel `SessionPhaseChanged`
/// event and the slice-owned [`DispatchTurn`] command.
///
/// Composition drains the staged routes after activation (see
/// [`bridge::drain_routes`]).
///
/// # Panics
///
/// Panics if the just-spawned queue actor cannot be subscribed to the
/// turn-dispatch topic — a broken subscription must abort launch rather
/// than leave the turn queue silently unconsumed.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    state: jinn_domain::common::state::State,
    services: jinn_domain::Services,
) {
    let queue_path = queue_actor::QueueActor::spawn(host.system(), state, services);
    host.subscribe_service(&queue_path, &turn_dispatch_topic())
        .expect("queue actor subscribes to the turn-dispatch topic");

    host.forward::<jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged, _>(
        turn_dispatch_topic(),
        || {
            jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged::schema_def()
        },
    );
    host.forward::<DispatchTurn, _>(turn_dispatch_topic(), || {
        <DispatchTurn as Schema>::schema_def()
    });
}
