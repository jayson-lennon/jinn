//! The gateway supervisor — live `[discord]` application without a
//! restart.
//!
//! Activation resolves the section once and parks the channel halves;
//! the gateway task itself spawns later, from composition, through the
//! hook this module hands back. The reload actor owns the diffing:
//!
//! - **Enabled → disabled while a session is busy** — a live
//!   conversation must not be cut off by a config edit, so the change
//!   parks on the supervisor and lands at the all-sessions-idle
//!   boundary (`SessionPhaseChanged`), which the gateway's drain loop
//!   already treats as its quiet signal.
//! - **Enabled → disabled while idle** — applied immediately: the
//!   gateway is told to run down, the flag flips, and the next `gdc`
//!   sees the disabled message.
//! - **Disabled → enabled** — nothing to interrupt; the supervisor
//!   spawns the gateway right away.
//! - **Content-only changes while running** — the swappable config
//!   handle ([`GatewayConfigHandle`]) applies them live: the authorize
//!   list, forum channel, and token-resolution order are read through
//!   it per request.

use std::sync::Arc;

use jinn_preferences_config::schemas::discord::DiscordConfig;
use tokio::sync::Notify;

use crate::status_actor::ConnectionState;
use crate::{DiscordGatewayChannels, discord_connection_slot};
use jinn_kernel::common::state::State;
use jinn_slices::PhaseView;
use jinn_slices::Slices;

/// The swappable `[discord]` config the gateway reads through.
///
/// A snapshot in an `Arc<RwLock<..>>`: readers clone the current value
/// under a short guard so no lock spans an await or a Discord call,
/// and a reload swaps the whole value — the same shape the shells
/// service uses. One writer: the reload actor.
#[derive(Debug, Clone)]
pub struct GatewayConfigHandle {
    inner: Arc<std::sync::RwLock<DiscordConfig>>,
}

impl GatewayConfigHandle {
    /// Wraps the activation-time resolution.
    #[must_use]
    pub fn new(config: DiscordConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(config)),
        }
    }

    /// Reads a snapshot of the current config.
    #[must_use]
    pub fn read(&self) -> DiscordConfig {
        self.inner
            .read()
            .expect("discord config lock is never poisoned")
            .clone()
    }

    /// Swaps the config whole.
    pub fn replace(&self, config: DiscordConfig) {
        *self
            .inner
            .write()
            .expect("discord config lock is never poisoned") = config;
    }

    /// A shared snapshot bound to the current value.
    ///
    /// The gateway task holds this for its lifetime: content reloads
    /// swap the slot, and every reader of *this* arc sees the swap; a
    /// gateway generation spawned later snapshots into a fresh arc.
    #[must_use]
    pub fn into_arc(self) -> Arc<DiscordConfig> {
        Arc::new(self.read())
    }
}

/// What the supervisor believes about the running gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayState {
    /// Nothing spawned; the slice activated disabled.
    Dormant,
    /// A gateway task is live under the applied config.
    Running,
    /// A disable landed while busy; the shutdown fires at the next
    /// all-sessions-idle boundary.
    PendingDisable,
}

/// The parked gateway machinery plus the live-reload state.
///
/// Minted at activation; composition attaches the spawn hook after the
/// core exists (the gateway needs the bridge and the session pool),
/// and the reload actor drives the diffs through [`Self::handle_reload`].
/// The spawn hook the supervisor calls to start one gateway
/// generation: the config handle to snapshot and the stop signal
/// the disable path notifies.
pub type GatewaySpawnHook =
    dyn Fn(GatewayConfigHandle, std::sync::Arc<tokio::sync::Notify>) + Send + Sync;

/// The supervisor.
pub struct GatewaySupervisor {
    /// The channel halves an enable spawns the gateway with. Cloned
    /// per spawn; the bridge actor keeps the partner senders alive for
    /// the process lifetime.
    parked: DiscordGatewayChannels,
    /// The swappable config the gateway reads through.
    config_handle: GatewayConfigHandle,
    /// What the supervisor believes is running.
    gateway: GatewayState,
    /// Fired once to run the gateway down at the idle boundary.
    shutdown: Arc<Notify>,
    /// The spawn hook composition attaches: takes the handle + shutdown
    /// and spawns the gateway task. `None` until attached — the reload
    /// actor refuses to enable before composition has wired it.
    spawner: Option<Box<GatewaySpawnHook>>,
    /// The slice registry — the `discord` flag and the connection cell.
    slices: Slices,
    /// Shared application state — the live sessions whose phase gates
    /// a disruptive disable.
    state_handle: State,
    /// The bus, for the dashboard row notes.
    bus: jinn_slices::bus::BusService,
    /// The live configuration layer — the reload fold resolves
    /// `[discord]` from it (the watcher has already swapped the
    /// snapshot by the time the event lands).
    config_layer: jinn_config::ConfigLayer,
}

impl std::fmt::Debug for GatewaySupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewaySupervisor")
            .field("gateway", &self.gateway)
            .field("spawner_attached", &self.spawner.is_some())
            .finish()
    }
}

impl GatewaySupervisor {
    /// Parks the machinery from an activation's output.
    #[must_use]
    pub fn new(
        parked: DiscordGatewayChannels,
        config: DiscordConfig,
        slices: Slices,
        state: State,
        bus: jinn_slices::bus::BusService,
        config_layer: jinn_config::ConfigLayer,
    ) -> Self {
        let gateway = if config.enabled {
            GatewayState::Running
        } else {
            GatewayState::Dormant
        };
        Self {
            parked,
            config_handle: GatewayConfigHandle::new(config),
            gateway,
            shutdown: Arc::new(Notify::new()),
            spawner: None,
            slices,
            state_handle: state,
            bus,
            config_layer,
        }
    }

    /// The parked channel halves — composition passes these through to
    /// the frontend spawn hook it attaches.
    #[must_use]
    pub fn parked(&self) -> &DiscordGatewayChannels {
        &self.parked
    }

    /// Re-resolves `[discord]` from the live layer.
    ///
    /// Called by the reload actor after `JinnTomlReloaded` — the
    /// watcher swapped the snapshot before publishing (the W12
    /// invariant), so this reads post-reload values. A malformed
    /// section cannot arrive here: the validate-gated reload refused
    /// the file wholesale.
    ///
    /// # Errors
    ///
    /// Returns the section error when the layer read fails; the caller
    /// keeps the applied config and logs.
    pub fn read_section(&self) -> Result<DiscordConfig, jinn_config::ConfigSectionError> {
        self.config_layer.get::<DiscordConfig>()
    }

    /// The config handle the gateway reads through.
    #[must_use]
    pub fn config_handle(&self) -> &GatewayConfigHandle {
        &self.config_handle
    }

    /// The shutdown signal the gateway loops listen for.
    #[must_use]
    pub fn shutdown_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.shutdown)
    }

    /// Attaches the spawn hook. Composition calls this once, after the
    /// core exists; before it returns, an enabled activation has
    /// already spawned its gateway.
    pub fn attach_spawner(
        &mut self,
        spawner: Box<dyn Fn(GatewayConfigHandle, Arc<Notify>) + Send + Sync>,
    ) {
        self.spawner = Some(spawner);
        if self.gateway == GatewayState::Running {
            self.spawn_gateway();
        }
    }

    /// Spawns the gateway through the attached hook.
    fn spawn_gateway(&self) {
        if let Some(spawner) = &self.spawner {
            spawner(self.config_handle.clone(), Arc::clone(&self.shutdown));
            self.slices.set_flag("discord", true);
        }
    }

    /// Applies a disable: the gateway runs down and the flag clears.
    fn apply_disable(&mut self) {
        self.shutdown.notify_waiters();
        self.gateway = GatewayState::Dormant;
        self.slices.set_flag("discord", false);
    }

    /// Applies an enable: the gateway spawns under the current config.
    fn apply_enable(&mut self) {
        self.gateway = GatewayState::Running;
        self.spawn_gateway();
    }

    /// Folds a `JinnTomlReloaded`: diffs enablement, applies what is
    /// safe now, defers the rest to the idle boundary.
    ///
    /// Content changes always apply live through the handle. An
    /// enablement flip toward disabled while any session is busy
    /// parks instead — a live conversation must not be cut off — and
    /// lands at the next all-idle transition.
    pub async fn handle_reload(&mut self, fresh: &DiscordConfig) {
        // Content: always applies now, running or not. Enablement is
        // carried inside it, so the handle's snapshot stays consistent.
        self.config_handle.replace(fresh.clone());
        match (self.gateway, fresh.enabled) {
            (GatewayState::Running, true) | (GatewayState::Dormant, false) => {
                // No enablement change (or no gateway to change).
            }
            (GatewayState::Running, false) => {
                if any_session_busy(&self.state_handle) {
                    // Busy: park. The next all-idle boundary fires the
                    // shutdown through `SessionPhaseChanged`.
                    self.gateway = GatewayState::PendingDisable;
                    self.publish_row_note(
                        "discord disabled in config — stops when sessions go idle",
                    )
                    .await;
                } else {
                    self.apply_disable();
                }
            }
            (GatewayState::Dormant, true) => {
                // Nothing to interrupt; spawn right away.
                self.apply_enable();
            }
            (GatewayState::PendingDisable, false) => {
                // Already parked; the boundary will handle it.
            }
            (GatewayState::PendingDisable, true) => {
                // The pending disable is cancelled by re-enabling.
                self.gateway = GatewayState::Running;
                self.publish_row_note("discord re-enabled; pending disable cancelled")
                    .await;
            }
        }
    }

    /// Folds a session phase transition: fires a parked disable when
    /// every live session just went idle.
    pub async fn handle_phase_changed(&mut self, new_phase: jinn_slices::PhaseView) {
        if self.gateway != GatewayState::PendingDisable || new_phase != PhaseView::Idle {
            return;
        }
        if !any_session_busy(&self.state_handle) {
            self.apply_disable();
            self.publish_row_note("discord stopped").await;
        }
    }

    /// Reads the connection cell for the diagnostic notes.
    #[must_use]
    pub fn connection(&self) -> ConnectionState {
        self.slices
            .reader::<ConnectionState>(&discord_connection_slot())
            .map(|cell| cell.read().clone())
            .unwrap_or_default()
    }

    /// The state machine's current position — what the dashboard row
    /// and any "pending change" surface reads.
    #[must_use]
    pub fn gateway_state(&self) -> &GatewayState {
        &self.gateway
    }

    /// The `discord` slice flag: whether the rest of jinn currently
    /// sees the bridge as live.
    #[must_use]
    pub fn flag(&self) -> bool {
        self.slices.flag("discord")
    }

    async fn publish_row_note(&self, note: &str) {
        self.bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: "discord".to_owned(),
                description: None,
                status_message: Some(note.to_owned()),
                note_tone: Some(jinn_slices::NoteTone::Warning),
            })
            .await;
    }
}

/// Whether any live session is mid-turn.
///
/// A shared read guard is taken and dropped before this returns; no
/// lock spans an await (the callers snapshot the answer first).
fn any_session_busy(state: &State) -> bool {
    let guard = state.read();
    guard
        .session
        .iter()
        .any(|(_, session)| session.phase() != PhaseView::Idle)
}
