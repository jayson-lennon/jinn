//! The listener: binds the `[webui2]` socket and serves `/web2` on it.
//!
//! A service actor whose spawn is the readiness point — once it returns,
//! the port accepts connections. The bind is
//! read once and frozen for the process's lifetime; a live `[webui2]`
//! edit applies at the next restart, because a rebind would strand every
//! open page's socket with no way to learn the new port.
//!
//! A successful bind notes the address the server is running on, on the
//! listener's dashboard row, so the user can read there how to reach the
//! page.

use std::net::{IpAddr, SocketAddr};
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
    /// The shared state, outbound handle and version every request reads.
    pub shared: Arc<ServerShared>,
    /// The bus the actor is spawned on.
    pub bus: BusService,
}

/// The listener actor: a lifetime anchor for the serve task.
///
/// It holds the task and the bus so both live exactly as long as the actor
/// and are dropped with it; no method reads either field back.
#[derive(Debug)]
pub struct ListenerActor {
    /// The serve task. Held for its `Drop` alone — dropping the actor
    /// aborts the task — which is why nothing ever reads it.
    #[expect(
        dead_code,
        reason = "the task is held for its Drop, which aborts it with the actor"
    )]
    pub serve_task: tokio::task::JoinHandle<()>,
    /// The bus the actor was spawned on.
    pub bus: BusService,
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

/// The path the page is served at, matching the router's `/web2` route.
pub(crate) const PAGE_PATH: &str = "/web2";

/// What the dashboard row calls the slice.
pub(crate) const WEBUI2_ROW_DESCRIPTION: &str = "Web UI (browser transcript, send, cancel)";

/// The address the listener bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundAddress(SocketAddr);

impl BoundAddress {
    /// The row's note: how a user reaches the running server.
    ///
    /// The same string goes to the dashboard row and the startup log, so
    /// the two cannot disagree.
    #[must_use]
    pub fn connection_note(self) -> String {
        connection_note(self.0)
    }
}

/// The dashboard note for a bound listener: what a user opens to reach
/// the page, built from the address the server is actually running on.
///
/// - A wildcard bind (`0.0.0.0` / `::`) serves every interface, so no
///   single address is true. The note names the port and shows the URL
///   with its host left for the user to fill in.
/// - Any other bind names its own address. An IPv6 host keeps its
///   brackets, which a URL requires.
///
/// An IPv4-mapped wildcard (`::ffff:0.0.0.0`) is not the all-zeros address
/// and reads as a specific bind here.
#[must_use]
pub fn connection_note(bound: SocketAddr) -> String {
    if is_unspecified(bound.ip()) {
        return format!(
            "serving on port {} on all interfaces (http://<your-ip>:{}{PAGE_PATH})",
            bound.port(),
            bound.port()
        );
    }
    format!("serving on {}", page_url(bound))
}

/// The page's URL for a specific bind.
fn page_url(bound: SocketAddr) -> String {
    let host = match bound.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    format!("http://{host}:{}{PAGE_PATH}", bound.port())
}

/// Whether the address serves every interface rather than one.
fn is_unspecified(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_unspecified(),
        IpAddr::V6(ip) => ip.is_unspecified(),
    }
}

/// Binds the socket, starts the serve task, spawns the actor, and notes
/// the bound address on the listener's dashboard row.
///
/// # Errors
///
/// Returns [`ListenerError::Bind`] when the address cannot be bound;
/// activation treats that as stop-and-report.
pub async fn spawn(
    system: &ActorSystem,
    deps: &ListenerDeps,
) -> Result<(BoundAddress, OutboundHandle), error_stack::Report<ListenerError>> {
    let listener = tokio::net::TcpListener::bind(&deps.bind)
        .await
        .change_context(WebUi2BindError)
        .attach(format!("failed to bind the web UI on {}", deps.bind))
        .change_context(ListenerError::Bind)?;
    let addr = listener
        .local_addr()
        .change_context(WebUi2BindError)
        .attach("bound listener has no local address")
        .change_context(ListenerError::Bind)?;
    let bound = BoundAddress(addr);
    // The row's note carries the address from here on; the shared state
    // was built before the bind and could not name it yet.
    deps.shared.health.bound(bound.connection_note());

    let router = crate::server::router(deps.shared.clone());
    let serve_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::warn!("web UI serve task ended: {e}");
        }
    });

    let bus = deps.bus.clone();
    let note_bus = deps.bus.clone();
    let outbound = deps.shared.outbound.clone();

    trouper::builder::spawn_service_builder::<ListenerActor>(system)
        .at(ActorPath::new(WEBUI2_ACTOR_NAME))
        .init_with(move || {
            let bus = bus.clone();
            Box::pin(async move { Ok(ListenerActor { serve_task, bus }) })
        })
        .emits::<jinn_slices::ServiceStatusUpdate>()
        .start();

    // The launch note: how a user reaches the running page, derived from
    // the address that was actually bound. It publishes only after the
    // bind succeeded, so a user who reads the row can reach the address.
    // The bind is frozen for the process's lifetime, so this is the row's
    // one note — there is no later transition to report.
    note_bus
        .publish(jinn_slices::ServiceStatusUpdate {
            name: WEBUI2_ACTOR_NAME.to_owned(),
            description: Some(WEBUI2_ROW_DESCRIPTION.to_owned()),
            status_message: Some(bound.connection_note()),
            note_tone: None,
        })
        .await;

    Ok((bound, outbound))
}

#[cfg(test)]
mod note_tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::{BoundAddress, connection_note};

    #[rstest::rstest]
    #[test]
    fn a_lan_bind_names_its_page_url() {
        // Given a bind on a LAN interface.
        let bound = "192.168.1.10:8388".parse().expect("valid bind");

        // When forming the note.
        let note = connection_note(bound);

        // Then it is that address's page URL.
        assert_eq!(
            note, "serving on http://192.168.1.10:8388/web2",
            "a specific bind names its own address and the page path"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_loopback_bind_names_its_page_url() {
        // Given the default loopback bind.
        let bound = "127.0.0.1:8388".parse().expect("valid bind");

        // When forming the note.
        let note = connection_note(bound);

        // Then it is the loopback page URL, by its literal IP.
        assert_eq!(
            note, "serving on http://127.0.0.1:8388/web2",
            "loopback is a specific bind, not a wildcard"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_ipv4_wildcard_bind_gives_the_port_and_a_substitutable_url() {
        // Given a bind on every IPv4 interface.
        let bound = "0.0.0.0:8388".parse().expect("valid bind");

        // When forming the note.
        let note = connection_note(bound);

        // Then no unusable address is printed: the port and a URL to substitute into.
        assert_eq!(
            note, "serving on port 8388 on all interfaces (http://<your-ip>:8388/web2)",
            "a wildcard is not a destination, so the host is left to the user"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_ipv6_wildcard_bind_gives_the_port_and_a_substitutable_url() {
        // Given a bind on every IPv6 interface.
        let bound = "[::]:8388".parse().expect("valid bind");

        // When forming the note.
        let note = connection_note(bound);

        // Then it reads like the IPv4 wildcard, with its own port.
        assert_eq!(
            note, "serving on port 8388 on all interfaces (http://<your-ip>:8388/web2)",
            "the IPv6 wildcard is not a destination either"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_ipv6_bind_keeps_its_brackets_in_the_url() {
        // Given a specific IPv6 bind.
        let bound = "[::1]:8388".parse().expect("valid bind");

        // When forming the note.
        let note = connection_note(bound);

        // Then the host stays bracketed, as a URL requires.
        assert_eq!(
            note, "serving on http://[::1]:8388/web2",
            "an unbracketed IPv6 host is not a valid URL"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_bound_address_notes_the_address_it_was_built_from() {
        // Given the bound address the listener resolved.
        let bound = BoundAddress("127.0.0.1:9123".parse().expect("valid bind"));

        // When asking it for the note.
        let note = bound.connection_note();

        // Then it matches the formatter's output for that address.
        assert_eq!(
            note, "serving on http://127.0.0.1:9123/web2",
            "the row and the log read one string from one value"
        );
    }
}
