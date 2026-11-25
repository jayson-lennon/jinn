//! The HTTP surface of `/web2`: the page shell, the client bundle, and the
//! one WebSocket every message rides.
//!
//! There are exactly four routes and no others: the page, the vendored
//! client assets, a health probe, and the socket upgrade. The server
//! renders no HTML beyond the shell and answers no action with a page —
//! every fact crosses the socket as a `ServerMsg`.

use std::sync::Arc;

use axum::{
    Router,
    extract::{
        State as AxumState,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use jinn_kernel::State;
use jinn_webui2_msg::{ClientMsg, ServerMsg};

use crate::{outbound::OutboundHandle, protocol};

/// What every handler reads.
#[derive(Debug, Clone)]
pub struct ServerShared {
    /// The shared application state.
    pub state: State,
    /// The outbound actor's connections and revisions.
    pub outbound: OutboundHandle,
    /// The serving binary's version.
    pub version: String,
}

/// The page shell, as committed under `templates/`.
const PAGE_SHELL: &str = include_str!("../templates/page.html");

/// The vendored client files, embedded at compile time from the build
/// script's inventory: `(path, bytes, content type)`.
static VENDOR: &[(&str, &[u8], &str)] = &[];

/// Builds the router.
#[must_use]
pub fn router(shared: Arc<ServerShared>) -> Router {
    Router::new()
        .route("/web2", get(page))
        .route("/web2/ws", get(ws_handler))
        .route("/vendor2/{*path}", get(vendor))
        .route("/health2", get(health))
        .with_state(shared)
}

async fn page() -> Html<&'static str> {
    Html(PAGE_SHELL)
}

async fn health(AxumState(shared): AxumState<Arc<ServerShared>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        format!("{{\"version\":\"{}\"}}", shared.version),
    )
        .into_response()
}

async fn vendor(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches("/vendor2/");
    let Some((_, bytes, content_type)) = VENDOR.iter().find(|(p, _, _)| *p == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    ([(header::CONTENT_TYPE, *content_type)], *bytes).into_response()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    AxumState(shared): AxumState<Arc<ServerShared>>,
) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// One connection's life: dispatch every `ClientMsg` the page sends and
/// write every frame back — the replies and the pushes alike — until the
/// socket closes.
///
/// Replies to a message go out ahead of anything the tick flushes, because
/// the dispatch writes them straight to the socket while pushed frames
/// arrive on the connection's channel.
async fn ws_loop(mut socket: WebSocket, shared: Arc<ServerShared>) {
    let mut pushed = shared.outbound.register();

    loop {
        tokio::select! {
            inbound = socket.recv() => {
                let Some(Ok(message)) = inbound else {
                    break;
                };
                let Message::Text(text) = message else {
                    continue;
                };
                let reply = match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(msg) => protocol::dispatch(msg, &shared.outbound).await,
                    Err(_) => crate::actions::malformed(),
                };
                for frame in reply.frames {
                    if write_frame(&mut socket, &frame).await.is_err() {
                        return;
                    }
                }
            }
            frame = pushed.recv() => {
                let Some(frame) = frame else {
                    break;
                };
                if write_frame(&mut socket, &frame).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn write_frame(socket: &mut WebSocket, frame: &ServerMsg) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}
