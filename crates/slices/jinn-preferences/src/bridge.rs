//! The preferences slice's bridge drain: forward routes and topic.
//!
//! Topics are named here (the slice consumes them); the relays
//! themselves are kernel bridge actors spawned by this drain — the
//! slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::BusMessage;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

/// The preferences slice's crossing topic (`jinn.preferences`): the
/// `UpdatePreferences`/`UpdateAppState` persistence commands forward
/// onto it for the two actors.
#[must_use]
pub fn preferences_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.preferences")
}

/// Drains the preferences slice's forward routes into bridge relays:
/// the persistence commands forward onto the preferences topic
/// (kameo bus → `jinn.preferences`).
///
/// Slice-local drain, called by composition after activation. Relays
/// register on the bus in their own `on_start`, so drain ordering
/// relative to publishers is free — the actors' own subscribes (the
/// readiness point) happen in `activate`, before any publish.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_preferences_config::protocol::command::UpdatePreferences>(services).await;
    forward::<jinn_preferences_config::protocol::app_state_command::UpdateAppState>(services).await;
}

/// Spawns the forward relay for `M` on the preferences topic.
async fn forward<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ForwardMessage,
{
    // Erased publishes (bridge closures) route natively on trouper: the
    // schema→topic rule mirrors the relay below.
    services.bus.route_topic::<M>(preferences_topic());

    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "preferences",
            topic: preferences_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}
