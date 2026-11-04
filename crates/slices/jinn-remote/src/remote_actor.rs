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
    _port: u16,
    /// The registry path, for best-effort shutdown removal.
    _registry_path: MarkedPathBuf<Absolute>,
}

/// Dependencies for [`RemoteListenerActor`].
#[derive(Clone)]
pub struct RemoteListenerActorDeps {
    /// The bind from `[remote] bind`, pre-parsed and loopback-checked.
    pub bind: std::net::SocketAddr,
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
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, crate::server::router((*shared).clone())).await {
                tracing::warn!("remote control surface serve task ended: {e}");
            }
        });

        let path = ActorPath::new(REMOTE_ACTOR_NAME);
        let registry_path = deps.registry_path.clone();
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with(move || {
                let port = resolved;
                let registry_path = registry_path.clone();
                Box::pin(async move {
                    Ok(Self {
                        _port: port,
                        _registry_path: registry_path,
                    })
                })
            })
            .start();
        Ok(path)
    }
}

impl MsgHandler<trouper::json::Json> for RemoteListenerActor {
    async fn handle(
        &mut self,
        _msg: &trouper::json::Json,
        _ctx: &mut trouper::context::MsgCtx<'_>,
    ) {
        // The actor consumes no messages: delivery publishes existing
        // schemas from the HTTP handlers directly.
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
