//! The context-assembly slice's bridge drain: forward routes and topic.
//!
//! Topics are named here (the slice consumes them); the relays
//! themselves are kernel bridge actors spawned by this drain — the
//! slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::BusMessage;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

/// The context-assembly slice's crossing topic (`jinn.context-assembly`):
/// kernel context-affecting events forward onto it for the size actor.
#[must_use]
pub fn context_assembly_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.context-assembly")
}

/// Drains the context-assembly slice's forward routes into bridge
/// relays: the kernel events the size actor folds (kameo bus →
/// `jinn.context-assembly`).
///
/// Slice-local drain, called by composition after activation. Relays
/// register on the bus in their own `on_start`, so drain ordering
/// relative to publishers is free.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_session_history_msg::HistoryAppended>(services).await;
    forward::<jinn_domain::feat::context::protocol::event::ContextOverrideChanged>(services).await;
    forward::<jinn_domain::protocol::system::ActiveSessionChanged>(services).await;
    forward::<jinn_session_history_msg::ChatEntryPinChanged>(services).await;
    forward::<jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted>(
        services,
    )
    .await;
}

/// Spawns the forward relay for `M` on the context-assembly topic.
async fn forward<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ForwardMessage,
{
    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "context-assembly",
            topic: context_assembly_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}
