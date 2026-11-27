//! The HTTP surface of the web UI: the page shell, the client bundle, and
//! the one WebSocket every message rides.
//!
//! The page is served at the root of the listener. There are exactly six
//! routes and no others: the page, the vendored client assets, a health
//! probe, one attachment's bytes, one tool result's full output, and the
//! socket upgrade. The server renders no HTML beyond the shell and
//! answers no action with a page — every fact crosses the socket as a
//! `ServerMsg`. The two exceptions are the on-demand routes: a
//! transcript's `<img>` needs an address to fetch, and a tool result's
//! full untruncated output is too heavy to ride every snapshot, so the
//! page fetches it when its disclosure opens.

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

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
use jinn_core_types::{ChatEntry, ChatEntryId, ChatEntryKind, SessionId};
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
    /// The dashboard row's health: told when a page connects, a `hello`
    /// is answered, or a write stalls.
    pub health: crate::health::WebUi2Health,
}

/// The page shell, as committed under `templates/`.
const PAGE_SHELL_TEMPLATE: &str = include_str!("../templates/page.html");

/// The client bundle, embedded at compile time from the build script's
/// inventory: one row per built file, gzip-compressed by the build.
mod vendor_assets {
    include!(concat!(env!("OUT_DIR"), "/vendor_assets.rs"));
}

/// Long-lived for an identical bundle; the pin changes whenever it does.
/// True only because the shell names each asset by that pin: a changed
/// bundle is a different URL, so a cached one is never reused across it.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";

/// The page shell, rendered once: its asset URLs carry the pinned hashes.
static PAGE_SHELL: LazyLock<String> = LazyLock::new(render_page_shell);

/// The shell with every asset named by `name.<sha256>.ext`, so a rebuilt
/// bundle — a new hash — is a new URL and no cached copy can outlive it.
///
/// An asset the bundle does not carry is named unhashed, which the vendor
/// route refuses: a broken bundle is a visible 404, never a stale copy.
fn render_page_shell() -> String {
    PAGE_SHELL_TEMPLATE
        .replace("{{ js_url }}", &hashed_vendor_url("client2.js"))
        .replace("{{ css_url }}", &hashed_vendor_url("client2.css"))
}

/// `client2.js` as the hashed path the shell names it by, or its plain
/// name when the bundle does not carry it.
fn hashed_vendor_url(served: &str) -> String {
    let Some(sha) = pinned_sha(served) else {
        return served.to_owned();
    };
    match served.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}.{sha}.{ext}"),
        None => served.to_owned(),
    }
}

/// An asset's sha256, as the manifest pinned it.
fn pinned_sha(served: &str) -> Option<&'static str> {
    vendor_assets::VENDOR_ASSETS
        .iter()
        .find(|asset| asset.path == served)
        .map(|asset| asset.sha256)
}

/// Splits a hashed vendor path back into the served name and the hash it
/// claims: `client2.<sha>.js` → (`client2.js`, `<sha>`).
fn split_hashed_vendor_path(path: &str) -> Option<(String, &str)> {
    let (stem, ext) = path.rsplit_once('.')?;
    let (name, sha) = stem.rsplit_once('.')?;
    Some((format!("{name}.{ext}"), sha))
}

/// Builds the router: the page at the root, the socket and the two
/// on-demand routes at the top level, the assets and the health probe
/// beside them.
pub fn router(shared: Arc<ServerShared>) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/ws", get(ws_handler))
        .route("/vendor/{*path}", get(vendor))
        .route("/attachment/{session}/{entry}/{ordinal}", get(attachment))
        .route("/tool-output/{session}/{entry}", get(tool_output))
        .route("/health", get(health))
        .with_state(shared)
}

async fn page() -> Response {
    // The shell is the one document that names the bundle, so it is always
    // revalidated; the bundle it names is immutable for as long as its hash.
    (
        [(header::CACHE_CONTROL, "no-cache")],
        Html(PAGE_SHELL.as_str()),
    )
        .into_response()
}

async fn health(AxumState(shared): AxumState<Arc<ServerShared>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        format!("{{\"version\":\"{}\"}}", shared.version),
    )
        .into_response()
}

async fn vendor(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches("/vendor/");
    // Only the hashed form is served: a path naming an asset without its
    // hash is a shell from before this bundle, or a hand-typed URL, and
    // answering it would hand a cached-stale page the bytes it expects.
    let Some((served, sha)) = split_hashed_vendor_path(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(asset) = vendor_assets::VENDOR_ASSETS
        .iter()
        .find(|asset| asset.path == served && asset.sha256 == sha)
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

/// The two ids every on-demand route parses, if both are well-formed.
///
/// Each route names the failure in its own 400; the ids either both parse
/// or they do not.
fn parse_ids(session_raw: &str, entry_raw: &str) -> Option<(SessionId, ChatEntryId)> {
    let session_id = SessionId::try_from_string(session_raw)?;
    let entry_id = ChatEntryId::try_from_string(entry_raw)?;
    Some((session_id, entry_id))
}

/// Resolves one entry's fact from live state first (a bounded wait, since
/// a just-sent entry may still be crossing the bus) and then from the
/// session store, for a session no longer in memory.
///
/// `find` reads the fact off an entry. It returns `None` for any entry
/// that is not the one asked for or does not carry the fact. The read
/// guard is taken, read, and dropped before each sleep, never held across
/// the await.
async fn resolve_entry_fact<T>(
    shared: &ServerShared,
    session_id: &SessionId,
    find: impl Fn(&ChatEntry) -> Option<T>,
) -> Option<T>
where
    T: Clone,
{
    let state = shared.outbound.state();
    let deadline = tokio::time::Instant::now() + ATTACHMENT_CONVERGENCE_BUDGET;
    loop {
        let found = {
            let guard = state.read();
            guard
                .session
                .get(session_id)
                .and_then(|session| session.history().iter().find_map(&find))
        };
        match found {
            Some(fact) => return Some(fact),
            None if tokio::time::Instant::now() >= deadline => break,
            None => tokio::time::sleep(ATTACHMENT_POLL_INTERVAL).await,
        }
    }

    // Archived: the store's hydrated snapshot carries the entries whole.
    shared
        .outbound
        .session_store()
        .load_session(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|snapshot| snapshot.entries.iter().find_map(&find))
}

/// `GET /attachment/{session}/{entry}/{ordinal}` — one attachment's
/// bytes, for the transcript's `<img>`.
///
/// Resolution order: live state first (a bounded
/// wait — a just-sent entry may still be crossing the bus), then the
/// session store's persisted entry for a session no longer in memory, then
/// 404. The response is immutable: entry ids never reuse and an
/// attachment's bytes never change once attached, so the browser caches
/// it forever and never re-fetches.
async fn attachment(
    AxumState(shared): AxumState<Arc<ServerShared>>,
    Path((session_raw, entry_raw, ordinal_raw)): Path<(String, String, String)>,
) -> Response {
    let Some((session_id, entry_id)) = parse_ids(&session_raw, &entry_raw) else {
        return plain(StatusCode::BAD_REQUEST, "session or entry id is not a UUID");
    };
    let Ok(ordinal) = ordinal_raw.parse::<usize>() else {
        return plain(StatusCode::BAD_REQUEST, "ordinal is not a number");
    };

    let found = resolve_entry_fact(&shared, &session_id, |entry| {
        attachment_at(entry, &entry_id, ordinal)
    })
    .await;
    match found {
        Some(attachment) => image_response(&attachment),
        None => plain(StatusCode::NOT_FOUND, "no such attachment"),
    }
}

/// `GET /tool-output/{session}/{entry}` — one tool result's full,
/// untruncated output, for the entry's "full output" disclosure.
///
/// The full output is the heaviest field a tool result carries, so it does
/// not ride the socket: the page fetches it here, when the disclosure
/// opens. Resolved like an attachment's bytes. Never cached: the route may
/// answer a session that is not in memory, and `no-store` keeps a page
/// from pinning an answer it should re-ask for.
async fn tool_output(
    AxumState(shared): AxumState<Arc<ServerShared>>,
    Path((session_raw, entry_raw)): Path<(String, String)>,
) -> Response {
    let Some((session_id, entry_id)) = parse_ids(&session_raw, &entry_raw) else {
        return plain(StatusCode::BAD_REQUEST, "session or entry id is not a UUID");
    };

    let found = resolve_entry_fact(&shared, &session_id, |entry| {
        full_output_at(entry, &entry_id)
    })
    .await;
    match found {
        Some(output) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            output,
        )
            .into_response(),
        None => plain(StatusCode::NOT_FOUND, "no such full output"),
    }
}

/// The full output of a tool result, when the entry is one that carries it.
fn full_output_at(entry: &ChatEntry, entry_id: &ChatEntryId) -> Option<String> {
    if entry.id != *entry_id {
        return None;
    }
    let ChatEntryKind::ToolResult { full_content, .. } = &entry.kind else {
        return None;
    };
    full_content.clone()
}

/// The `ordinal`-th attachment of a user entry, when the entry is one.
fn attachment_at(
    entry: &ChatEntry,
    entry_id: &ChatEntryId,
    ordinal: usize,
) -> Option<jinn_core_types::attachment::Attachment> {
    if entry.id != *entry_id {
        return None;
    }
    let ChatEntryKind::User { attachments, .. } = &entry.kind else {
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

/// How long one frame may sit in a write before the connection is given
/// up on.
///
/// A browser that stopped reading — a background tab, a stalled parse —
/// keeps the socket open and the write pending. Fifteen seconds is well
/// past any healthy write and under the page's own liveness timeout, so a
/// stalled page is closed here, its `onclose` fires, and it reconnects
/// from its cursor instead of waiting forever.
const WRITE_STALL_BUDGET: Duration = Duration::from_secs(15);

/// One connection's life: a reader and a writer over the same socket, as
/// two tasks.
///
/// The reader dispatches every `ClientMsg` the page sends, one at a time
/// and in order, so a send-then-cancel can never race. It never writes:
/// each reply is handed to the writer. The writer owns the socket's send
/// half and writes whatever it is handed — a reply, a pushed frame, or a
/// heartbeat — under a per-frame stall budget. A slow handler therefore
/// delays only that page's own replies, never its heartbeat or its
/// pushes, and a browser that stopped reading stalls one write and is
/// closed, rather than blocking the dispatch that produced the frame.
///
/// Either half ending ends the connection: the writer reports a stall or
/// a socket error, and the reader, seeing that, drops the stream — which
/// closes the socket — so the page's `onclose` fires exactly once.
async fn ws_loop(socket: WebSocket, shared: Arc<ServerShared>) {
    use futures::StreamExt;

    let (sink, stream) = socket.split();
    // Replies and pushes wait on the writer here; the channel is the
    // queue a stalled browser fills instead of the dispatch blocking.
    let (frames_tx, frames_rx) = tokio::sync::mpsc::channel::<ServerMsg>(64);
    // The writer's word that it gave up on the connection — a stall or a
    // socket error. The reader ends the connection when it hears it.
    let (gave_up_tx, gave_up_rx) = tokio::sync::oneshot::channel::<()>();
    let writer = tokio::spawn(write_loop(
        sink,
        frames_rx,
        gave_up_tx,
        shared.health.clone(),
    ));

    read_loop(stream, shared, frames_tx, gave_up_rx).await;
    // The reader's return dropped the sender: the writer's channel is
    // closed and it exits on its own. A writer that gave up has already
    // returned. The abort only covers a writer still mid-write.
    writer.abort();
}

/// The reader: one dispatch at a time, in order, each reply handed to the
/// writer. A pushed frame is forwarded the same way, so it never waits on
/// the socket. Ends when the page closes, the writer gives up, or an
/// enqueue finds the writer gone.
async fn read_loop(
    mut stream: futures::stream::SplitStream<WebSocket>,
    shared: Arc<ServerShared>,
    frames_tx: tokio::sync::mpsc::Sender<ServerMsg>,
    gave_up_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use futures::StreamExt;

    // The guard lives as long as this loop: when the connection ends, its
    // drop unregisters the page at once.
    let (_id, mut pushed, _connection) = shared.outbound.register();
    shared.health.connected();
    shared.health.publish().await;
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    heartbeat.reset();
    let mut gave_up_rx = std::pin::pin!(gave_up_rx);

    loop {
        tokio::select! {
            // The writer gave up, or its sender was dropped without a word:
            // either way no more frames will go out, so end the connection.
            _ = &mut gave_up_rx => break,
            _ = heartbeat.tick() => {
                if frames_tx.send(ServerMsg::Ping).await.is_err() {
                    break;
                }
            }
            inbound = stream.next() => {
                let Some(Ok(message)) = inbound else {
                    break;
                };
                let Message::Text(text) = message else {
                    continue;
                };
                let reply = match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(msg @ ClientMsg::Hello { .. }) => {
                        crate::health::timed_hello(&shared.health, || {
                            protocol::dispatch(msg, &shared.outbound)
                        })
                        .await
                    }
                    Ok(msg) => protocol::dispatch(msg, &shared.outbound).await,
                    Err(_) => crate::actions::malformed(),
                };
                for frame in reply.frames {
                    if frames_tx.send(frame).await.is_err() {
                        return;
                    }
                }
            }
            frame = pushed.recv() => {
                let Some(frame) = frame else {
                    break;
                };
                if frames_tx.send(frame).await.is_err() {
                    break;
                }
            }
        }
    }
    // The connection is over: the guard's drop (on return) unregisters
    // the page, and the row stops counting it.
    shared.health.disconnected();
    shared.health.publish().await;
}

/// The writer: every frame it is handed, each under the stall budget.
/// Returns when the page's sender is dropped; on a stall or a socket
/// error it tells the reader first, so the connection is closed rather
/// than left half-open.
async fn write_loop<S>(
    mut sink: S,
    mut frames_rx: tokio::sync::mpsc::Receiver<ServerMsg>,
    gave_up: tokio::sync::oneshot::Sender<()>,
    health: crate::health::WebUi2Health,
) where
    S: futures::Sink<Message> + Unpin,
{
    use futures::SinkExt;

    while let Some(frame) = frames_rx.recv().await {
        let text = serde_json::to_string(&frame).unwrap_or_default();
        let written =
            tokio::time::timeout(WRITE_STALL_BUDGET, sink.send(Message::Text(text.into()))).await;
        if !matches!(written, Ok(Ok(()))) {
            // Name the stall on the dashboard, tell the reader, and
            // return — dropping the sink, which with the stream gone
            // closes the socket.
            health.write_stalled();
            health.publish().await;
            let _ = gave_up.send(());
            return;
        }
    }
}

#[cfg(test)]
mod attachment_route_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;
    use axum::extract::{Path, State as AxumState};

    /// A shared server over a harness-shaped handle: a fresh state on a
    /// recording bus, nothing spawned.
    pub(super) fn shared() -> Arc<ServerShared> {
        let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
        let note_bus = bus.clone();
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
        let work_time_cell = slices
            .reader::<jinn_work_time_msg::WorkingTimeState>(&jinn_work_time_msg::work_time_slot())
            .expect("the catalog registers the work-time slot");
        let personas_cell = slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("the catalog registers the persona slot");
        let mcp_runtime_cell = slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("the catalog registers the mcp runtime slot");
        let applets_catalog_cell = slices
            .reader::<jinn_applets_msg::AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
            .expect("the catalog registers the applets catalog slot");
        let applets_picker_cell =
            slices
                .reader::<jinn_applets_msg::AppletsPickerState>(
                    &jinn_applets_msg::applets_picker_slot(),
                )
                .expect("the catalog registers the applets picker slot");
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
                settles: crate::settle::SettleTracker::default(),
                session_store: jinn_session_state::SessionStoreService::new(Arc::new(
                    jinn_kernel::common::services::test_services::FakeSessionStore,
                )),
                config: services.config.clone(),
                services,
                provider_cell,
                work_time_cell,
                personas_cell,
                mcp_runtime_cell,
                applets_catalog_cell,
                applets_picker_cell,
            }),
            version: "test".to_owned(),
            health: crate::health::WebUi2Health::new(note_bus),
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

    /// A sink whose sends never complete: a browser that stopped reading.
    struct StalledSink;

    impl futures::Sink<Message> for StalledSink {
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn a_write_that_stalls_past_the_budget_gives_up_on_the_connection() {
        // Given a writer over a sink that never completes a send, with one frame queued.
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(4);
        let (gave_up_tx, gave_up_rx) = tokio::sync::oneshot::channel();
        let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
        let health = crate::health::WebUi2Health::new(bus);
        let writer = tokio::spawn(write_loop(StalledSink, frames_rx, gave_up_tx, health));
        frames_tx.send(ServerMsg::Ping).await.expect("queue");
        drop(frames_tx);

        // When the write has been pending for the whole budget.
        tokio::time::sleep(WRITE_STALL_BUDGET + Duration::from_secs(1)).await;

        // Then the writer has given up on the connection.
        let verdict = tokio::time::timeout(Duration::from_secs(1), gave_up_rx).await;
        assert!(matches!(verdict, Ok(Ok(()))));
        writer.abort();
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn a_write_that_completes_keeps_the_connection() {
        // Given a writer over a sink that completes every send, with two frames queued.
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(4);
        let (gave_up_tx, gave_up_rx) = tokio::sync::oneshot::channel();
        let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
        let health = crate::health::WebUi2Health::new(bus);
        let writer = tokio::spawn(write_loop(
            futures::sink::drain(),
            frames_rx,
            gave_up_tx,
            health,
        ));
        frames_tx.send(ServerMsg::Ping).await.expect("queue");
        frames_tx.send(ServerMsg::Ping).await.expect("queue");
        drop(frames_tx);

        // When the writer drains the queue and exits normally.
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .expect("the writer exits")
            .expect("the writer task joins");

        // Then it never gave up on the connection.
        assert!(gave_up_rx.await.is_err());
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

    /// The full output of one tool result, as the route answers it.
    async fn full_output_of(shared: &Arc<ServerShared>, session: &str, entry: &str) -> Response {
        tool_output(
            AxumState(shared.clone()),
            Path((session.to_owned(), entry.to_owned())),
        )
        .await
    }

    /// A tool result carrying a full output, pushed onto the active session.
    fn session_with_one_full_output(shared: &ServerShared) -> (SessionId, ChatEntryId) {
        let entry = ChatEntry::tool_result_truncated(
            "c1",
            "bash",
            "out".to_owned(),
            "the whole output".to_owned(),
            jinn_core_types::ToolResultStatus::Success,
            jinn_core_types::tool_types::TruncationMeta {
                truncated_by: jinn_core_types::tool_types::TruncatedBy::Bytes,
                total_lines: 10,
                total_bytes: 14,
                output_lines: 1,
                output_bytes: 3,
            },
        );
        let id = entry.id.clone();
        let state = shared.outbound.state();
        let session_id = state.read().session.active_session_id().clone();
        state.write().session.active_session_mut().push_entry(entry);
        (session_id, id)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_output_route_serves_a_tool_results_full_output_as_plain_text() {
        // Given an active session whose tool result carries a full output.
        let shared = shared();
        let (session_id, entry_id) = session_with_one_full_output(&shared);

        // When the page fetches the full output for that entry.
        let response =
            full_output_of(&shared, &session_id.to_string(), &entry_id.to_string()).await;

        // Then it is the full output, as uncached plain text.
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body")
            .to_vec();
        assert_eq!(body, b"the whole output");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_output_route_answers_404_for_an_entry_without_a_full_output() {
        // Given an active session whose tool result was never truncated.
        let shared = shared();
        let entry = ChatEntry::tool_result(
            "c1",
            "bash",
            "out",
            jinn_core_types::ToolResultStatus::Success,
        );
        let entry_id = entry.id.clone();
        let state = shared.outbound.state();
        let session_id = state.read().session.active_session_id().clone();
        state.write().session.active_session_mut().push_entry(entry);

        // When the page fetches the full output for that entry.
        let response =
            full_output_of(&shared, &session_id.to_string(), &entry_id.to_string()).await;

        // Then the answer is 404.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_output_route_answers_400_for_a_malformed_entry_id() {
        // Given an active session.
        let shared = shared();
        let session_id = shared
            .outbound
            .state()
            .read()
            .session
            .active_session_id()
            .clone();

        // When the entry segment is not a UUID.
        let response = full_output_of(&shared, &session_id.to_string(), "not-a-uuid").await;

        // Then the answer is 400, before any lookup.
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

/// The page shell and the vendor route: the bundle is named by its pin.
#[cfg(test)]
mod vendor_route_tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::*;

    /// The `client2.js` row's sha256, as the manifest pinned it.
    fn js_sha() -> &'static str {
        pinned_sha("client2.js")
    }

    /// An asset's sha256, as the manifest pinned it.
    fn pinned_sha(served: &str) -> &'static str {
        super::pinned_sha(served).expect("the bundle carries the asset")
    }

    async fn vendor_status(path: &str) -> (StatusCode, String) {
        let uri: Uri = format!("/vendor/{path}").parse().expect("a valid uri");
        let response = vendor(uri).await;
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        (response.status(), cache)
    }

    #[rstest::rstest]
    #[test]
    fn the_shell_names_each_asset_by_its_pinned_hash() {
        // Given the page shell.
        let shell = PAGE_SHELL.as_str();

        // Then its stylesheet URL carries the pinned hash, so a rebuilt
        // bundle can never be reached through a cached shell.
        assert!(
            shell.contains(&format!(
                "/vendor/client2.{sha}.css",
                sha = pinned_sha("client2.css")
            )),
            "the shell does not name the hashed stylesheet"
        );
        // And so does its script URL.
        assert!(
            shell.contains(&format!("/vendor/client2.{sha}.js", sha = js_sha())),
            "the shell does not name the hashed script"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_vendor_route_serves_the_asset_at_its_hashed_path_as_immutable() {
        // Given the hashed path the shell names.
        let path = format!("client2.{sha}.js", sha = js_sha());

        // When it is requested.
        let (status, cache) = vendor_status(&path).await;

        // Then the asset is served, immutable.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache, IMMUTABLE_CACHE);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_vendor_route_answers_404_for_a_stale_hash() {
        // Given a hash no asset carries — a shell from a previous bundle.
        let stale = "client2.0000000000000000000000000000000000000000000000000000000000000000.js";

        // When it is requested.
        let (status, _) = vendor_status(stale).await;

        // Then nothing is served: a stale page must not be handed bytes.
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_vendor_route_answers_404_for_an_unhashed_path() {
        // Given the bundle's path without a hash — a shell from before the
        // hashed scheme, or a hand-typed URL.
        let (status, _) = vendor_status("client2.js").await;

        // Then nothing is served.
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

/// The router as a whole: which paths serve the web UI and which do not,
/// each answered through the real dispatch rather than a hand call.
#[cfg(test)]
mod router_tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::*;
    use crate::server::attachment_route_tests::shared;
    use axum::body::to_bytes;
    use tower::ServiceExt as _;

    /// Builds the full router over `shared` and issues a GET through
    /// axum's own tower stack.
    async fn router_get(shared: &Arc<ServerShared>, path: &str) -> Response {
        router(shared.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("the router answers")
    }

    /// The response body as text, for asserting on what a route served.
    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body")
            .to_vec();
        String::from_utf8(bytes).expect("the body is UTF-8")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_root_url_serves_the_page_shell() {
        // Given a built router.
        let shared = shared();

        // When the root is requested.
        let response = router_get(&shared, "/").await;

        // Then the page shell answers, naming the bundle under `/vendor/`.
        assert_eq!(response.status(), StatusCode::OK);
        let shell = body_text(response).await;
        let js_url = format!(
            "/vendor/client2.{}.js",
            pinned_sha("client2.js").expect("the bundle carries the script")
        );
        assert!(
            shell.contains(&js_url),
            "the root must serve the shell that names {js_url}: {shell}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_old_web2_path_is_not_served() {
        // Given a built router.
        let shared = shared();

        // When the retired page path is requested.
        let response = router_get(&shared, "/web2").await;

        // Then nothing serves it: there is no alias and no redirect.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_health_route_answers_the_version() {
        // Given a built router.
        let shared = shared();

        // When the health probe is requested.
        let response = router_get(&shared, "/health").await;

        // Then it answers the serving binary's version as JSON.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "{\"version\":\"test\"}");
    }
}
