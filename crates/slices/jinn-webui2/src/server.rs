//! The HTTP surface of `/web2`: the page shell, the client bundle, and the
//! one WebSocket every message rides.
//!
//! There are exactly five routes and no others: the page, the vendored
//! client assets, a health probe, one attachment's bytes, and the socket
//! upgrade. The server renders no HTML beyond the shell and answers no
//! action with a page — every fact crosses the socket as a `ServerMsg`.
//! The one exception is the attachment route: a transcript's `<img>` needs
//! an address to fetch, and the bytes it returns are the entry's own.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{
        Path, State as AxumState,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use jinn_webui2_msg::{ClientMsg, ServerMsg};

use crate::{outbound::OutboundHandle, protocol};

/// What every handler reads.
#[derive(Debug, Clone)]
pub struct ServerShared {
    /// The outbound actor's state, connections and revisions: the one
    /// handle every request is dispatched through.
    pub outbound: OutboundHandle,
    /// The serving binary's version.
    pub version: String,
}

/// The page shell, as committed under `templates/`.
const PAGE_SHELL: &str = include_str!("../templates/page.html");

/// The client bundle, embedded at compile time from the build script's
/// inventory: one row per built file, gzip-committed and sha256-pinned.
mod vendor_assets {
    include!(concat!(env!("OUT_DIR"), "/vendor_assets.rs"));
}

/// Long-lived for an identical bundle; the pin changes whenever it does.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";

/// Builds the router.
pub fn router(shared: Arc<ServerShared>) -> Router {
    Router::new()
        .route("/web2", get(page))
        .route("/web2/ws", get(ws_handler))
        .route("/vendor2/{*path}", get(vendor))
        .route(
            "/web2/attachment/{session}/{entry}/{ordinal}",
            get(attachment),
        )
        .route("/health2", get(health))
        .with_state(shared)
}

async fn page() -> Response {
    // The shell names the bundle by a fixed path, so it must always be
    // revalidated; the bundle itself is immutable for as long as its pin.
    ([(header::CACHE_CONTROL, "no-cache")], Html(PAGE_SHELL)).into_response()
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
    let Some(asset) = vendor_assets::VENDOR_ASSETS
        .iter()
        .find(|asset| asset.path == path)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, asset.content_type),
            (header::CONTENT_ENCODING, "gzip"),
            (header::CACHE_CONTROL, IMMUTABLE_CACHE),
        ],
        asset.bytes,
    )
        .into_response()
}

/// The bounded wait for a live attachment — a just-sent entry lands within
/// the dispatch latency; the budget only binds on a wedged bus, and then
/// the store read below is the answer.
const ATTACHMENT_CONVERGENCE_BUDGET: Duration = Duration::from_millis(2_000);

/// How often live state is re-read while waiting.
const ATTACHMENT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// `GET /web2/attachment/{session}/{entry}/{ordinal}` — one attachment's
/// bytes, for the transcript's `<img>`.
///
/// Resolution order is the original web UI's: live state first (a bounded
/// wait — a just-sent entry may still be crossing the bus), then the
/// session store's persisted entry for a session no longer in memory, then
/// 404. The response is immutable: entry ids never reuse and an
/// attachment's bytes never change once attached, so the browser caches
/// it forever and never re-fetches.
async fn attachment(
    AxumState(shared): AxumState<Arc<ServerShared>>,
    Path((session_raw, entry_raw, ordinal_raw)): Path<(String, String, String)>,
) -> Response {
    let Some(session_id) = jinn_core_types::SessionId::try_from_string(&session_raw) else {
        return plain(StatusCode::BAD_REQUEST, "session id is not a UUID");
    };
    let Some(entry_id) = jinn_core_types::ChatEntryId::try_from_string(&entry_raw) else {
        return plain(StatusCode::BAD_REQUEST, "entry id is not a UUID");
    };
    let Ok(ordinal) = ordinal_raw.parse::<usize>() else {
        return plain(StatusCode::BAD_REQUEST, "ordinal is not a number");
    };

    let state = shared.outbound.state();
    // Live state, bounded. The read guard is taken, read, and dropped
    // before each sleep — never held across the await.
    let deadline = tokio::time::Instant::now() + ATTACHMENT_CONVERGENCE_BUDGET;
    loop {
        let found = {
            let guard = state.read();
            guard.session.get(&session_id).and_then(|session| {
                session
                    .history()
                    .iter()
                    .find_map(|entry| attachment_at(entry, &entry_id, ordinal))
            })
        };
        match found {
            Some(attachment) => return image_response(&attachment),
            None if tokio::time::Instant::now() >= deadline => break,
            None => tokio::time::sleep(ATTACHMENT_POLL_INTERVAL).await,
        }
    }

    // Archived: the store's hydrated snapshot carries the attachments.
    let from_store = shared
        .outbound
        .session_store()
        .load_session(&session_id)
        .await
        .ok()
        .flatten()
        .and_then(|snapshot| {
            snapshot
                .entries
                .iter()
                .find_map(|entry| attachment_at(entry, &entry_id, ordinal))
        });
    match from_store {
        Some(attachment) => image_response(&attachment),
        None => plain(StatusCode::NOT_FOUND, "no such attachment"),
    }
}

/// The `ordinal`-th attachment of a user entry, when the entry is one.
fn attachment_at(
    entry: &jinn_core_types::ChatEntry,
    entry_id: &jinn_core_types::ChatEntryId,
    ordinal: usize,
) -> Option<jinn_core_types::attachment::Attachment> {
    if entry.id != *entry_id {
        return None;
    }
    let jinn_core_types::ChatEntryKind::User { attachments, .. } = &entry.kind else {
        return None;
    };
    attachments.get(ordinal).cloned()
}

/// An attachment's bytes as an immutable, cacheable image response.
fn image_response(attachment: &jinn_core_types::attachment::Attachment) -> Response {
    (
        [
            (header::CONTENT_TYPE, attachment.media_type().to_owned()),
            (header::CACHE_CONTROL, IMMUTABLE_CACHE.to_owned()),
        ],
        attachment.data().to_vec(),
    )
        .into_response()
}

/// A plain-text status answer, for the route's refusals.
fn plain(status: StatusCode, message: &'static str) -> Response {
    (status, message).into_response()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    AxumState(shared): AxumState<Arc<ServerShared>>,
) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// How often an idle connection is sent a heartbeat.
///
/// An order of magnitude under the page's liveness timeout, so a healthy
/// connection never trips it and a dead one is noticed within two beats.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// One connection's life: dispatch every `ClientMsg` the page sends and
/// write every frame back — the replies and the pushes alike — until the
/// socket closes.
///
/// Replies to a message go out ahead of anything the tick flushes, because
/// the dispatch writes them straight to the socket while pushed frames
/// arrive on the connection's channel. An idle connection still sends a
/// heartbeat on a fixed interval, so the page can tell a live connection
/// from one the network dropped without closing the socket.
async fn ws_loop(mut socket: WebSocket, shared: Arc<ServerShared>) {
    let mut pushed = shared.outbound.register();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    heartbeat.reset();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if write_frame(&mut socket, &ServerMsg::Ping).await.is_err() {
                    return;
                }
            }
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

#[cfg(test)]
mod attachment_route_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;
    use axum::extract::{Path, State as AxumState};

    /// A shared server over a harness-shaped handle: a fresh state on a
    /// recording bus, nothing spawned.
    fn shared() -> Arc<ServerShared> {
        let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
        let slices = jinn_slices::Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let app_state = jinn_kernel::AppState::default();
        app_state.frontend.attach_slices(slices.clone());
        app_state.session.attach_slices(slices.clone());
        let state = jinn_kernel::State::new(app_state);
        let provider_cell = slices
            .reader::<jinn_provider_selection_msg::ProviderCell>(
                &jinn_provider_selection_msg::provider_state_slot(),
            )
            .expect("the catalog registers the provider slot");
        let personas_cell = slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("the catalog registers the persona slot");
        let mcp_runtime_cell = slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("the catalog registers the mcp runtime slot");
        let services = jinn_kernel::TestServices::builder()
            .providers(jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            })
            .build();
        Arc::new(ServerShared {
            outbound: OutboundHandle::new(crate::outbound::OutboundActorDeps {
                state,
                bus,
                connections: crate::connections::ConnectionRegistry::default(),
                revisions: crate::revision::RevisionTracker::default(),
                session_store: jinn_session_state::SessionStoreService::new(Arc::new(
                    jinn_kernel::common::services::test_services::FakeSessionStore,
                )),
                config: services.config.clone(),
                services,
                provider_cell,
                personas_cell,
                mcp_runtime_cell,
            }),
            version: "test".to_owned(),
        })
    }

    /// A user entry carrying one PNG, pushed onto the active session.
    fn session_with_one_image(
        shared: &ServerShared,
    ) -> (jinn_core_types::SessionId, jinn_core_types::ChatEntryId) {
        let entry = jinn_core_types::ChatEntry::user_with_attachments(
            "look",
            vec![jinn_core_types::attachment::Attachment::image(
                "image/png".to_owned(),
                vec![0x89, 0x50, 0x4e, 0x47],
            )],
        );
        let id = entry.id.clone();
        let state = shared.outbound.state();
        let session_id = state.read().session.active_session_id().clone();
        state.write().session.active_session_mut().push_entry(entry);
        (session_id, id)
    }

    async fn get(
        shared: &Arc<ServerShared>,
        session: &str,
        entry: &str,
        ordinal: &str,
    ) -> Response {
        attachment(
            AxumState(shared.clone()),
            Path((session.to_owned(), entry.to_owned(), ordinal.to_owned())),
        )
        .await
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn attachment_route_serves_a_live_entries_bytes_with_its_media_type() {
        // Given an active session whose user entry carries one PNG.
        let shared = shared();
        let (session_id, entry_id) = session_with_one_image(&shared);

        // When the browser asks for it.
        let response = get(&shared, &session_id.to_string(), &entry_id.to_string(), "0").await;

        // Then the bytes come back as the attachment's own media type.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn attachment_route_answers_404_for_an_unknown_entry() {
        // Given an active session with no attachments.
        let shared = shared();
        let session_id = shared
            .outbound
            .state()
            .read()
            .session
            .active_session_id()
            .clone();

        // When the browser asks for an entry the session does not hold.
        let response = get(
            &shared,
            &session_id.to_string(),
            &jinn_core_types::ChatEntryId::new().to_string(),
            "0",
        )
        .await;

        // Then the answer is 404.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn attachment_route_answers_400_for_a_malformed_session_id() {
        // Given a server.
        let shared = shared();

        // When the session segment is not a UUID.
        let response = get(
            &shared,
            "not-a-uuid",
            &jinn_core_types::ChatEntryId::new().to_string(),
            "0",
        )
        .await;

        // Then the answer is 400, before any lookup.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn attachment_route_answers_400_for_a_malformed_ordinal() {
        // Given an active session whose user entry carries one PNG.
        let shared = shared();
        let (session_id, entry_id) = session_with_one_image(&shared);

        // When the ordinal segment is not a number.
        let response = get(
            &shared,
            &session_id.to_string(),
            &entry_id.to_string(),
            "first",
        )
        .await;

        // Then the answer is 400.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[rstest::rstest]
    #[test]
    fn attachment_at_returns_none_for_an_ordinal_past_the_end() {
        // Given a user entry carrying one attachment.
        let entry = jinn_core_types::ChatEntry::user_with_attachments(
            "look",
            vec![jinn_core_types::attachment::Attachment::image(
                "image/png".to_owned(),
                vec![0x89],
            )],
        );

        // When asked for the second one.
        let found = attachment_at(&entry, &entry.id.clone(), 1);

        // Then there is none.
        assert!(found.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn attachment_at_returns_none_for_a_non_user_entry() {
        // Given an assistant entry, which carries no attachments.
        let entry = jinn_core_types::ChatEntry::assistant("hello");

        // When asked for an attachment of it.
        let found = attachment_at(&entry, &entry.id.clone(), 0);

        // Then there is none, whatever the ordinal.
        assert!(found.is_none());
    }
}
