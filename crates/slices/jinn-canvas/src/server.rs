//! The axum listener — everything a running canvas server does.
//!
//! Surfaces:
//!
//! - `/` reads the artifact file and returns it as HTML with the reload
//!   snippet injected into the response (never into the file on disk).
//! - `/vendor/<path>` serves the embedded, pre-gzipped library set with
//!   the pinned MIME types and `Content-Encoding: gzip`.
//! - `/ws` is the reload broadcast; `/healthz` is the canvas liveness
//!   marker.
//! - `/reload` is the loopback control request: swap the artifact and
//!   broadcast, for tools that drive the running server directly.
//!
//! The module has no serve orchestration (that is [`super::canvas_actor`])
//! and no file watching (that is [`super::watcher`]); it is the
//! request/response half of the server plus the shared [`ServerShared`]
//! the spawned server task holds.

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::broadcast;

/// The reload snippet injected into the served artifact's HTML. It opens
/// the WebSocket and reloads on any server-pushed message; nothing else.
pub const RELOAD_SNIPPET: &str = r#"<script>(function(){var ws;function open(){ws=new WebSocket((location.protocol==="https:"?"wss://":"ws://")+location.host+"/ws");ws.onmessage=function(){location.reload();};ws.onclose=function(){setTimeout(open,1000);};}open();})();</script>"#;

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
        .with_state(shared)
}

/// Builds the server's shared state for a resolved bind and artifact:
/// the reload broadcast (capacity bounds a burst of edits), the empty
/// watcher slot (the actor arms it before the listener accepts).
#[must_use]
pub fn shared_for(artifact: &MarkedPathBuf<Absolute>) -> ServerShared {
    let (reload_tx, _) = tokio::sync::broadcast::channel(64);
    ServerShared {
        artifact: Arc::new(RwLock::new(artifact.clone())),
        reload_tx,
        watcher: Arc::new(RwLock::new(None)),
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

/// `GET /` — the artifact with the reload snippet injected, read fresh
/// on every request so edits land without a restart.
async fn serve_artifact(State(shared): State<Arc<ServerShared>>) -> Response {
    let path = shared.artifact();
    match std::fs::read(path.as_path()) {
        Ok(bytes) => Html(inject_reload_snippet(&bytes, RELOAD_SNIPPET)).into_response(),
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
            // foreign origin need the CORS blessing; `*` is safe on a
            // loopback server with no credentials.
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
/// This endpoint is the loopback control plane; the server binds a
/// loopback address only, so nothing off-machine can reach it.
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
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{RELOAD_SNIPPET, inject_reload_snippet};

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
}
