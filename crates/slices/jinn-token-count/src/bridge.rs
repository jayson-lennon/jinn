//! The token-count slice's bridge drain: forward routes and topic.
//!
//! Topics are named here (the token-count slice consumes them); the
//! relays themselves are kernel bridge actors spawned by this drain —
//! the slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::BusMessage;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

use crate::token_count_topic;

/// Drains the token-count slice's forward routes into bridge relays: the
/// kernel session events the slice's actors fold (kameo bus →
/// `jinn.token-count`).
///
/// Slice-local drain, called by composition after activation. Relays
/// register on the bus in their own `on_start`, so drain ordering
/// relative to publishers is free.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_session_history_msg::HistoryAppended>(services).await;
    forward::<jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted>(
        services,
    )
    .await;
    forward::<jinn_domain::feat::session::protocol::session_closed::SessionClosed>(services).await;
}

/// Spawns the forward relay for `M` on the token-count topic.
async fn forward<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ForwardMessage,
{
    // Erased publishes (bridge closures) route natively on trouper: the
    // schema→topic rule mirrors the relay below.
    services.bus.route_topic::<M>(token_count_topic());

    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "token-count",
            topic: token_count_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}
