//! The listener actor — binds the control surface at activation and
//! owns the axum serve task's lifetime.
//!
//! The actor consumes no bus messages: delivery publishes *existing*
//! chat-input schemas directly from the HTTP handlers through
//! [`BusService`]. Its purpose is the async init context (activation is
//! sync; binding is async) and the serve task's home: when the actor
//! system shuts down, the listener goes with the process.
//!
//! Init order is the readiness contract: bind → resolve the port →
//! register in the instance registry → spawn the axum task. A client
//! that sees the registry entry can reach the port, because
//! registration happens only after the bind succeeded.

use marked_path::Absolute;
use marked_path::MarkedPathAccess as _;
use marked_path::MarkedPathBuf;
use std::sync::Arc;

use jinn_boot_msg::JinnTomlReloaded;
use jinn_kernel::State;
use jinn_remote_msg::registry;
use jinn_slices::bus::BusService;
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment.
pub const REMOTE_ACTOR_NAME: &str = "remote";

/// The listener actor: bound and serving from init onward.
pub struct RemoteListenerActor {
    /// The resolved port, for the registry entry and shutdown removal.
    port: u16,
    /// The axum serve task. Held so a rebind aborts it — aborting
    /// closes the old listener and frees the old port.
    serve_task: Option<tokio::task::JoinHandle<()>>,
    /// The registry file this instance advertises itself in — a rebind
    /// rewrites the entry so `jinn remote` discovery stays truthful.
    registry_path: MarkedPathBuf<Absolute>,
    /// This process's pid — the registry entry's key.
    pid: u32,
    /// The live session state the HTTP handlers read; the rebind
    /// rebuilds the server state with it.
    state: State,
    /// The message fabric, for delivery publications and row notes.
    bus: BusService,
    /// The serving binary's version (reported by `/health`).
    version: String,
    /// The live config layer: a `JinnTomlReloaded` re-reads `[remote]
    /// bind` and hot-rebinds (the control surface is plain stateless
    /// HTTP — no held connections a rebind would break).
    config: jinn_config::ConfigLayer,
}

/// Dependencies for [`RemoteListenerActor`].
#[derive(Clone)]
pub struct RemoteListenerActorDeps {
    /// The bind from `[remote] bind`, pre-parsed and loopback-checked.
    pub bind: std::net::SocketAddr,
    /// The live config layer: a `JinnTomlReloaded` re-reads the bind.
    pub config: jinn_config::ConfigLayer,
    /// The live session state the handlers read and mark.
    pub state: State,
    /// The message fabric, for delivery publications.
    pub bus: BusService,
    /// The registry file this instance advertises itself in.
    pub registry_path: MarkedPathBuf<Absolute>,
    /// This process's pid.
    pub pid: u32,
    /// The serving binary's version.
    pub version: String,
}

impl ServiceActor for RemoteListenerActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args). The `async` signature is the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("RemoteListenerActor spawns via init_with"))
    }
}

impl RemoteListenerActor {
    /// Spawns the actor and starts the listener.
    ///
    /// The spawn handshake is the readiness point: once it returns, the
    /// registry carries this instance and the port accepts connections.
    ///
    /// # Errors
    ///
    /// Returns the bind failure (address in use, permission denied) or
    /// the registry write failure; either aborts activation where the
    /// cause is visible.
    pub async fn spawn(
        system: &ActorSystem,
        deps: &RemoteListenerActorDeps,
    ) -> Result<ActorPath, error_stack::Report<RegistryError>> {
        // Port 0 asks the OS here, at listener start, so the registry
        // entry and /health report the truth.
        let listener = tokio::net::TcpListener::bind(deps.bind)
            .await
            .map_err(|e| {
                error_stack::Report::new(RegistryError::InvalidSpec).attach(format!(
                    "failed to bind the remote control surface on {}: {e}",
                    deps.bind
                ))
            })?;
        let resolved = listener
            .local_addr()
            .map_err(|e| {
                error_stack::Report::new(RegistryError::InvalidSpec)
                    .attach(format!("bound listener has no local address: {e}"))
            })?
            .port();

        // Registry write: a running jinn is discoverable only through
        // this file, so its failure aborts activation rather than
        // leaving a listener nothing can find.
        let entry = registry::InstanceEntry::new(
            deps.pid,
            resolved,
            std::env::current_dir()
                .map(|cwd| cwd.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or_default())
                .unwrap_or_default(),
        );
        registry::register(deps.registry_path.as_path(), entry).map_err(|e| {
            error_stack::Report::new(RegistryError::InvalidSpec).attach(format!(
                "failed to write the instance registry at {}: {e}",
                deps.registry_path.as_path().display()
            ))
        })?;

        let shared = Arc::new(crate::server::ServerShared::new(
            deps.state.clone(),
            deps.bus.clone(),
            deps.pid,
            resolved,
            deps.version.clone(),
        ));
        let serve_task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, crate::server::router((*shared).clone())).await {
                tracing::warn!("remote control surface serve task ended: {e}");
            }
        });

        let path = ActorPath::new(REMOTE_ACTOR_NAME);
        let registry_path = deps.registry_path.clone();
        let config_layer = deps.config.clone();
        let pid = deps.pid;
        let state = deps.state.clone();
        let bus = deps.bus.clone();
        let version = deps.version.clone();

        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with(move || {
                let port = resolved;
                let registry_path = registry_path.clone();
                let config_layer = config_layer;
                let state = state.clone();
                let bus = bus.clone();
                let version = version.clone();
                Box::pin(async move {
                    Ok(Self {
                        port,
                        serve_task: Some(serve_task),
                        registry_path,
                        pid,
                        state,
                        bus,
                        version,
                        config: config_layer,
                    })
                })
            })
            .handles::<JinnTomlReloaded>()
            .emits::<jinn_slices::ServiceStatusUpdate>()
            .start();
        Ok(path)
    }
}

// The actor consumes no other messages: delivery publishes existing
// schemas from the HTTP handlers directly.
//
// A reload re-reads the bind and hot-rebinds: the control surface is
// plain stateless HTTP, so a rebind between requests breaks nobody.
// The registry entry is rewritten first-if-listener-succeeds so `jinn
// remote` discovery never names a dead port; a failed rebind keeps the
// old entry and the old listener (the serve task dies with its
// listener only when the new bind succeeded, so the failure path here
// never touches the old one).
impl MsgHandler<JinnTomlReloaded> for RemoteListenerActor {
    async fn handle(&mut self, _msg: &JinnTomlReloaded, _ctx: &mut trouper::context::MsgCtx<'_>) {
        // Given a jinn.toml whose [remote] bind just changed.
        let parsed = self
            .config
            .get::<jinn_preferences_config::schemas::RemoteConfig>()
            .ok()
            .and_then(|config| config.parse_bind().ok());
        let Some(bind) = parsed else {
            self.note(
                "remote bind re-read refused; the previous bind stays live",
                Some(jinn_slices::NoteTone::Error),
            )
            .await;
            return;
        };
        if bind.port() == self.port {
            return;
        }
        // Bind the new listener BEFORE touching anything live: if this
        // fails (port taken), the old listener and its registry entry
        // stay untouched — the refusal is the whole story. The old task
        // is aborted only after the new bind succeeded (below).
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(e) => {
                self.note(
                    &format!("remote rebind to {bind} failed ({e}); the previous bind stays live"),
                    Some(jinn_slices::NoteTone::Error),
                )
                .await;
                return;
            }
        };
        let resolved = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => {
                self.note(
                    &format!(
                        "remote rebind to {bind} lost its port ({e}); the previous bind stays live"
                    ),
                    Some(jinn_slices::NoteTone::Error),
                )
                .await;
                return;
            }
        };
        // Registry write while BOTH listeners are up: the entry names
        // the new port and the old one still serves, so discovery can
        // never land on a dead port at any instant.
        let cwd = std::env::current_dir()
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entry = registry::InstanceEntry::new(
            self.pid,
            resolved,
            cwd,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or_default())
                .unwrap_or_default(),
        );
        if let Err(e) = registry::register(self.registry_path.as_path(), entry) {
            self.note(
                &format!(
                    "remote rebind reached {resolved} but the registry write failed ({e}); \
                     the previous bind stays live"
                ),
                Some(jinn_slices::NoteTone::Error),
            )
            .await;
            return;
        }
        // Swap: end the old serve task (aborting it closes the old
        // listener and frees the old port), then hold the new one.
        if let Some(old) = self.serve_task.take() {
            old.abort();
        }
        let shared = crate::server::ServerShared::new(
            self.state.clone(),
            self.bus.clone(),
            self.pid,
            resolved,
            self.version.clone(),
        );
        self.serve_task = Some(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, crate::server::router(shared)).await {
                tracing::warn!("remote control surface serve task ended: {e}");
            }
        }));
        let old = self.port;
        self.port = resolved;
        self.note(
            &format!("remote rebound to {resolved} (was {old}) — the bind change applied"),
            None,
        )
        .await;
    }
}

impl RemoteListenerActor {
    /// Publishes a row note on the remote dashboard row. Best-effort:
    /// a failed note must not fail the rebind.
    async fn note(&self, message: &str, tone: Option<jinn_slices::NoteTone>) {
        self.bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: REMOTE_ACTOR_NAME.to_owned(),
                description: None,
                status_message: Some(message.to_owned()),
                note_tone: tone,
            })
            .await;
    }
}

/// Removes this instance's registry entry. Best effort by design: the
/// graceful shutdown path calls it, but a dead entry is invisible to
/// every reader through the port probe either way.
pub fn deregister(registry_path: &std::path::Path, pid: u32) {
    if let Err(e) = registry::remove_entry(registry_path, pid) {
        tracing::debug!(
            "best-effort registry removal failed for pid {pid} at {}: {e}",
            registry_path.display()
        );
    }
}
