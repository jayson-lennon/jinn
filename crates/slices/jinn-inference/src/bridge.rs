//! The inference slice's bridge drain: forward routes and topic.
//!
//! Topics are named here (the inference slice consumes them); the relays
//! themselves are kernel bridge actors spawned by this drain — the
//! slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

use crate::inference_topic;

/// Drains the inference slice's forward routes into bridge relays:
/// `SendToLlmProvider` and `CancelStream` dispatch commands, plus the
/// actor's own `StreamCompleted` echo (all kameo bus → `jinn.inference`).
///
/// Slice-local drain, called by composition after activation. Relays
/// register on the bus in their own `on_start`, so drain ordering
/// relative to publishers is free.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_inference_msg::SendToLlmProvider>(services).await;
    forward::<jinn_inference_msg::CancelStream>(services).await;
    forward::<jinn_inference_msg::StreamCompleted>(services).await;
}

/// Spawns the forward relay for `M` on the inference topic.
async fn forward<M>(services: &Services)
where
    M: jinn_slices::BusMessage + jinn_slices::host::ForwardMessage,
{
    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "inference",
            topic: inference_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}
