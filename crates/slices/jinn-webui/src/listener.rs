//! The listener actor — binds the web UI at activation and owns the
//! axum serve task's lifetime.
//!
//! Like the remote listener minus the parts webui must not have: no
//! config-reload handling (the bind is frozen — a rebind would strand
//! connected SSE pages) and no instance-registry write (the registry
//! is pid-keyed and remote owns this pid's entry; a second writer
//! would corrupt discovery). Resolved-URL notes go to the webui
//! dashboard row via [`ServiceStatusUpdate`].
//!
//! Init order is the readiness contract: bind → resolve the port →
//! spawn the fan-out's consumers' router → announce. A user who reads
//! the dashboard row can reach the port, because the note publishes
//! only after the bind succeeded.

use std::sync::Arc;

use crate::fanout::FanoutHandle;
use jinn_kernel::State;
use jinn_slices::bus::BusService;
use trouper::actor::ActorPath;
use trouper::actor::ServiceActor;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment — also the dashboard row name.
pub const WEBUI_ACTOR_NAME: &str = "webui";

/// The listener actor: bound and serving from init onward.
pub struct WebUiListenerActor {
    /// The resolved port. Held for future notes; the launch note
    /// already published carries it.
    #[expect(
        dead_code,
        reason = "held so a later handler can note the URL without re-deriving it; today's only note happens at spawn"
    )]
    port: u16,
    /// The axum serve task. Held so the task dies with the actor.
    #[expect(
        dead_code,
        reason = "the handle's only duty is lifetime: dropping it at actor teardown ends the serve task"
    )]
    serve_task: tokio::task::JoinHandle<()>,
    /// The message fabric, for dashboard notes.
    #[expect(
        dead_code,
        reason = "held for the same future notes the port is; the launch note publishes through the spawn-local clone"
    )]
    bus: BusService,
}

/// Dependencies for [`WebUiListenerActor`].
#[derive(Clone)]
pub struct WebUiListenerActorDeps {
    /// The bind from `[webui] bind`, pre-parsed. Frozen for the
    /// process lifetime.
    pub bind: std::net::SocketAddr,
    /// The live session state the handlers read.
    pub state: State,
    /// The message fabric, for action publications and notes.
    pub bus: BusService,
    /// The session store, for archived sessions' summaries and
    /// transcripts.
    pub session_store: jinn_session_state::SessionStoreService,
    /// The provider cell, for the model picker's cache read.
    pub provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    /// The fan-out's broadcast handle, shared with the SSE endpoint.
    pub fanout: FanoutHandle,
    /// The serving binary's version.
    pub version: String,
}

impl ServiceActor for WebUiListenerActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args). The `async` signature is the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("WebUiListenerActor spawns via init_with"))
    }
}

impl WebUiListenerActor {
    /// Spawns the actor and starts the listener.
    ///
    /// The spawn handshake is the readiness point: once it returns, the
    /// port accepts connections and the fan-out's subscriptions are
    /// live.
    ///
    /// # Errors
    ///
    /// Returns the bind failure (address in use, permission denied);
    /// aborts activation where the cause is visible.
    pub async fn spawn(
        system: &ActorSystem,
        deps: &WebUiListenerActorDeps,
    ) -> Result<ActorPath, error_stack::Report<RegistryError>> {
        let listener = tokio::net::TcpListener::bind(deps.bind)
            .await
            .map_err(|e| {
                error_stack::Report::new(RegistryError::InvalidSpec)
                    .attach(format!("failed to bind the web UI on {}: {e}", deps.bind))
            })?;
        let resolved = listener
            .local_addr()
            .map_err(|e| {
                error_stack::Report::new(RegistryError::InvalidSpec)
                    .attach(format!("bound listener has no local address: {e}"))
            })?
            .port();

        let shared = Arc::new(crate::server::ServerShared::new(
            deps.state.clone(),
            deps.bus.clone(),
            deps.session_store.clone(),
            deps.provider_cell.clone(),
            deps.fanout.clone(),
            resolved,
            deps.version.clone(),
        ));
        let serve_task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, crate::server::router((*shared).clone())).await {
                tracing::warn!("web UI serve task ended: {e}");
            }
        });

        let path = ActorPath::new(WEBUI_ACTOR_NAME);
        let actor_bus = deps.bus.clone();
        let note_bus = deps.bus.clone();
        let port = resolved;

        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with(move || {
                Box::pin(async move {
                    Ok(Self {
                        port,
                        serve_task,
                        bus: actor_bus,
                    })
                })
            })
            // No `.handles`: the bind is frozen. A config reload finds no
            // subscriber here, by design — re-reads would strand the SSE
            // pages.
            .emits::<jinn_slices::ServiceStatusUpdate>()
            .start();

        // Best-effort launch note on the webui dashboard row: the URL a
        // user copies into a browser. A failed note never fails the
        // bind.
        note_bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: WEBUI_ACTOR_NAME.to_owned(),
                description: Some("Web UI (browser transcript, send, cancel)".to_owned()),
                status_message: Some(format!("serving on http://localhost:{port}/")),
                note_tone: None,
            })
            .await;

        Ok(path)
    }
}
