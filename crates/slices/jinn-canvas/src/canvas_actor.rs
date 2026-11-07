//! The canvas actor — the one serving host.
//!
//! Serves one HTML artifact per process over HTTP (on whatever interface
//! the `[canvas] bind` config names; loopback by default) with
//! WebSocket live reload. Dormant until the `canvas` tool's first serve
//! ask: no listener, no watcher, no filesystem work at activation. The
//! first [`CanvasCommand`] establishes the axum listener and the
//! directory watcher; every later ask swaps the served artifact on the
//! live listener (same bind, watcher re-armed onto the new directory).
//! Pages live and die with the process: when jinn exits, the listener
//! closes.
//!
//! The actor owns the server state exclusively — the [`ServerShared`],
//! the listener task, and the watcher — and is the sole writer of its
//! dashboard row, through [`ServiceStatusUpdate`] publications on every
//! observable transition: offline at activation, the bind URL and
//! served path on each successful serve, an error-toned note on a
//! failed serve. The message carries no lifecycle field (the recorded
//! asymmetry: liveness is the runtime census's verdict, not the row's
//! to contradict).
//!
//! The tool layer reaches the actor through the
//! [`CanvasServerHandle`] minted in [`activate`]; the actor-side
//! implementation bounds the ask, so a wedged actor yields a transport
//! error rather than hanging the caller.

mod handlers;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jinn_boot_msg::JinnTomlReloaded;
use jinn_canvas_msg::CanvasCommand;
use jinn_canvas_msg::DrainCanvasMetrics;
use jinn_slices::bus::BusService;
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::server::ServerShared;

/// The actor's registered path segment. The dashboard row and the
/// status publications key off this name.
pub const CANVAS_ACTOR_NAME: &str = "canvas";

/// The outer bound on a serve round trip. The actor answers every
/// command (success or a real refusal), so reaching this bound means
/// the actor never processed the ask — reported as a transport failure,
/// never as a domain one.
pub(crate) const SERVE_ASK_TIMEOUT: Duration = Duration::from_secs(15);

/// The canvas actor: dormant until the first serve command.
pub struct CanvasActor {
    /// The bind to listen on, from `[canvas] bind`. Port `0` binds an
    /// OS-assigned port at the first serve. Re-read from the config
    /// layer on every `JinnTomlReloaded`; while a listener is live a
    /// changed bind parks here until the last browser disconnects (see
    /// `pending_bind`), then applies.
    bind: std::net::SocketAddr,
    /// The config layer the bind re-reads from on a reload.
    config: jinn_config::ConfigLayer,
    /// The server state once the first serve has established it; `None`
    /// is the dormant phase (no listener has ever bound).
    server: Option<ServingState>,
    /// A bind change observed while a listener was live. `None` until
    /// such a change lands; cleared when it applies. The apply point is
    /// the actor's drain tick: once no browser holds a page (zero ws
    /// clients), the old URL has nobody holding it and the next serve
    /// uses the new bind.
    pending_bind: Option<std::net::SocketAddr>,
    /// The message fabric, for the dashboard status publications.
    bus: BusService,
}

/// Everything the live serving phase owns.
struct ServingState {
    /// The resolved bind the listener accepted (port `0` already
    /// resolved at the first serve; stable for the server's life).
    bind: std::net::SocketAddr,
    /// Shared request-handler state: artifact path, reload broadcast,
    /// watcher slot.
    shared: ServerShared,
    /// The axum serve task. Held (not detached) so a pending-bind
    /// rebind aborts it — aborting closes the old listener and frees
    /// the old port; a detached task would pin the port forever.
    serve_task: tokio::task::JoinHandle<()>,
}

impl ServiceActor for CanvasActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args). The `async` signature is the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("CanvasActor spawns via init_with"))
    }
}

/// Dependencies for [`CanvasActor`].
#[derive(Clone)]
pub struct CanvasActorDeps {
    /// The bind from `[canvas] bind`, pre-parsed. A bind that fails to
    /// parse cannot activate the slice: the serve path would refuse
    /// every ask with the same sentence, so the failure is surfaced
    /// where it can abort launch instead.
    pub bind: std::net::SocketAddr,
    /// The live config layer: a `JinnTomlReloaded` re-reads the bind
    /// from here while the actor is dormant.
    pub config: jinn_config::ConfigLayer,
    /// The message fabric, for the dashboard status publications.
    pub bus: BusService,
}

impl CanvasActor {
    /// Spawns the actor at `canvas` and publishes the offline status.
    ///
    /// The spawn handshake is the readiness point: once it returns, the
    /// actor's `CanvasCommand` subscription is live and a serve ask can
    /// never race the mailbox.
    pub fn spawn(system: &ActorSystem, deps: &CanvasActorDeps) -> ActorPath {
        let path = ActorPath::new(CANVAS_ACTOR_NAME);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    Box::pin(async move {
                        let actor = Self {
                            bind: deps.bind,
                            config: deps.config,
                            server: None,
                            pending_bind: None,
                            bus: deps.bus.clone(),
                        };
                        // The offline publication is the row's baseline:
                        // created by the status fold, corrected by the
                        // runtime census's own spawn announcement.
                        actor.publish_offline().await;
                        Ok(actor)
                    })
                }
            })
            .handles::<CanvasCommand>()
            .handles::<DrainCanvasMetrics>()
            // Live config: a jinn.toml reload re-reads `[canvas] bind`.
            .handles::<jinn_boot_msg::JinnTomlReloaded>()
            .emits::<jinn_canvas_msg::CanvasServeOutcome>()
            .emits::<jinn_canvas_msg::CanvasMetricsDrainOutcome>()
            .emits::<jinn_slices::ServiceStatusUpdate>()
            .mailbox(16, trouper::inbox::OverloadPolicy::Block)
            .start()
    }

    /// Publishes the offline row status: no server is live.
    pub(crate) async fn publish_offline(&self) {
        self.publish_status(Some(crate::status::offline_message()), None)
            .await;
    }

    /// Publishes the serving row status: bind URL and artifact path.
    pub(crate) async fn publish_serving(&self, url: &str, artifact: &Path) {
        self.publish_status(Some(crate::status::serving_message(url, artifact)), None)
            .await;
    }

    /// Publishes a failed serve as an error-toned note on the row.
    pub(crate) async fn publish_serve_failed(&self, reason: &str) {
        self.publish_status(Some(reason.to_owned()), Some(jinn_slices::NoteTone::Error))
            .await;
    }

    /// The single publication path: row name fixed to the actor's
    /// registered name, description left to whatever the row already
    /// says.
    async fn publish_status(
        &self,
        status_message: Option<String>,
        note_tone: Option<jinn_slices::NoteTone>,
    ) {
        self.bus
            .publish(jinn_slices::ServiceStatusUpdate {
                name: CANVAS_ACTOR_NAME.to_owned(),
                description: None,
                status_message,
                note_tone,
            })
            .await;
    }
}

impl MsgHandler<CanvasCommand> for CanvasActor {
    async fn handle(&mut self, msg: &CanvasCommand, ctx: &mut MsgCtx<'_>) {
        handlers::handle_serve(self, msg, ctx).await;
    }
}

impl MsgHandler<DrainCanvasMetrics> for CanvasActor {
    async fn handle(&mut self, _msg: &DrainCanvasMetrics, ctx: &mut MsgCtx<'_>) {
        handlers::handle_drain_metrics(self, ctx).await;
    }
}

impl MsgHandler<JinnTomlReloaded> for CanvasActor {
    async fn handle(&mut self, _msg: &JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_reload(self).await;
    }
}

/// Mints the tool-facing handle from the spawned actor's path.
///
/// Composition calls this after the spawn handshake and stores the
/// result in `Services.canvas_server`; an unset handle keeps the
/// `canvas` tool's graceful-error path.
#[must_use]
pub fn canvas_server_handle(
    system: ActorSystem,
    actor_path: ActorPath,
) -> Arc<dyn jinn_canvas_msg::CanvasServerHandle> {
    struct Impl(ActorSystem, ActorPath);

    #[async_trait::async_trait]
    impl jinn_canvas_msg::CanvasServerHandle for Impl {
        async fn serve(
            &self,
            path: String,
        ) -> Result<jinn_canvas_msg::CanvasServeOutcome, jinn_canvas_msg::CanvasServeTransportError>
        {
            // Outer bound so a wedged actor yields Timeout instead of
            // hanging the tool caller. The trouper ask has its own
            // mandatory timeout; this outer timeout wraps the whole
            // round trip and stays the authoritative bound.
            match tokio::time::timeout(
                SERVE_ASK_TIMEOUT,
                self.0
                    .ask(self.1.clone(), CanvasCommand { path }, SERVE_ASK_TIMEOUT),
            )
            .await
            {
                Ok(Ok(value)) => {
                    let outcome = value
                        .decode::<jinn_canvas_msg::CanvasServeOutcome>()
                        .unwrap_or_else(|_| {
                            jinn_canvas_msg::CanvasServeOutcome::refused(
                                "the canvas actor's reply did not decode".to_owned(),
                            )
                        });
                    Ok(outcome)
                }
                Ok(Err(_report)) => Err(jinn_canvas_msg::CanvasServeTransportError::Mailbox),
                Err(_) => Err(jinn_canvas_msg::CanvasServeTransportError::Timeout),
            }
        }

        async fn drain_metrics(
            &self,
        ) -> Result<
            jinn_canvas_msg::CanvasMetricsDrainOutcome,
            jinn_canvas_msg::CanvasServeTransportError,
        > {
            // The same outer bound as `serve`: a wedged actor yields a
            // transport error instead of hanging the enqueue path — the
            // user's message must go out either way.
            match tokio::time::timeout(
                SERVE_ASK_TIMEOUT,
                self.0.ask(
                    self.1.clone(),
                    jinn_canvas_msg::DrainCanvasMetrics,
                    SERVE_ASK_TIMEOUT,
                ),
            )
            .await
            {
                Ok(Ok(value)) => match value.decode::<jinn_canvas_msg::CanvasMetricsDrainOutcome>()
                {
                    Ok(outcome) => Ok(outcome),
                    Err(_decode) => Err(jinn_canvas_msg::CanvasServeTransportError::Mailbox),
                },
                Ok(Err(_report)) => Err(jinn_canvas_msg::CanvasServeTransportError::Mailbox),
                Err(_) => Err(jinn_canvas_msg::CanvasServeTransportError::Timeout),
            }
        }

        fn name(&self) -> &'static str {
            "canvas-server"
        }
    }

    Arc::new(Impl(system, actor_path))
}
