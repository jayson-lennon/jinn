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
    /// The process turn ledger (cheap clone, shared map) — read-only
    /// from the web side; the cancel handler's no-live-turn witness.
    pub turn_ledger: jinn_kernel::common::turn_ledger::TurnLedger,
    /// The provider cell, for the model picker's cache read.
    pub provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    /// The personas cell, for the persona modal's rows and the switch
    /// POST's validation.
    pub personas_cell: jinn_slices::TypedCell<jinn_persona_msg::Personas>,
    /// The MCP runtime cell, for the MCP modal's status projection
    /// (read-only; the coordinator is the writer).
    pub mcp_runtime_cell: jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState>,
    /// The applets catalog cell, for the applets modal's rows
    /// (read-only; the applets slice's store actor is the writer).
    pub applets_catalog_cell: jinn_slices::TypedCell<jinn_applets_msg::AppletsCatalog>,
    /// The applets picker cell, for the modal's serving phase
    /// (read-only; the applets slice's republisher is the writer).
    pub applets_picker_cell: jinn_slices::TypedCell<jinn_applets_msg::AppletsPickerState>,
    /// The provider registry, for the picker rows' shared builder.
    pub provider_registry: jinn_provider_config::ProviderRegistryService,
    /// The resolved API keys, for the picker rows' availability flags.
    pub api_keys: jinn_provider_config::ApiKeysService,
    /// The user's config layer, for the new-session form's project
    /// and lifecycle lists (read-only).
    pub config: jinn_slices::ConfigLayer,
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
            deps.turn_ledger.clone(),
            deps.provider_cell.clone(),
            deps.personas_cell.clone(),
            deps.mcp_runtime_cell.clone(),
            deps.applets_catalog_cell.clone(),
            deps.applets_picker_cell.clone(),
            deps.provider_registry.clone(),
            deps.api_keys.clone(),
            deps.config.clone(),
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
        // user copies into a browser, derived from what was actually
        // bound — a configured LAN IP or all-interfaces bind must not
        // be reported as localhost. A failed note never fails the bind.
        note_bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: WEBUI_ACTOR_NAME.to_owned(),
                description: Some("Web UI (browser transcript, send, cancel)".to_owned()),
                status_message: Some(note_of(deps.bind, port)),
                note_tone: None,
            })
            .await;

        Ok(path)
    }
}

/// The dashboard note for the resolved listener: what a user copies
/// into a browser, derived from the configured bind rather than an
/// assumed localhost.
///
/// - A loopback bind names its exact URL (the literal IP, not
///   `localhost` — on dual-stack systems `localhost` can resolve to
///   `::1` first and miss a `127.0.0.1`-only bind).
/// - An unspecified bind (`0.0.0.0` / `::`) serves every interface, so
///   no single URL is true; the note says so instead of guessing.
/// - Any other bind names its own IP and port.
#[must_use]
fn note_of(bind: std::net::SocketAddr, port: u16) -> String {
    use std::net::IpAddr;
    let unspecified = match bind.ip() {
        IpAddr::V4(ip) => ip.is_unspecified(),
        IpAddr::V6(ip) => ip.is_unspecified(),
    };
    if unspecified {
        format!("serving on all interfaces, port {port}")
    } else {
        format!("serving on http://{}:{port}/", bind.ip())
    }
}

#[cfg(test)]
mod note_tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::note_of;

    #[rstest::rstest]
    #[test]
    fn a_loopback_bind_notes_its_exact_url() {
        // Given the default loopback bind.
        let bind = "127.0.0.1:8387".parse().expect("valid bind");

        // When forming the note.
        let note = note_of(bind, 8387);

        // Then it names the literal IP URL, not `localhost`.
        assert_eq!(note, "serving on http://127.0.0.1:8387/");
    }

    #[rstest::rstest]
    #[test]
    fn an_all_interfaces_bind_does_not_pretend_to_be_localhost() {
        // Given a bind on every interface.
        let bind = "0.0.0.0:8387".parse().expect("valid bind");

        // When forming the note.
        let note = note_of(bind, 8387);

        // Then it says all-interfaces instead of naming one address.
        assert_eq!(note, "serving on all interfaces, port 8387");
    }

    #[rstest::rstest]
    #[test]
    fn a_lan_bind_names_its_own_address() {
        // Given a bind on a LAN interface.
        let bind = "192.168.1.10:8387".parse().expect("valid bind");

        // When forming the note.
        let note = note_of(bind, 8387);

        // Then the note is that address's URL.
        assert_eq!(note, "serving on http://192.168.1.10:8387/");
    }
}
