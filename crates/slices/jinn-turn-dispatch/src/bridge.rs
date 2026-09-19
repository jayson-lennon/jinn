//! The turn-dispatch slice's bridge drain: forward routes and topic.
//!
//! Topics are named here (the turn-dispatch slice consumes them); the
//! relays themselves are kernel bridge actors spawned by this drain —
//! the slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::BusMessage;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

use crate::turn_dispatch_topic;

/// Drains the turn-dispatch slice's forward routes into bridge relays:
/// the kernel `SessionPhaseChanged` event and the slice-owned
/// `DispatchTurn` command (kameo bus → `jinn.turn-dispatch`).
///
/// Slice-local drain, called by composition after activation. Relays
/// register on the bus in their own `on_start`, so drain ordering
/// relative to publishers is free.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged>(
        services,
    )
    .await;
    forward::<jinn_turn_dispatch_msg::DispatchTurn>(services).await;
}

/// Spawns the forward relay for `M` on the turn-dispatch topic.
async fn forward<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ForwardMessage,
{
    // Erased publishes (bridge closures) route natively on trouper: the
    // schema→topic rule mirrors the relay below.
    services.bus.route_topic::<M>(turn_dispatch_topic());

    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "turn-dispatch",
            topic: turn_dispatch_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}
