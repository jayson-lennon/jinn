//! The axum web surface — routes over plain HTTP + SSE.
//!
//! [`router`] builds the application; [`ServerShared`] is the state the
//! handlers reach through axum's `State` extractor: the live session
//! state, the bus, the session store, the provider cell, and the
//! fan-out's broadcast handle.
//!
//! Handlers are the HTTP layer only: parse, call delivery, map the
//! outcome. Read paths capture data under one state read (or one store
//! await) and drop the guard before rendering. The fragment helpers —
//! [`controls_fragment`], [`sessions_fragment`],
//! [`transcript_full_fragment`] — are the same render functions the
//! SSE fan-out calls, so a pushed fragment and a pulled one come from
//! one code path.

use axum::Form;
use axum::Json;
use axum::Router;
use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::Sse;
use axum::response::sse::Event as SseEvent;
use axum::response::sse::KeepAlive;
use axum::routing::get;
use axum::routing::post;
use futures::StreamExt as _;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_kernel::common::state::StateReadGuard;
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use jinn_webui_msg::SseEnvelope;
use jinn_webui_msg::SseKind;
use jinn_webui_msg::TranscriptQuery;
use std::convert::Infallible;
use std::time::Duration;

/// The SSE keepalive cadence: comment frames keep VPN/NAT idle
/// timeouts from severing the stream.
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

/// The state every handler shares.
#[derive(Clone)]
pub struct ServerShared {
    /// The live session registry and this process's app state.
    state: State,
    /// The message fabric, for action publications.
    bus: BusService,
    /// The session store, for archived sessions.
    session_store: jinn_session_state::SessionStoreService,
    /// The provider cell, for the model picker.
    provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    /// The fan-out's broadcast handle, for the SSE endpoint.
    fanout: crate::fanout::FanoutHandle,
    /// The bound port (reported by `/healthz`).
    port: u16,
    /// The serving binary's version (reported by `/healthz`).
    version: String,
}

impl ServerShared {
    /// Assembles the shared handler state.
    #[must_use]
    pub fn new(
        state: State,
        bus: BusService,
        session_store: jinn_session_state::SessionStoreService,
        provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
        fanout: crate::fanout::FanoutHandle,
        port: u16,
        version: String,
    ) -> Self {
        Self {
            state,
            bus,
            session_store,
            provider_cell,
            fanout,
            port,
            version,
        }
    }
}

/// Builds the web surface's routes.
pub fn router(shared: ServerShared) -> Router {
    Router::new()
        .route("/", get(root_redirects_to_web))
        .route("/web", get(page_root))
        .route("/web/session/{id}", get(page_session))
        .route("/web/sessions", get(sessions_fragment_handler))
        .route("/web/transcript/{id}", get(transcript))
        .route("/web/model-picker/{id}", get(model_picker))
        .route("/web/stream/{id}", get(stream))
        .route("/web/send", post(send))
        .route("/web/cancel", post(cancel))
        .route("/web/model", post(model))
        .route("/web/session/{id}/activate", post(activate))
        .route("/vendor/{*path}", get(serve_vendor))
        .route("/healthz", get(health))
        .with_state(shared)
}

/// The handler error shape: status + one sentence.
type HandlerError = (StatusCode, String);

/// Maps a domain refusal or render failure to the response error.
fn fail(status: u16, sentence: impl Into<String>) -> HandlerError {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        sentence.into(),
    )
}

fn parse_session_id(raw: &str) -> Result<SessionId, HandlerError> {
    SessionId::try_from_string(raw).ok_or_else(|| fail(400, "session id is not a UUID"))
}

// ── Pages ───────────────────────────────────────────────────────────

/// `GET /` — the bare host root redirects to the page's real home.
/// The canonical URL is `/web` so the surface never fights any other
/// handler that might later claim the root path.
async fn root_redirects_to_web() -> axum::response::Redirect {
    axum::response::Redirect::temporary("/web")
}

/// `GET /web` — the landing page: sessions list, no transcript.
async fn page_root(
    AxumState(shared): AxumState<ServerShared>,
) -> Result<Html<String>, HandlerError> {
    let sessions_html = sessions_fragment(&shared, None).await?;
    let html = crate::templates::render(&crate::templates::PageTemplate {
        style: crate::templates::STYLE_CSS,
        session_id: String::new(),
        title: "jinn".to_owned(),
        model: String::new(),
        sessions_html,
        transcript_html: String::new(),
        controls_html: String::new(),
        model_html: String::new(),
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Html(html))
}

/// `GET /web/session/{id}` — the session page: full render for one
/// session, live or archived.
async fn page_session(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Html<String>, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let sessions_html = sessions_fragment(&shared, Some(&id)).await?;
    let transcript_html = crate::templates::render(&crate::templates::TranscriptTemplate {
        entries: view.transcript.entries.clone(),
        start: 0,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    let controls_html = crate::templates::render(&crate::templates::ControlsTemplate {
        session_id: session_id.clone(),
        busy: view.phase != PhaseView::Idle.as_str(),
    })
    .map_err(|e| fail(500, e.to_string()))?;
    let model_html = model_picker_html(&shared, &session_id, &view.transcript.model)?;
    let html = crate::templates::render(&crate::templates::PageTemplate {
        style: crate::templates::STYLE_CSS,
        session_id: session_id.clone(),
        title: view.transcript.title.clone(),
        model: view.transcript.model.clone(),
        sessions_html,
        transcript_html,
        controls_html,
        model_html,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Html(html))
}

// ── Shared reads ────────────────────────────────────────────────────

/// A session's full view for one page render: the projection plus the
/// phase, from live state or (when not loaded) the store.
struct SessionView {
    transcript: crate::transcript::TranscriptModel,
    phase: String,
}

/// Builds one session's view. The live path reads under one guard and
/// drops it before any await; the archived path awaits the store
/// (request-scoped, no command round-trip).
async fn session_view(shared: &ServerShared, id: &SessionId) -> Result<SessionView, HandlerError> {
    let live = {
        let guard = shared.state.read();
        guard.session.get(id).map(|session| {
            (
                crate::transcript::TranscriptModel::from_live(
                    session.history(),
                    session.title(),
                    &model_label(session.model_selection()),
                    session.phase().as_str(),
                    session.is_bot(),
                ),
                session.phase(),
            )
        })
    };
    if let Some((transcript, phase)) = live {
        return Ok(SessionView {
            transcript,
            phase: phase.as_str().to_owned(),
        });
    }

    let snapshot = shared
        .session_store
        .load_session(id)
        .await
        .map_err(|report| fail(500, format!("session store read failed: {report:?}")))?
        .ok_or_else(|| fail(404, "no session with that id (live or archived)"))?;
    Ok(SessionView {
        transcript: crate::transcript::TranscriptModel::from_snapshot(
            &snapshot,
            snapshot.metadata.title.as_deref(),
            &model_label(&snapshot.metadata.profile.model),
            PhaseView::Idle.as_str(),
            false,
        ),
        phase: PhaseView::Idle.as_str().to_owned(),
    })
}

/// The model selection rendered as the label a reader recognizes.
fn model_label(selection: &jinn_core_types::ModelSelection) -> String {
    match selection {
        jinn_core_types::ModelSelection::Single(name) => name.clone(),
        jinn_core_types::ModelSelection::Alloy { models, .. } => models.join(", "),
    }
}

/// The sessions list: live rows from state, archived rows from the
/// store, active first. `active` names the session to mark active
/// (`None` on the landing page). Store summaries are awaited on the
/// request path (request-scoped; no command round-trip); a store
/// failure degrades to the live-only list — a web session list must
/// not 500 because a disk read hiccuped.
async fn sessions_fragment(
    shared: &ServerShared,
    active: Option<&SessionId>,
) -> Result<String, HandlerError> {
    let (mut rows, active_id) = {
        let guard = shared.state.read();
        let rows: Vec<jinn_webui_msg::SessionListItem> = guard
            .session
            .iter()
            .map(|(id, session)| jinn_webui_msg::SessionListItem {
                session_id: id.clone(),
                title: session.title().unwrap_or("Untitled Session").to_owned(),
                active: Some(id) == active,
                is_bot: session.is_bot(),
                loaded: true,
            })
            .collect();
        let active_id = guard.session.active_session_id().clone();
        (rows, active_id)
    };

    // Archived rows: skip ids already live; the merged list is the one
    // shape the template renders.
    if let Ok(summaries) = shared.session_store.load_summaries().await {
        for summary in summaries {
            if rows.iter().any(|row| row.session_id == summary.session_id) {
                continue;
            }
            rows.push(jinn_webui_msg::SessionListItem {
                session_id: summary.session_id,
                title: summary.title,
                active: active == Some(&active_id),
                is_bot: summary.is_bot,
                loaded: false,
            });
        }
    }
    rows.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });

    crate::templates::render(&crate::templates::SessionsTemplate { rows })
        .map_err(|e| fail(500, e.to_string()))
}

/// The composer-controls fragment from a state guard — the shape the
/// fan-out calls with its own guard. The phase decides the button.
pub fn controls_fragment(session_id: &SessionId, guard: &StateReadGuard<'_>) -> String {
    let busy = guard
        .session
        .get(session_id)
        .is_some_and(|session| session.phase().is_working());
    crate::templates::render(&crate::templates::ControlsTemplate {
        session_id: session_id.to_string(),
        busy,
    })
    .unwrap_or_else(|_| String::new())
}

/// The sessions-list fragment from a state guard — the fan-out's shape.
///
/// Live rows only: the fan-out renders on the actor's hot path, and
/// the store's summary set changes rarely enough that the next page
/// pull (or a `SessionLoadCompleted`-triggered page refresh) carries
/// the archived truth.
pub fn sessions_live_fragment(guard: &StateReadGuard<'_>) -> String {
    let rows: Vec<jinn_webui_msg::SessionListItem> = guard
        .session
        .iter()
        .map(|(id, session)| jinn_webui_msg::SessionListItem {
            session_id: id.clone(),
            title: session.title().unwrap_or("Untitled Session").to_owned(),
            active: id == guard.session.active_session_id(),
            is_bot: session.is_bot(),
            loaded: true,
        })
        .collect();
    crate::templates::render(&crate::templates::SessionsTemplate { rows })
        .unwrap_or_else(|_| String::new())
}

/// The full-transcript fragment from a state guard — the fan-out's
/// convergence push.
pub fn transcript_full_fragment(session_id: &SessionId, guard: &StateReadGuard<'_>) -> String {
    let Some(session) = guard.session.get(session_id) else {
        return String::new();
    };
    let transcript = crate::transcript::TranscriptModel::from_live(
        session.history(),
        session.title(),
        &model_label(session.model_selection()),
        session.phase().as_str(),
        session.is_bot(),
    );
    crate::templates::render(&crate::templates::TranscriptTemplate {
        entries: transcript.entries,
        start: 0,
    })
    .unwrap_or_else(|_| String::new())
}

// ── Fragments (HTTP) ────────────────────────────────────────────────

/// `GET /web/sessions` — the sessions fragment (live + archived).
async fn sessions_fragment_handler(
    AxumState(shared): AxumState<ServerShared>,
) -> Result<Html<String>, HandlerError> {
    let active = {
        let guard = shared.state.read();
        guard.session.active_session_id().clone()
    };
    Ok(Html(sessions_fragment(&shared, Some(&active)).await?))
}

/// `GET /web/transcript/{id}?since=N` — the cursor pull.
///
/// The correctness dependency: a page (re)connects, reads its
/// `Last-Event-ID`, pulls everything after it, then subscribes to the
/// stream. A cursor past the entry count re-reads everything — the
/// history-shrink re-sync rule.
async fn transcript(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<TranscriptQuery>,
) -> Result<Html<String>, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let start = query.cursor_within(view.transcript.len());
    let entries: Vec<_> = view.transcript.since(query.since).to_vec();
    let html = crate::templates::render(&crate::templates::TranscriptTemplate {
        entries,
        start: start.unwrap_or(0),
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Html(html))
}

/// `GET /web/model-picker/{id}` — the model-picker fragment.
async fn model_picker(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Html<String>, HandlerError> {
    let current = {
        let id = parse_session_id(&session_id)?;
        let guard = shared.state.read();
        match guard.session.get(&id) {
            Some(session) => model_label(session.model_selection()),
            None => String::new(),
        }
    };
    Ok(Html(model_picker_html(&shared, &session_id, &current)?))
}

/// Renders the model-picker fragment: the cache's ids, sorted, with
/// the active selection marked.
fn model_picker_html(
    shared: &ServerShared,
    session_id: &str,
    current: &str,
) -> Result<String, HandlerError> {
    let mut models: Vec<String> = {
        let guard = shared.provider_cell.read();
        match guard.model_cache.as_ref() {
            Some(cache) => cache
                .entries
                .values()
                .flatten()
                .map(|info| info.id.clone())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    models.sort();
    models.dedup();
    crate::templates::render(&crate::templates::ModelTemplate {
        session_id: session_id.to_owned(),
        current: current.to_owned(),
        models,
    })
    .map_err(|e| fail(500, e.to_string()))
}

// ── Actions ─────────────────────────────────────────────────────────

/// `POST /web/send` — deliver text into a live session. Responds with
/// the re-rendered composer controls; the SSE `controls` event
/// corrects any race within one batch window.
async fn send(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::SendForm>, axum::extract::rejection::FormRejection>,
) -> Result<Html<String>, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_send(&shared.state, &shared.bus, &form.session_id, &form.text)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let busy = {
        let guard = shared.state.read();
        let id = parse_session_id(&form.session_id)?;
        guard
            .session
            .get(&id)
            .is_some_and(|session| session.phase().is_working())
    };
    let html = crate::templates::render(&crate::templates::ControlsTemplate {
        session_id: form.session_id,
        busy,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Html(html))
}

/// `POST /web/cancel` — cancel a live turn. The cancel event's phase
/// settle pushes the idle controls over SSE; this response shows the
/// still-busy state until then.
async fn cancel(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::CancelForm>, axum::extract::rejection::FormRejection>,
) -> Result<Html<String>, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_cancel(&shared.bus, &form.session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let html = crate::templates::render(&crate::templates::ControlsTemplate {
        session_id: form.session_id,
        busy: true,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Html(html))
}

/// `POST /web/model` — switch a session's model. Responds with the
/// re-rendered picker, selection on the new model.
async fn model(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::ModelForm>, axum::extract::rejection::FormRejection>,
) -> Result<Html<String>, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_model(
        &shared.state,
        &shared.provider_cell,
        &shared.bus,
        &form.session_id,
        &form.provider_id,
    )
    .await
    .map_err(|error| fail(error.status(), error.sentence()))?;
    Ok(Html(model_picker_html(
        &shared,
        &form.session_id,
        &form.provider_id,
    )?))
}

/// `POST /web/session/{id}/activate` — switch the active session.
///
/// Publishes `SessionLoadRequested` (the store actor hydrates an
/// archived session or activates a loaded one) and responds with the
/// fresh sessions list; the `SessionLoadCompleted` SSE push then
/// re-renders every connected page's list with the new active mark.
async fn activate(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Html<String>, HandlerError> {
    crate::delivery::handle_switch(&shared.bus, &session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let active = parse_session_id(&session_id)?;
    Ok(Html(sessions_fragment(&shared, Some(&active)).await?))
}

// ── SSE ─────────────────────────────────────────────────────────────

/// `GET /web/stream/{id}` — the live push for one session.
///
/// Order is the correctness contract: **catch-up first, subscribe
/// second.** The endpoint renders the entries after the client's
/// `Last-Event-ID` cursor from state and emits them as `chat` events,
/// then subscribes to the broadcast and filters by the path's session
/// id — a tab left on an old session never receives another session's
/// fragments. A fragment published between catch-up and subscribe is
/// re-delivered by the next `transcript-full` convergence push; the
/// pull endpoint remains the truth, so no ordering here can lose data
/// permanently.
///
/// The htmx SSE extension swaps fragments by event name; the payload's
/// cursor rides the `id:` line and returns as `Last-Event-ID` on
/// reconnect.
async fn stream(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    // A malformed id streams nothing; the immediate close is the error
    // surface (EventSource has no error body).
    let Ok(id) = parse_session_id(&session_id) else {
        return Sse::new(futures::stream::empty::<Result<SseEvent, Infallible>>()).into_response();
    };

    // Catch-up: the entries after the client's cursor, rendered under
    // one guard and dropped before the stream is built. An unknown
    // session catches up empty — the session may still be loading; the
    // first `transcript-full` push fills it.
    let cursor: usize = headers
        .get(axum::http::HeaderName::from_static("last-event-id"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let catchup = {
        let guard = shared.state.read();
        guard.session.get(&id).map(|session| {
            let transcript = crate::transcript::TranscriptModel::from_live(
                session.history(),
                session.title(),
                &model_label(session.model_selection()),
                session.phase().as_str(),
                session.is_bot(),
            );
            (transcript.since(Some(cursor)).to_vec(), transcript.len())
        })
    };
    let catchup_frames = catchup.map_or_else(Vec::new, |(entries, total)| {
        render_catchup_frames(&entries, cursor, total)
    });

    // Subscribe AFTER the catch-up snapshot: a fragment published in
    // between re-arrives via the next convergence push.
    let rx = shared.fanout.subscribe();
    let session_filter = id.clone();
    let live = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |result| {
        futures::future::ready(result.ok().and_then(|envelope: SseEnvelope| {
            (envelope.session_id == session_filter)
                .then(|| Ok::<_, Infallible>(sse_event_of(&envelope)))
        }))
    });
    let stream = futures::StreamExt::chain(
        futures::stream::iter(
            catchup_frames
                .into_iter()
                .map(|frame| Ok::<_, Infallible>(catchup_event(frame))),
        ),
        live,
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
        .into_response()
}

/// Renders the catch-up entries into SSE frames: one `chat` frame per
/// entry (each advancing the cursor), so a client dropping mid-catchup
/// resumes exactly where it stopped.
/// One catch-up frame before SSE wrapping: the entry HTML (empty for
/// the trailing cursor-advance frame) and the `id:` cursor it parks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatchupFrame {
    /// The entry HTML; empty on the trailing cursor-advance frame.
    pub html: String,
    /// The SSE `id:` — the cursor this frame leaves the client at.
    pub cursor: usize,
}

/// Builds the catch-up frames for a reconnect: every entry after
/// `start` as a data frame, then one empty frame that parks the cursor
/// at `total` even when the last render failed, so a reconnect does
/// not re-pull forever.
pub(crate) fn render_catchup_frames(
    entries: &[crate::transcript::RenderedEntry],
    start: usize,
    total: usize,
) -> Vec<CatchupFrame> {
    let mut frames: Vec<CatchupFrame> = entries
        .iter()
        .enumerate()
        .filter_map(|(offset, entry)| {
            let template = crate::templates::EntryTemplate {
                entry: entry.clone(),
                index: start + offset,
            };
            let html = crate::templates::render(&template).ok()?;
            Some(CatchupFrame {
                html,
                cursor: start + offset + 1,
            })
        })
        .collect();
    frames.push(CatchupFrame {
        html: String::new(),
        cursor: total.max(start),
    });
    frames
}

fn catchup_event(frame: CatchupFrame) -> SseEvent {
    SseEvent::default()
        .event(SseKind::Chat.event_name())
        .id(frame.cursor.to_string())
        .data(frame.html)
}

/// One envelope → one SSE frame: htmx's `event:` name, the cursor on
/// the `id:` line, the HTML as data.
fn sse_event_of(envelope: &SseEnvelope) -> SseEvent {
    SseEvent::default()
        .event(envelope.kind.event_name())
        .id(envelope.cursor.to_string())
        .data(envelope.html.clone())
}

// ── Vendor + health ─────────────────────────────────────────────────

/// `GET /vendor/<path>` — one embedded, pre-gzipped asset.
async fn serve_vendor(AxPath(path): AxPath<String>) -> axum::response::Response {
    use axum::http::HeaderValue;
    use axum::http::header;
    use axum::response::IntoResponse;

    let Some(asset) = crate::vendor::VENDOR_ASSETS.iter().find(|a| a.path == path) else {
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
            // Same blessing canvas applies: the assets carry no
            // credentials and the server exposes no session state.
            (
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            ),
            // The dist changes only at a jinn upgrade; the served URL
            // is versioned by the binary, not the request.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
    )
        .into_response();
    // The bytes are already gzipped on disk; forward the compressed
    // form verbatim and let the Content-Length speak for the payload.
    {
        use axum::body::Body as AxBody;
        *response.body_mut() = AxBody::from(asset.bytes.to_vec());
    }
    response
}

/// `GET /healthz` — instance identity.
async fn health(AxumState(shared): AxumState<ServerShared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": shared.version,
        "port": shared.port,
    }))
}
