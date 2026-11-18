//! The axum web surface — routes over plain HTTP + SSE.
//!
//! [`router`] builds the application; [`ServerShared`] is the state the
//! handlers reach through axum's `State` extractor: the live session
//! state, the bus, the session store, the provider cell, and the
//! fan-out's broadcast handle.
//!
//! Handlers are the HTTP layer only: parse, call delivery, map the
//! outcome. Read paths capture data under one state read (or one store
//! await) and drop the guard before rendering. The region render
//! functions live in [`crate::regions`] — the same functions the SSE
//! fan-out calls, so a pushed fragment and a pulled one come from one
//! code path. This module owns the page shell, the SSE endpoint, the
//! action posts, vendor serving, and health: the cross-region chrome
//! a region change must never need to edit.

use axum::Form;
use axum::Json;
use axum::Router;
use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Sse;
use axum::response::sse::Event as SseEvent;
use axum::response::sse::KeepAlive;
use axum::routing::get;
use axum::routing::post;
use futures::StreamExt as _;
use jinn_core_types::SessionId;
use jinn_kernel::State;
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

// ── Response shapes ─────────────────────────────────────────────────

/// A whole-page HTML response (`Content-Type: text/html`): the shell
/// with every region filled. The compile-time answer to the shipped
/// wrong-shape bug — a page endpoint literally cannot return a
/// fragment, because [`Page`] and [`Fragment`] are distinct types and
/// axum knows how to render each.
#[derive(Debug, Clone)]
pub struct Page(String);

impl Page {
    /// Wraps rendered page HTML.
    #[must_use]
    pub fn new(html: String) -> Self {
        Self(html)
    }

    /// The wrapped HTML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl axum::response::IntoResponse for Page {
    fn into_response(self) -> axum::response::Response {
        axum::response::Html(self.0).into_response()
    }
}

/// A fragment HTML response: one region's markup, destined for a
/// client-side swap into an existing page. Distinct from [`Page`] so a
/// handler that answers a fragment cannot be confused with one that
/// answers a page.
#[derive(Debug, Clone)]
pub struct Fragment(String);

impl Fragment {
    /// Wraps rendered fragment HTML.
    #[must_use]
    pub fn new(html: String) -> Self {
        Self(html)
    }

    /// The wrapped HTML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl axum::response::IntoResponse for Fragment {
    fn into_response(self) -> axum::response::Response {
        axum::response::Html(self.0).into_response()
    }
}

/// The state every handler shares.
///
/// Fields are `pub(crate)`: the region modules' pull handlers read the
/// same state under the same discipline (one guard, dropped before any
/// await) the page handlers here use. There are no setters — the
/// shared state is assembled once in `new` and read-only thereafter.
#[derive(Clone)]
pub struct ServerShared {
    /// The live session registry and this process's app state.
    ///
    /// Reading goes through [`ServerShared::read_state`], which holds
    /// the guard discipline (one guard, dropped before any await) the
    /// page and fragment handlers share.
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

    /// Reads the app state under one short-lived guard. The guard
    /// discipline: take it, render synchronously, drop it before any
    /// `.await` — the same contract the fan-out's render loop keeps.
    #[must_use]
    pub(crate) fn read_state(&self) -> jinn_kernel::common::state::StateReadGuard<'_> {
        self.state.read()
    }

    /// Reads the provider cell under one short-lived guard.
    pub(crate) fn read_provider(
        &self,
    ) -> parking_lot::RwLockReadGuard<'_, jinn_provider_selection_msg::ProviderCell> {
        self.provider_cell.read()
    }

    /// Loads the archived-session summaries through the session store.
    pub(crate) async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        self.session_store.load_summaries().await
    }

    /// The app state, for the delivery paths that take `&State` (the
    /// fan-out shares the same handle; nothing outside the crate
    /// reaches it).
    #[must_use]
    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    /// The provider cell, for the delivery path that resolves the
    /// picked model.
    #[must_use]
    pub(crate) fn provider_cell(
        &self,
    ) -> &jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell> {
        &self.provider_cell
    }

    /// The message fabric, for action publications.
    #[must_use]
    pub(crate) fn bus(&self) -> &BusService {
        &self.bus
    }
}

/// Builds the web surface's routes.
///
/// Region pull routes fold out of the registry — each region binds its
/// own handler through [`crate::region::WebRegion::pull`], so adding a
/// region with a pull route needs no edit here. The remaining routes
/// are the cross-region chrome: the page shell, the SSE stream, the
/// action posts, vendor serving, and health.
pub fn router(shared: ServerShared) -> Router {
    let base = Router::new()
        .route("/", get(root_redirects_to_web))
        .route("/web", get(page_root))
        .route("/web/session/{id}", get(page_session))
        .route("/web/stream/{id}", get(stream))
        .route("/web/send", post(send))
        .route("/web/cancel", post(cancel))
        .route("/web/model", post(model))
        .route("/web/session/{id}/activate", post(activate))
        .route("/vendor/{*path}", get(serve_vendor))
        .route("/healthz", get(health));
    crate::region::regions()
        .iter()
        .fold(base, |router, region| match region.pull() {
            Some(pull) => router.route(pull.pattern, pull.handler),
            None => router,
        })
        .with_state(shared)
}

/// The handler error shape: status + one sentence.
pub type HandlerError = (StatusCode, String);

/// Maps a domain refusal or render failure to the response error.
fn fail(status: u16, sentence: impl Into<String>) -> HandlerError {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        sentence.into(),
    )
}

/// Maps an askama render failure to the 500 error — the shape the
/// region modules' pull-path renders return through.
pub(crate) fn map_render_error(error: &askama::Error) -> HandlerError {
    fail(500, error.to_string())
}

pub(crate) fn parse_session_id(raw: &str) -> Result<SessionId, HandlerError> {
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
async fn page_root(AxumState(shared): AxumState<ServerShared>) -> Result<Page, HandlerError> {
    let sessions_html = crate::regions::sessions::sessions_fragment(&shared, None).await?;
    let html = crate::templates::render(&crate::templates::PageTemplate::landing(sessions_html))
        .map_err(|e| fail(500, e.to_string()))?;
    Ok(Page::new(html))
}

/// `GET /web/session/{id}` — the session page: full render for one
/// session, live or archived.
async fn page_session(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Page, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let sessions_html = crate::regions::sessions::sessions_fragment(&shared, Some(&id)).await?;
    let transcript_html = crate::templates::render(&crate::templates::TranscriptTemplate {
        entries: view.transcript.entries.clone(),
        start: 0,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    let controls_html =
        crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
            session_id.clone(),
            view.phase != PhaseView::Idle.as_str(),
        ))
        .map_err(|e| fail(500, e.to_string()))?;
    let model_html = crate::regions::model_picker::model_picker_html(
        &shared,
        &session_id,
        &view.transcript.model,
    )?;
    let html = crate::templates::render(&crate::templates::PageTemplate::session(
        session_id.clone(),
        view.transcript.title.clone(),
        view.transcript.model.clone(),
        view.transcript.len(),
        sessions_html,
        transcript_html,
        controls_html,
        model_html,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Page::new(html))
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
        let guard = shared.read_state();
        guard.session.get(id).map(|session| {
            (
                crate::transcript::TranscriptModel::from_live(
                    session.history(),
                    session.title(),
                    &crate::regions::transcript::model_label(session.model_selection()),
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
            &crate::regions::transcript::model_label(&snapshot.metadata.profile.model),
            PhaseView::Idle.as_str(),
            false,
        ),
        phase: PhaseView::Idle.as_str().to_owned(),
    })
}

// ── Region pulls (HTTP) ─────────────────────────────────────────────

/// `GET /web/transcript/{id}?since=N` — the cursor pull (the
/// transcript region's GET half; the region module binds it).
///
/// The correctness dependency: a page (re)connects, reads its
/// `Last-Event-ID`, pulls everything after it, then subscribes to the
/// stream. A cursor past the entry count re-reads everything — the
/// history-shrink re-sync rule.
pub(crate) async fn transcript_pull(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<TranscriptQuery>,
) -> Result<Fragment, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let start = query.cursor_within(view.transcript.len());
    let entries: Vec<_> = view.transcript.since(query.since).to_vec();
    let html = crate::templates::render(&crate::templates::TranscriptTemplate {
        entries,
        start: start.unwrap_or(0),
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Fragment::new(html))
}

// ── Actions ─────────────────────────────────────────────────────────

/// `POST /web/send` — deliver text into a live session. Responds with
/// the re-rendered composer controls; the SSE `controls` event
/// corrects any race within one batch window.
async fn send(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::SendForm>, axum::extract::rejection::FormRejection>,
) -> Result<Fragment, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_send(shared.state(), shared.bus(), &form.session_id, &form.text)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let busy = {
        let guard = shared.read_state();
        let id = parse_session_id(&form.session_id)?;
        guard
            .session
            .get(&id)
            .is_some_and(|session| session.phase().is_working())
    };
    let html = crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
        form.session_id,
        busy,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Fragment::new(html))
}

/// `POST /web/cancel` — cancel a live turn. The cancel event's phase
/// settle pushes the idle controls over SSE; this response shows the
/// still-busy state until then.
async fn cancel(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::CancelForm>, axum::extract::rejection::FormRejection>,
) -> Result<Fragment, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_cancel(&shared.bus, &form.session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let html = crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
        form.session_id,
        true,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Fragment::new(html))
}

/// `POST /web/model` — switch a session's model. Responds with the
/// re-rendered picker, selection on the new model.
async fn model(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<Form<jinn_webui_msg::ModelForm>, axum::extract::rejection::FormRejection>,
) -> Result<Fragment, HandlerError> {
    let Form(form) = form.map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
    crate::delivery::handle_model(
        shared.state(),
        shared.provider_cell(),
        shared.bus(),
        &form.session_id,
        &form.provider_id,
    )
    .await
    .map_err(|error| fail(error.status(), error.sentence()))?;
    Ok(Fragment::new(
        crate::regions::model_picker::model_picker_html(
            &shared,
            &form.session_id,
            &form.provider_id,
        )?,
    ))
}

/// `POST /web/session/{id}/activate` — switch the active session.
///
/// Publishes `SessionLoadRequested` (the store actor hydrates an
/// archived session or activates a loaded one) and responds with the
/// **whole page** for the activated session. The web client replaces
/// `#page-root` with it and rebinds its stream — the response shape is
/// [`Page`], not a fragment, which the return type makes unconfusable.
/// A page the client does not swap (no client, curl) still shows the
/// activated session's correct URL-renderable state.
async fn activate(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Page, HandlerError> {
    crate::delivery::handle_switch(&shared.bus, &session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // Validate the path id (the page render re-parses it; this check
    // makes a malformed id a 400 before the switch publishes).
    parse_session_id(&session_id)?;
    // A page render for the activated session. The load is async (the
    // store actor hydrates it), so a just-archived session may not be
    // in live state yet; `page_session` falls through to the store and
    // the SSE `transcript-full` convergence push corrects any lag.
    page_session(AxumState(shared), AxPath(session_id)).await
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
/// The client dispatches by event name (see `webui/client/src/sse.ts`);
/// the payload's cursor rides the `id:` line and returns as
/// `Last-Event-ID` on reconnect.
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
        let guard = shared.read_state();
        guard.session.get(&id).map(|session| {
            let transcript = crate::transcript::TranscriptModel::from_live(
                session.history(),
                session.title(),
                &crate::regions::transcript::model_label(session.model_selection()),
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

/// One envelope → one SSE frame: the event name the client dispatches
/// on, the cursor on the `id:` line, the HTML as data.
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

    let Some((asset, versioned)) = crate::vendor::resolve_request(&path) else {
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
            // Cache policy splits on the URL form. A versioned URL
            // (`main.6a91e504.js`) names one content forever — its hash
            // is the manifest's sha256 prefix — so the browser may keep
            // it for a year without ever revalidating; a new build
            // ships new hashes and busts itself. The bare form exists
            // only as a migration path (a cached old page's unversioned
            // module imports), so it must revalidate every use: the
            // cheap 304 keeps old pages working while never serving
            // stale code for long.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(if versioned.is_some() {
                    "public, max-age=31536000, immutable"
                } else {
                    "public, max-age=0, must-revalidate"
                }),
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
