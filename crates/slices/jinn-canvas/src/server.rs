//! The axum listener — everything a running canvas server does.
//!
//! Surfaces:
//!
//! - `/` reads the artifact file and returns it as HTML with the reload
//!   and tracker snippets injected into the response (never into the
//!   file on disk).
//! - `/vendor/<path>` serves the embedded, pre-gzipped library set with
//!   the pinned MIME types and `Content-Encoding: gzip`.
//! - `/ws` is the reload broadcast; `/healthz` is the canvas liveness
//!   marker.
//! - `/reload` is the control request: swap the artifact and
//!   broadcast, for tools that drive the running server directly.
//! - `/metrics` is the page→jinn telemetry sink: the injected tracker
//!   posts interaction events here; they accumulate in
//!   [`ServerShared`] until the session-turn slice drains them.
//!
//! The module has no serve orchestration (that is [`super::canvas_actor`])
//! and no file watching (that is [`super::watcher`]); it is the
//! request/response half of the server plus the shared [`ServerShared`]
//! the spawned server task holds.

use std::collections::VecDeque;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use jiff::Timestamp;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::broadcast;

/// The reload snippet injected into the served artifact's HTML. It opens
/// the WebSocket and reloads on any server-pushed message; nothing else.
pub const RELOAD_SNIPPET: &str = r#"<script>(function(){var ws;function open(){ws=new WebSocket((location.protocol==="https:"?"wss://":"ws://")+location.host+"/ws");ws.onmessage=function(){location.reload();};ws.onclose=function(){setTimeout(open,1000);};}open();})();</script>"#;

/// The telemetry snippet injected next to the reload snippet: loads the
/// authored tracker from `/vendor/`, which reports clicks and
/// `canvas.track` calls to [`METRICS_ROUTE`].
pub const TRACKER_SNIPPET: &str = r#"<script src="/vendor/canvas-telemetry.js" defer></script>"#;

/// The route the tracker posts events to.
pub const METRICS_ROUTE: &str = "/metrics";

/// The accumulation bound on metrics rows: past this the oldest event
/// drops. An unattended page (an agent never came back for) cannot grow
/// the buffer, or the next user message carrying it, without bound.
pub(crate) const METRICS_BUFFER_CAP: usize = 200;

/// One interaction event a served page reported, as received.
///
/// The event body is whatever the page posted — the server validates
/// only that it is a JSON object. Field names are the agent's frontend
/// responsibility; the accumulation and the injected block carry them
/// through verbatim.
#[derive(Debug, Clone)]
pub struct MetricEntry {
    /// The event as posted: at minimum `name`, `at`, `props`.
    pub event: serde_json::Value,
    /// When jinn received it — the page clock and jinn's can disagree,
    /// and this is the ordering the drain renders in.
    pub received_at: Timestamp,
    /// The artifact path being served when the event landed.
    pub artifact: String,
}

impl MetricEntry {
    /// Stamps an event with the receive time and the served artifact.
    #[must_use]
    pub fn new(event: serde_json::Value, artifact: &str) -> Self {
        Self {
            event,
            received_at: Timestamp::now(),
            artifact: artifact.to_owned(),
        }
    }
}

/// State shared across the server's request handlers and the watcher.
#[derive(Clone)]
pub struct ServerShared {
    /// Absolute path of the artifact being served — canonical and spelled
    /// with the platform's classic separators (no Windows verbatim
    /// prefix). Behind a lock because a re-target swaps it while requests
    /// are in flight; the lock is never held across an await.
    pub artifact: Arc<RwLock<MarkedPathBuf<Absolute>>>,
    /// Reload broadcast: the payload is irrelevant (any message means
    /// "reload"); capacity bounds a burst of edits.
    pub reload_tx: broadcast::Sender<()>,

    /// The live directory watcher. Behind the same lock discipline as
    /// `artifact` because a re-target re-arms it: dropping the previous
    /// watcher unregisters its watch, so watchers never accumulate (two
    /// live watchers would broadcast every edit twice, and so on).
    pub watcher: Arc<RwLock<Option<Arc<notify::RecommendedWatcher>>>>,

    /// Interaction events the served page posted to `/metrics`, oldest
    /// first. The canvas actor is the sole writer; the drain is
    /// `std::mem::take` — a consumed buffer stays empty.
    pub metrics: Arc<RwLock<VecDeque<MetricEntry>>>,
}

impl ServerShared {
    /// Reads the currently served artifact path.
    #[must_use = "the read artifact path is the point of the call"]
    pub fn artifact(&self) -> MarkedPathBuf<Absolute> {
        self.artifact.read().clone()
    }

    /// Swaps the served artifact (the re-target path).
    pub fn set_artifact(&self, artifact: MarkedPathBuf<Absolute>) {
        *self.artifact.write() = artifact;
    }

    /// Swaps the live watcher handle (drop old, install new). The one
    /// writer of the slot; watchers are replaced, never accumulated.
    pub fn swap_watcher(&self, watcher: Arc<notify::RecommendedWatcher>) {
        *self.watcher.write() = Some(watcher);
    }

    /// Accumulates one interaction event, dropping the oldest past
    /// [`METRICS_BUFFER_CAP`] so an unattended page stays bounded.
    pub(crate) fn push_metric(&self, entry: MetricEntry) {
        let mut metrics = self.metrics.write();
        if metrics.len() >= METRICS_BUFFER_CAP {
            metrics.pop_front();
        }
        metrics.push_back(entry);
    }

    /// How many browsers currently hold the page: live `/ws` reload
    /// subscriptions. Zero means no served URL has a holder, which is
    /// the pending-bind apply boundary.
    #[must_use]
    pub fn client_count(&self) -> usize {
        self.reload_tx.receiver_count()
    }

    /// Takes every accumulated event, oldest first. A consumed buffer
    /// is empty: each event reaches the agent exactly once.
    #[must_use = "draining is the point; discarding the events loses them"]
    pub fn drain_metrics(&self) -> Vec<MetricEntry> {
        std::mem::take(&mut *self.metrics.write()).into()
    }

    /// A `ServerShared` with a placeholder artifact, for buffer tests:
    /// no file on disk is read through it.
    ///
    /// # Panics
    ///
    /// Panics if `/` cannot be canonicalized, which no supported
    /// platform does — the constructor is test-only for that reason.
    #[cfg(test)]
    #[must_use]
    pub fn for_test() -> Self {
        // "/" exists and is a directory; canonicalization cannot fail.
        let artifact = {
            #[expect(
                clippy::expect_used,
                reason = "test-only constructor: / canonicalizes on every supported platform"
            )]
            let canonical =
                marked_path::CanonicalPath::from_path(std::path::Path::new("/")).expect("/");
            canonical.into_marked()
        };
        let (reload_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            artifact: Arc::new(RwLock::new(artifact)),
            reload_tx,
            watcher: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RwLock::new(VecDeque::new())),
        }
    }
}

/// The snippet string `serve_artifact` injects: reload and tracker in
/// one `</head>`-adjacent insertion.
#[must_use]
pub fn combined_injections() -> String {
    format!("{RELOAD_SNIPPET}{TRACKER_SNIPPET}")
}

/// Builds the full router for a canvas server.
#[must_use = "the router carries every canvas route; building it without serving it is a wiring bug"]
pub fn router(shared: ServerShared) -> Router {
    let shared = Arc::new(shared);
    Router::new()
        .route("/", get(serve_artifact))
        .route("/vendor/{*path}", get(serve_vendor))
        .route("/ws", get(ws_handler))
        .route("/healthz", get(health_handler))
        .route("/reload", get(reload_handler))
        .route(METRICS_ROUTE, post(metrics_handler))
        .with_state(shared)
}

/// Builds the server's shared state for a resolved bind and artifact:
/// the reload broadcast (capacity bounds a burst of edits), the empty
/// watcher slot (the actor arms it before the listener accepts), and
/// the empty metrics buffer (the `/metrics` route fills it).
#[must_use]
pub fn shared_for(artifact: &MarkedPathBuf<Absolute>) -> ServerShared {
    let (reload_tx, _) = tokio::sync::broadcast::channel(64);
    ServerShared {
        artifact: Arc::new(RwLock::new(artifact.clone())),
        reload_tx,
        watcher: Arc::new(RwLock::new(None)),
        metrics: Arc::new(RwLock::new(VecDeque::new())),
    }
}

/// Canonicalizes `path` into the typed, classic-spelled form every
/// artifact consumer reads (file read, watcher parent, status line,
/// outcome) — `CanonicalPath` canonicalizes, proves existence, and strips
/// the Windows verbatim prefix, and `into_marked` hands the pipeline a
/// `MarkedPathBuf<Absolute>`.
///
/// # Errors
///
/// Propagates the [`marked_path::PathError`] — `Io` when the path does not
/// exist or cannot be read, `NotAbsolute` if the resolved path somehow
/// carries no absolute form.
pub(crate) fn canonical_artifact(
    path: &std::path::Path,
) -> Result<MarkedPathBuf<Absolute>, marked_path::PathError> {
    marked_path::CanonicalPath::from_path(path).map(marked_path::CanonicalPath::into_marked)
}

/// `GET /` — the artifact with the reload and tracker snippets
/// injected, read fresh on every request so edits land without a
/// restart.
async fn serve_artifact(State(shared): State<Arc<ServerShared>>) -> Response {
    let path = shared.artifact();
    match std::fs::read(path.as_path()) {
        Ok(bytes) => Html(inject_reload_snippet(&bytes, &combined_injections())).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!(
                "canvas artifact unreadable: {path} ({e})\nfix: recreate the file and run serve again\n",
            ),
        )
            .into_response(),
    }
}

/// Injects the reload snippet before `</head>` when the document has
/// one, else prepends it. Byte-level on the artifact's UTF-8; no parsing.
///
/// Only ever applied to the artifact's HTML response — never to vendor
/// assets, where appended bytes would corrupt the payload.
#[must_use]
pub fn inject_reload_snippet(html: &[u8], snippet: &str) -> String {
    let body = String::from_utf8_lossy(html);
    match body.rfind("</head>") {
        Some(idx) => {
            let mut out = String::with_capacity(body.len() + snippet.len());
            // `</head>` is pure ASCII, so both split points are char
            // boundaries and the slices cannot panic.
            let (before, after) = body.split_at(idx);
            out.push_str(before);
            out.push_str(snippet);
            out.push_str(after);
            out
        }
        None => {
            let mut out = String::with_capacity(body.len() + snippet.len() + 1);
            out.push_str(snippet);
            out.push('\n');
            out.push_str(&body);
            out
        }
    }
}

/// `GET /vendor/<path>` — one embedded, pre-gzipped asset.
async fn serve_vendor(AxPath(path): AxPath<String>) -> Response {
    let Some(asset) = crate::VENDOR_ASSETS.iter().find(|a| a.path == path) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no vendor asset at /{path}\n"),
        )
            .into_response();
    };
    let mut response = (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            ),
            (
                header::CONTENT_ENCODING,
                HeaderValue::from_static(asset.encoding),
            ),
            // Vendored libraries are plain scripts and fonts, but ES
            // module imports and font loads from a `file://` page or a
            // foreign origin need the CORS blessing; `*` is safe here
            // because the assets carry no credentials and the server has
            // no session state to expose.
            (
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            ),
        ],
    )
        .into_response();
    // The bytes are already gzipped on disk; forward the compressed form
    // verbatim and let the Content-Length speak for the payload.
    {
        use axum::body::Body as AxBody;
        *response.body_mut() = AxBody::from(byteslice(asset.bytes));
    }
    response
}

/// Re-wraps a vendored byte slice for the response body.
fn byteslice(bytes: &'static [u8]) -> Vec<u8> {
    bytes.to_vec()
}

/// `POST /metrics` — the tracker's sink. Accepts any JSON object and
/// accumulates it, stamped with the receive time and the served
/// artifact; the session-turn drain delivers the lot to the agent with
/// the next user message. No schema, no field whitelist: the field
/// names are the agent's frontend responsibility.
///
/// Anything that is not a JSON object (bad JSON, a bare scalar, an
/// array) is refused with a one-line reason and accumulates nothing.
async fn metrics_handler(State(shared): State<Arc<ServerShared>>, body: String) -> Response {
    let event: serde_json::Value = match serde_json::from_str(&body) {
        Ok(event) => event,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("metrics body must be one JSON object ({e})\n"),
            )
                .into_response();
        }
    };
    if !event.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            "metrics body must be one JSON object (got a non-object JSON value)\n",
        )
            .into_response();
    }
    shared.push_metric(MetricEntry::new(
        event,
        &shared.artifact().to_string_lossy(),
    ));
    (StatusCode::OK, "metric accepted\n").into_response()
}

/// `GET /ws` — upgrade and subscribe to the reload broadcast.
async fn ws_handler(ws: WebSocketUpgrade, State(shared): State<Arc<ServerShared>>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// The per-connection WebSocket loop: relay every broadcast to the
/// browser; outbound only — the client never sends anything the server
/// needs. A dropped connection just ends the task.
async fn ws_loop(socket: WebSocket, shared: Arc<ServerShared>) {
    use futures::{SinkExt as _, StreamExt as _};

    let mut rx = shared.reload_tx.subscribe();
    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            broadcast = rx.recv() => {
                // Lagged or closed, either ends the relay: lag still means
                // "things changed" but the next reload arrives on its own.
                if broadcast.is_err() {
                    break;
                }
                if sender.send(Message::Text("reload".into())).await.is_err() {
                    break;
                }
            }
            inbound = receiver.next() => {
                // The socket closed (or errored): the task is done.
                if inbound.is_none() {
                    break;
                }
            }
        }
    }
}

/// `GET /healthz` — the canvas-specific liveness marker. Any 200 whose
/// body starts with [`HEALTH_MARKER`] identifies a live canvas server.
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, format!("{HEALTH_MARKER}\n"))
}

/// The `/reload` control request's optional artifact query parameter:
/// present on a re-target, absent on a plain refresh.
#[derive(Debug, Deserialize)]
struct ReloadQuery {
    /// The artifact to switch to before broadcasting.
    artifact: Option<String>,
}

/// `GET /reload` — the control request from `serve` (re-target) and
/// `refresh`: optionally swap the artifact, then broadcast a reload to
/// every connected browser.
///
/// This endpoint is the control plane: whoever can reach the bind can
/// drive the server, so securing the interface it listens on is the
/// operator's call, made through the `[canvas] bind` config.
async fn reload_handler(
    Query(query): Query<ReloadQuery>,
    State(shared): State<Arc<ServerShared>>,
) -> Response {
    if let Some(artifact) = query.artifact {
        if !std::path::Path::new(&artifact).is_file() {
            return (
                StatusCode::NOT_FOUND,
                format!("re-target artifact missing: {artifact}\n"),
            )
                .into_response();
        }
        // The swap stores the canonical, classic-spelled path — the same
        // form the serve path stores — so every reader (file read, watcher
        // parent, status line) sees one spelling.
        let canonical = match canonical_artifact(std::path::Path::new(&artifact)) {
            Ok(canonical) => canonical,
            Err(e) => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("re-target artifact unreadable: {artifact} ({e})\n"),
                )
                    .into_response();
            }
        };
        shared.set_artifact(canonical.clone());

        // Re-arm the watcher onto the new file's directory before the
        // broadcast, so the post-swap edit cycle is watched from the
        // first change onward. The old watcher drops with the swap:
        // watchers must be replaced, never accumulated, or every edit
        // broadcasts once per accumulated watcher.
        if let Some(dir) = canonical.parent() {
            match crate::watcher::establish(dir.as_path(), shared.reload_tx.clone()) {
                Ok(watcher) => {
                    shared.swap_watcher(watcher);
                }
                Err(report) => {
                    tracing::warn!("re-target watcher re-arm failed: {report}");
                }
            }
        }
    }
    // A lost broadcast receiver is fine: no browser is connected.
    let _ = shared.reload_tx.send(());
    (StatusCode::OK, "reload broadcast\n").into_response()
}

/// The health body prefix a live canvas server answers with.
pub const HEALTH_MARKER: &str = "jinn-canvas";

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "test assertions need expect/unwrap and direct indices"
    )]

    use std::sync::Arc;

    use axum::extract::State;

    use super::{
        METRICS_BUFFER_CAP, MetricEntry, RELOAD_SNIPPET, ServerShared, TRACKER_SNIPPET,
        combined_injections, inject_reload_snippet, metrics_handler, shared_for,
    };
    use marked_path::MarkedPathAccess as _;

    /// Builds `ServerShared` for a temp-dir artifact (canonical, on disk
    /// at build time — the handler reads the path, never the file).
    fn shared_for_temp_artifact(name: &str) -> ServerShared {
        let dir = jinn_testutil::tempdir();
        let artifact = dir.path.join(name).expect("name join");
        std::fs::write(artifact.as_path(), "<html></html>").expect("seed artifact");
        let canonical =
            marked_path::CanonicalPath::from_path(artifact.as_path()).expect("canonical");
        shared_for(&canonical.into_marked())
    }

    #[rstest::rstest]
    #[test]
    fn snippet_is_injected_before_closing_head() {
        // Given an artifact document with a head.
        let html = b"<html><head><title>t</title></head><body></body></html>";

        // When injecting the snippet.
        let out = inject_reload_snippet(html, RELOAD_SNIPPET);

        // Then the snippet sits immediately before `</head>`.
        let idx = out.find(RELOAD_SNIPPET).expect("snippet present");
        let (_, after_snippet) = out.split_at(idx + RELOAD_SNIPPET.len());
        assert!(after_snippet.starts_with("</head>"));
    }

    #[rstest::rstest]
    #[test]
    fn document_without_head_gets_the_snippet_prepended() {
        // Given a fragment with no `</head>`.
        let html = b"<div>hi</div>";

        // When injecting.
        let out = inject_reload_snippet(html, RELOAD_SNIPPET);

        // Then the snippet leads the document and the fragment follows.
        assert!(out.starts_with(RELOAD_SNIPPET));
        assert!(out.ends_with("<div>hi</div>"));
    }

    #[rstest::rstest]
    #[test]
    fn injection_is_response_only_bytes_on_disk_pass_through() {
        // Given an artifact's exact bytes.
        let html: &[u8] = b"<html><head></head><body>x</body></html>";

        // When injecting into a response view of those bytes.
        let out = inject_reload_snippet(html, RELOAD_SNIPPET);

        // Then removing the snippet reproduces the file byte for byte —
        // the server adds to the response, alters nothing else.
        let stripped = out.replace(RELOAD_SNIPPET, "");
        assert_eq!(stripped, String::from_utf8_lossy(html));
    }

    #[rstest::rstest]
    #[test]
    fn served_response_carries_tracker_and_reload_snippet() {
        // Given the combined injection string.
        let combined = combined_injections();

        // Then it carries both snippets, reload first (the reload loop
        // must never wait on the tracker's fetch).
        assert!(
            combined.contains(RELOAD_SNIPPET) && combined.contains(TRACKER_SNIPPET),
            "{combined}"
        );
        assert!(
            combined.find(RELOAD_SNIPPET).expect("reload present")
                < combined.find(TRACKER_SNIPPET).expect("tracker present")
        );
    }

    #[rstest::rstest]
    #[test]
    fn stripping_both_injections_reproduces_the_file_bytes() {
        // Given an artifact's exact bytes.
        let html: &[u8] = b"<html><head><title>t</title></head><body>y</body></html>";

        // When injecting the combined string into a response view.
        let out = inject_reload_snippet(html, &combined_injections());

        // Then stripping both snippets reproduces the file byte for byte
        // — the served response is the file plus injections, nothing
        // else, and the file on disk is untouched.
        let stripped = out.replace(TRACKER_SNIPPET, "").replace(RELOAD_SNIPPET, "");
        assert_eq!(stripped, String::from_utf8_lossy(html));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn metrics_post_accepts_arbitrary_json_object() {
        // Given a live ServerShared and an event with fields the server
        // has never heard of.
        let shared = shared_for_temp_artifact("chart.html");
        let body =
            r#"{"name":"quiz.answer","at":1730000000000,"props":{"choice":"b"},"anything":42}"#;

        // When posting it to the metrics handler.
        let response = metrics_handler(State(Arc::new(shared.clone())), body.to_owned()).await;

        // Then it is accepted, and the accumulated row carries the event
        // verbatim plus the receive stamp and the served artifact.
        assert_eq!(response.status(), 200);
        let drained = shared.drain_metrics();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].event["anything"], 42);
        assert_eq!(drained[0].event["props"]["choice"], "b");
        assert!(
            drained[0].artifact.ends_with("chart.html"),
            "{:?}",
            drained[0].artifact
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn metrics_post_rejects_non_json_body() {
        // Given a live ServerShared and a body that is not JSON.
        let shared = shared_for_temp_artifact("chart.html");

        // When posting it.
        let response =
            metrics_handler(State(Arc::new(shared.clone())), "not json".to_owned()).await;

        // Then the post is refused and nothing accumulated.
        assert_eq!(response.status(), 400);
        assert!(shared.drain_metrics().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn metrics_post_rejects_non_object_json() {
        // Given a live ServerShared and a valid JSON array.
        let shared = shared_for_temp_artifact("chart.html");

        // When posting it.
        let response = metrics_handler(State(Arc::new(shared.clone())), "[1,2]".to_owned()).await;

        // Then the post is refused and nothing accumulated — the sink
        // takes objects only, so the rendered lines stay one event each.
        assert_eq!(response.status(), 400);
        assert!(shared.drain_metrics().is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn metrics_accumulation_drops_oldest_at_cap() {
        // Given a ServerShared pushed past the cap.
        let shared = ServerShared::for_test();
        for i in 0..(METRICS_BUFFER_CAP + 5) {
            shared.push_metric(MetricEntry::new(
                serde_json::json!({ "name": format!("event-{i}") }),
                "/tmp/chart.html",
            ));
        }

        // When draining.
        let drained = shared.drain_metrics();

        // Then the buffer bounded at the cap, keeping the newest events —
        // the first five (the oldest) dropped off the front.
        assert_eq!(drained.len(), METRICS_BUFFER_CAP);
        assert_eq!(drained[0].event["name"], "event-5");
        assert_eq!(
            drained[drained.len() - 1].event["name"],
            format!("event-{}", METRICS_BUFFER_CAP + 4)
        );
    }

    #[rstest::rstest]
    #[test]
    fn drain_metrics_empties_the_buffer() {
        // Given a ServerShared holding two events.
        let shared = ServerShared::for_test();
        shared.push_metric(MetricEntry::new(serde_json::json!({ "name": "a" }), "x"));
        shared.push_metric(MetricEntry::new(serde_json::json!({ "name": "b" }), "x"));

        // When draining twice.
        let first = shared.drain_metrics();
        let second = shared.drain_metrics();

        // Then the first drain carries both, in order, and the second is
        // empty — every event reaches the agent exactly once.
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].event["name"], "a");
        assert_eq!(first[1].event["name"], "b");
        assert!(second.is_empty());
    }
}
