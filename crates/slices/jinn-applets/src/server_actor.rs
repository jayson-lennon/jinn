//! The applets server actor — the one serving host, dormant until the
//! first open.
//!
//! Nothing binds at activation: no listener, no filesystem work. The
//! store actor sends the actor an [`EnsureServing`] internal command the
//! first time an applet is opened (and on every open after — the bound
//! listener is reused, never restarted). The first `EnsureServing`
//! establishes the axum listener (port `0` resolved there, so the URL an
//! opener builds is the truth); every later one is a no-op that reads
//! back the resolved bind. The server lives as long as the process:
//! when jinn exits, the listener closes.
//!
//! The actor owns the serving state exclusively and answers every
//! `EnsureServing` with the resolved bind, so the URL the picker's
//! browser-launch builds and the listener a browser connects to can
//! never disagree.

mod handlers;

use std::net::SocketAddr;
use std::sync::Arc;

use marked_path::{Absolute, MarkedPathBuf};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment.
pub const APPLETS_ACTOR_NAME: &str = "applets-server";

/// The internal command the store actor sends on an applet open: make sure
/// the listener is up, and answer with the resolved bind.
///
/// Rides the actor's mailbox as an ask — the reply is the resolved
/// bind, or the refusal sentence on a bind failure. Not a bus schema:
/// it is actor-internal, between the two actors of one slice.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Ensure the applets server is bound; reply with the resolved bind.")]
pub struct EnsureServing {
    /// The applets root to serve from (the store actor owns the path).
    pub apps_root: String,
}

impl jinn_slices::BusMessage for EnsureServing {}

/// The ask's reply: the resolved bind on success, the failure sentence
/// on refusal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "Reply to an EnsureServing ask.")]
pub struct EnsureServingOutcome {
    /// Whether the server is bound and serving.
    pub ok: bool,
    /// The resolved bind the listener accepted (port 0 already
    /// resolved). Empty on failure.
    pub bind: String,
    /// The failure sentence on failure, empty on success.
    pub error: String,
}

impl EnsureServingOutcome {
    /// The success outcome, carrying the resolved bind.
    #[must_use]
    pub fn serving(bind: SocketAddr) -> Self {
        Self {
            ok: true,
            bind: bind.to_string(),
            error: String::new(),
        }
    }

    /// The failure outcome, carrying the sentence.
    #[must_use]
    pub fn refused(error: String) -> Self {
        Self {
            ok: false,
            bind: String::new(),
            error,
        }
    }
}

impl jinn_slices::BusMessage for EnsureServingOutcome {}

/// The applets server actor: dormant until the first `EnsureServing`.
pub struct AppletsServerActor {
    /// The bind from `[applets] bind`. Port `0` binds an OS-assigned
    /// port at the first ensure. Re-read from the config layer on every
    /// `JinnTomlReloaded`; a changed bind hot-rebinds (the HTTP surface
    /// is stateless — no held connections a rebind would break).
    bind: SocketAddr,
    /// The live config layer the bind re-reads from on a reload.
    config: jinn_config::ConfigLayer,
    /// The applets root the server serves from — the rebind path
    /// re-establishes against it.
    apps_root: MarkedPathBuf<Absolute>,
    /// The bus, for the dashboard row's bind-change notes.
    bus: jinn_slices::bus::BusService,
    /// The serving phase once the first ensure has established it;
    /// `None` is the dormant phase (no listener has ever bound).
    serving: Option<ServingState>,
}

/// Everything the live serving phase owns.
struct ServingState {
    /// The resolved bind the listener accepted (port `0` already
    /// resolved at the first ensure; stable for the server's life).
    bind: SocketAddr,
    /// The axum serve task. Held (not detached) so a rebind aborts it —
    /// aborting closes the old listener and frees the old port.
    serve_task: tokio::task::JoinHandle<()>,
}

impl ServiceActor for AppletsServerActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args).
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("AppletsServerActor spawns via init_with"))
    }
}

/// Dependencies for [`AppletsServerActor`].
#[derive(Clone)]
pub struct AppletsServerActorDeps {
    /// The bind from `[applets] bind`, pre-parsed and pre-validated
    /// loopback by `activate`. A bind that cannot parse or names an
    /// exposed interface never gets here: activation aborts launch.
    pub bind: SocketAddr,
    /// The live config layer: a `JinnTomlReloaded` re-reads the bind.
    pub config: jinn_config::ConfigLayer,
    /// The applets root the server serves from — the rebind path
    /// re-establishes against it.
    pub apps_root: MarkedPathBuf<Absolute>,
    /// The bus, for the dashboard row's bind-change notes.
    pub bus: jinn_slices::bus::BusService,
}

impl AppletsServerActor {
    /// Spawns the actor, dormant — no listener binds during the spawn
    /// handshake.
    pub fn spawn(system: &ActorSystem, deps: &AppletsServerActorDeps) -> ActorPath {
        let path = ActorPath::new(APPLETS_ACTOR_NAME);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .init_with({
                let bind = deps.bind;
                let config = deps.config.clone();
                let apps_root = deps.apps_root.clone();
                let bus = deps.bus.clone();
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            bind,
                            config,
                            apps_root,
                            bus,
                            serving: None,
                        })
                    })
                }
            })
            .handles::<EnsureServing>()
            // Live config: a jinn.toml reload re-reads [applets] bind
            // and hot-rebinds (the HTTP surface is stateless — no held
            // connections to break).
            .handles::<jinn_boot_msg::JinnTomlReloaded>()
            .emits::<EnsureServingOutcome>()
            .emits::<jinn_slices::ServiceStatusUpdate>()
            .start()
    }
}

impl MsgHandler<EnsureServing> for AppletsServerActor {
    async fn handle(&mut self, msg: &EnsureServing, ctx: &mut MsgCtx<'_>) {
        handlers::handle_ensure_serving(self, msg, ctx).await;
    }
}

// A reload re-reads the bind and hot-rebinds: the applets surface is
// plain stateless HTTP (no held connections, no websocket), so a
// rebind between requests breaks nobody, and the picker's URL base
// stays correct without a restart. A failed rebind keeps the old
// listener via keep-dormant: the next ensure tries the new bind fresh.
impl MsgHandler<jinn_boot_msg::JinnTomlReloaded> for AppletsServerActor {
    async fn handle(&mut self, _msg: &jinn_boot_msg::JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        let parsed = self
            .config
            .get::<jinn_preferences_config::schemas::AppletsConfig>()
            .ok()
            .and_then(|config| config.parse_bind().ok());
        let Some(bind) = parsed else {
            self.note(
                "applets bind re-read refused; the previous bind stays live",
                Some(jinn_slices::NoteTone::Error),
            )
            .await;
            return;
        };
        let live = self.serving.as_ref().map(|server| server.bind);
        if live.is_some_and(|serving| serving == bind) || self.bind == bind {
            return;
        }
        match live {
            // Dormant: adopt the new bind; the next ensure uses it.
            None => {
                self.bind = bind;
                self.note(&format!("applets bind changed to {bind}"), None)
                    .await;
            }
            // Serving: hot-rebind. The old serve task is aborted
            // first (closing the old listener and freeing the old
            // port); the new one serves the same root.
            Some(_) => {
                if let Some(old) = self.serving.take() {
                    old.serve_task.abort();
                }
                match handlers::bind_listener(bind).await {
                    Ok((listener, resolved)) => {
                        self.serving = Some(handlers::serve(listener, &self.apps_root, resolved));
                        self.bind = resolved;
                        self.note(
                            &format!("applets rebound to {resolved} — the bind change applied"),
                            None,
                        )
                        .await;
                    }
                    Err(error) => {
                        // Keep-old is impossible (the old listener is
                        // gone with the serving state), so go dormant;
                        // the next ensure tries the new bind fresh.
                        self.serving = None;
                        self.bind = bind;
                        self.note(
                            &format!(
                                "applets rebind to {bind} failed ({error}); the next open retries it"
                            ),
                            Some(jinn_slices::NoteTone::Error),
                        )
                        .await;
                    }
                }
            }
        }
    }
}

impl AppletsServerActor {
    /// Publishes a row note on the applets dashboard row.
    async fn note(&self, message: &str, tone: Option<jinn_slices::NoteTone>) {
        self.bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: APPLETS_ACTOR_NAME.to_owned(),
                description: None,
                status_message: Some(message.to_owned()),
                note_tone: tone,
            })
            .await;
    }
}

/// Builds the shared request-handler state for a bound listener over
/// `apps_root`.
#[must_use]
pub fn shared_for(apps_root: MarkedPathBuf<Absolute>) -> Arc<crate::server::ServerShared> {
    Arc::new(crate::server::ServerShared::new(apps_root))
}
