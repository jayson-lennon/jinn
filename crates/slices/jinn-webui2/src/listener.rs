//! The listener: binds the `[webui2]` socket and serves `/web2` on it.
//!
//! A service actor whose spawn is the readiness point — once it returns,
//! the port accepts connections. Like the original web UI, the bind is
//! read once and frozen for the process's lifetime; a live `[webui2]`
//! edit applies at the next restart, because a rebind would strand every
//! open page's socket with no way to learn the new port.

use std::sync::Arc;

use error_stack::ResultExt as _;
use jinn_preferences_config::schemas::WebUi2BindError;
use jinn_slices::bus::BusService;
use trouper::actor::{ActorPath, ServiceActor};
use trouper::system::ActorSystem;

use crate::{outbound::OutboundHandle, server::ServerShared};

/// The name the listener is spawned under.
pub const WEBUI2_ACTOR_NAME: &str = "webui2";

/// Why the listener could not come up.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum ListenerError {
    /// The configured address could not be bound (in use, permission denied).
    Bind,
}

/// Everything the listener needs to bind and serve.
#[derive(Debug, Clone)]
pub struct ListenerDeps {
    /// The `host:port` to bind.
    pub bind: String,
    /// The serving binary's version, for the health probe.
    pub version: String,
    /// The shared state and outbound handle every request reads.
    pub shared: Arc<ServerShared>,
    /// The bus the actor is spawned on.
    pub bus: BusService,
}

/// The listener actor.
#[derive(Debug)]
pub struct ListenerActor {
    /// The port that was actually bound (a `0` port resolves here).
    pub port: u16,
    /// The serve task, dropped (and so aborted) with the actor.
    pub serve_task: tokio::task::JoinHandle<()>,
    bus: BusService,
}

impl ServiceActor for ListenerActor {
    // Never called: spawned via `init_with`. The `async` signature is the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
        Err(
            error_stack::Report::new(trouper::registry::RegistryError::InvalidSpec)
                .attach("ListenerActor spawns via init_with"),
        )
    }
}

impl jinn_kernel::common::actor_deps::BusPublish for ListenerActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

/// The address the listener bound, as `(host, port)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundAddress {
    /// The host portion of the bound address.
    pub host: String,
    /// The port that was bound.
    pub port: u16,
}

impl BoundAddress {
    /// The `host:port` a browser opens.
    #[must_use]
    pub fn to_url_base(&self) -> String {
        let host = self.host.trim_start_matches(['[', ']']);
        format!("http://{host}:{}", self.port)
    }
}

/// Binds the socket, starts the serve task, and spawns the actor.
///
/// # Errors
///
/// Returns [`ListenerError::Bind`] when the address cannot be bound;
/// activation treats that as stop-and-report, like the original web UI.
pub async fn spawn(
    system: &ActorSystem,
    deps: &ListenerDeps,
) -> Result<(BoundAddress, OutboundHandle), error_stack::Report<ListenerError>> {
    let listener = tokio::net::TcpListener::bind(&deps.bind)
        .await
        .change_context(WebUi2BindError)
        .attach(format!("failed to bind the second web UI on {}", deps.bind))
        .change_context(ListenerError::Bind)?;
    let addr = listener
        .local_addr()
        .change_context(WebUi2BindError)
        .attach("bound listener has no local address")
        .change_context(ListenerError::Bind)?;
    let bound = BoundAddress {
        host: addr.ip().to_string(),
        port: addr.port(),
    };

    let router = crate::server::router(deps.shared.clone());
    let serve_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::warn!("second web UI serve task ended: {e}");
        }
    });

    let bus = deps.bus.clone();
    let port = bound.port;
    let outbound = deps.shared.outbound.clone();

    trouper::builder::spawn_service_builder::<ListenerActor>(system)
        .at(ActorPath::new(WEBUI2_ACTOR_NAME))
        .init_with(move || {
            let bus = bus.clone();
            Box::pin(async move {
                Ok(ListenerActor {
                    port,
                    serve_task,
                    bus,
                })
            })
        })
        .start();

    Ok((bound, outbound))
}
