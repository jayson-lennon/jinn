//! The session-init slice's bridge drain: forward + reverse routes.
//!
//! Topics are named here (this slice consumes them); the relays
//! themselves are kernel bridge actors spawned by this drain — the
//! slice-local drain convention (the slice owns the message types it
//! names, the kernel must not depend on slice crates).

use jinn_domain::Services;
use jinn_slices::BusMessage;
use jinn_slices::host::Direction;
use jinn_slices::host::RouteEntry;

use crate::session_init_topic;

/// Drains the session-init slice's staged routes into bridge relays:
/// the 8 forward triggers (kameo bus → `jinn.session-init`) and the 3
/// reverse results (`SkillsLoaded`, `PromptTemplatesLoaded`,
/// `ContextFilesLoaded` — trouper schema topics → kameo bus).
///
/// The reverse relays are the bridge's first production trouper→kameo
/// routes; kernel consumers subscribe to the same Rust types and are
/// none the wiser.
pub async fn drain_routes(services: &Services) {
    forward::<jinn_domain::init::env_init_actor::EnvironmentLoaded>(services).await;
    forward::<jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated>(services)
        .await;
    forward::<jinn_session_msg::SessionSetupCompleted>(services).await;
    forward::<jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted>(
        services,
    )
    .await;
    forward::<jinn_domain::feat::session_lifecycle::protocol::event::SessionCwdChanged>(services)
        .await;
    forward::<jinn_domain::feat::skills::ScanSkills>(services).await;
    forward::<jinn_domain::feat::provider::protocol::command::RescanPromptTemplates>(services)
        .await;
    forward::<jinn_domain::feat::context::protocol::command::ScanContextFiles>(services).await;

    reverse::<jinn_domain::feat::skills::SkillsLoaded>(services).await;
    reverse::<jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded>(services).await;
    reverse::<jinn_domain::feat::context::protocol::event::ContextFilesLoaded>(services).await;
}

/// Spawns the forward relay for `M` on the session-init topic.
async fn forward<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ForwardMessage,
{
    jinn_domain::common::trouper_bridge::spawn_one::<M>(
        services,
        &RouteEntry {
            schema_id: <M as trouper::schema::Schema>::schema_id(),
            name: "session-init",
            topic: session_init_topic(),
            direction: Direction::Forward,
        },
    )
    .await;
}

/// Spawns the reverse relay for `M` on `M`'s schema-named topic.
async fn reverse<M>(services: &Services)
where
    M: BusMessage + jinn_slices::host::ReverseMessage,
{
    jinn_domain::common::trouper_bridge::spawn_reverse_relay::<M>(
        &services.trouper_system,
        services.bus.clone(),
    );
}
