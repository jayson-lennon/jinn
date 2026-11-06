//! Persona rescan actor — re-scans the persona directory on
//! `ResourcesChanged` and republishes `PersonasLoaded`.
//!
//! The persona slice scans once at activation and composition publishes
//! the result after every subscriber is up; this actor repeats that
//! scan when the config-watch slice reports the persona directory
//! changed. It owns no state and writes no cell: the session actor's
//! existing `PersonasLoaded` handler replaces the personas cell and
//! rewrites every live session whose stored persona stopped resolving,
//! so republishing the fresh scan is the whole job.

use marked_path::Absolute;
use marked_path::MarkedPathBuf;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::builder::spawn_service_builder;
use trouper::context::MsgCtx;
use trouper::prelude::ActorPath;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_kernel::common::services::Services;
use jinn_persona_msg::PersonasLoaded;

/// The persona rescan actor's static trouper path.
pub const PERSONA_RESCAN_PATH: &str = "persona-rescan-actor";

/// The persona rescan actor.
pub struct PersonaRescanActor {
    /// Runtime services — the personas dir comes from the shared paths.
    services: Services,
}

impl ServiceActor for PersonaRescanActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects services via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("PersonaRescanActor is spawned via init_with"),
        )
    }
}

impl PersonaRescanActor {
    /// Spawns the actor at its static path, subscribing to
    /// `ResourcesChanged` and emitting `PersonasLoaded`.
    pub fn spawn(system: &ActorSystem, services: Services) -> ActorPath {
        spawn_service_builder::<Self>(system)
            .at(ActorPath::new(PERSONA_RESCAN_PATH))
            .init_with({
                move || {
                    let services = services.clone();
                    Box::pin(async move { Ok(Self { services }) })
                }
            })
            // The config-watch slice publishes the past-tense fact; this
            // actor is its persona listener and the republish is its
            // whole job.
            .handles::<jinn_boot_msg::ResourcesChanged>()
            .emits::<PersonasLoaded>()
            .start()
    }

    /// Re-scans the persona directory and republishes the catalog.
    ///
    /// Scan semantics are the activation ones (`.md` files only, one
    /// unparseable file noted and skipped), so a bad file never drops
    /// the batch. The session actor's `PersonasLoaded` handler owns the
    /// cell update and the session rewrites.
    fn rescan_and_publish(&self) -> PersonasLoaded {
        let personas_dir: MarkedPathBuf<Absolute> = self.services.paths.personas_dir();
        let entries = crate::scan(&personas_dir);
        PersonasLoaded {
            personas: entries,
            error: None,
        }
    }
}

impl MsgHandler<jinn_boot_msg::ResourcesChanged> for PersonaRescanActor {
    async fn handle(&mut self, _msg: &jinn_boot_msg::ResourcesChanged, ctx: &mut MsgCtx<'_>) {
        // The event carries no payload (the watcher cannot know which
        // slice cares about which directory), so this rescans on every
        // resource change. The rescan is cheap and idempotent when the
        // personas dir is not the one that changed, and the session
        // actor's `PersonasLoaded` handler dedupes by replacing the
        // catalog cell wholesale.
        ctx.publish(self.rescan_and_publish());
    }
}
