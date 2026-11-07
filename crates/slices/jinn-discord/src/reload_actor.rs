//! The gateway reload actor — folds `JinnTomlReloaded` and
//! `SessionPhaseChanged` into the supervisor.
//!
//! Always spawned, even for a disabled activation: that is what makes a
//! live `[discord] enabled = true` edit connect without a restart (the
//! dormant supervisor spawns the gateway through the hook composition
//! attached) and what parks a disable until the sessions go idle. One
//! declarant per schema is preserved — the bridge subscriber never
//! handles these events; this actor owns both.

use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::prelude::ActorPath;
use trouper::registry::RegistryError;

use crate::gateway_supervisor::GatewaySupervisor;

/// The reload actor's static trouper path.
pub const RELOAD_PATH: &str = "discord-reload-actor";

/// The gateway reload actor. Owns the supervisor outright — every fold
/// runs on the actor's single thread, so the supervisor needs no lock
/// of its own.
pub struct DiscordReloadActor {
    supervisor: SharedSupervisor,
}

impl ServiceActor for DiscordReloadActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("DiscordReloadActor is spawned via init_with"),
        )
    }
}

impl DiscordReloadActor {
    /// Builds the actor over a shared supervisor.
    ///
    /// The supervisor is minted once at activation, wrapped in
    /// `Arc<tokio::sync::Mutex<_>>`, and handed to both this spawn and
    /// composition's `attach_spawner` site: the actor folds events
    /// into it, composition attaches the gateway spawn hook to it.
    /// Every fold runs under the mutex, so the supervisor itself
    /// needs no interior locking.
    #[must_use]
    pub fn new(supervisor: SharedSupervisor) -> Self {
        Self { supervisor }
    }
}

/// The shared supervisor handle: minted at activation, held by both
/// the reload actor and the composition-side spawn hook.
pub type SharedSupervisor = std::sync::Arc<tokio::sync::Mutex<GatewaySupervisor>>;

impl DiscordReloadActor {
    /// Spawns the actor at its static path, subscribing to the reload
    /// event and the phase event — the two folds the live `[discord]`
    /// application needs.
    pub fn spawn(system: &trouper::system::ActorSystem, supervisor: SharedSupervisor) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(RELOAD_PATH))
            .init_with({
                let supervisor = std::sync::Arc::clone(&supervisor);
                move || Box::pin(async move { Ok(Self { supervisor }) })
            })
            .handles::<jinn_boot_msg::JinnTomlReloaded>()
            .handles::<jinn_session_msg::SessionPhaseChanged>()
            .start()
    }
}

impl MsgHandler<jinn_boot_msg::JinnTomlReloaded> for DiscordReloadActor {
    async fn handle(&mut self, _msg: &jinn_boot_msg::JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        // Fresh resolution off the live layer — the watcher swapped the
        // snapshot before publishing (the W12 invariant), so this reads
        // post-reload values. A malformed section cannot arrive here:
        // the validate-gated reload refused the file wholesale.
        let fresh = {
            let supervisor = self.supervisor.lock().await;
            match supervisor.read_section() {
                Ok(config) => config,
                Err(report) => {
                    tracing::warn!(report = ?report, "post-reload [discord] read failed; keeping applied");
                    return;
                }
            }
        };
        self.supervisor.lock().await.handle_reload(&fresh).await;
    }
}

impl MsgHandler<jinn_session_msg::SessionPhaseChanged> for DiscordReloadActor {
    async fn handle(&mut self, msg: &jinn_session_msg::SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.supervisor
            .lock()
            .await
            .handle_phase_changed(msg.new_phase)
            .await;
    }
}
