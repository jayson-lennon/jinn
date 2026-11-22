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
use axum::extract::FromRequest;
use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Sse;
use axum::response::sse::Event as SseEvent;
use axum::response::sse::KeepAlive;
use axum::routing::get;
use axum::routing::post;
use datastar::consts::ElementPatchMode;
use datastar::prelude::{ExecuteScript, PatchElements, PatchSignals};
use futures::StreamExt as _;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use jinn_webui_msg::SseEnvelope;
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
///
/// A runtime pull that names a swap target (`{selector: '#…'}` in its
/// Datastar binding) needs the response to say where the markup goes:
/// [`Fragment::with_swap`] stamps the `Datastar-Selector`/`Datastar-Mode`
/// headers the browser reads off a `text/html` action response. A plain
/// [`Fragment::new`] stays header-less (a browser fetch with no selector
/// of its own drops the markup — the tests read the body directly).
#[derive(Debug, Clone)]
pub struct Fragment {
    html: String,
    selector: Option<&'static str>,
    mode: Option<&'static str>,
}

impl Fragment {
    /// Wraps rendered fragment HTML.
    #[must_use]
    pub fn new(html: String) -> Self {
        Self {
            html,
            selector: None,
            mode: None,
        }
    }

    /// Names the swap target and mode the browser applies this
    /// fragment with.
    #[must_use]
    pub fn with_swap(mut self, selector: &'static str, mode: &'static str) -> Self {
        self.selector = Some(selector);
        self.mode = Some(mode);
        self
    }

    /// The wrapped HTML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.html
    }
}

impl axum::response::IntoResponse for Fragment {
    fn into_response(self) -> axum::response::Response {
        let mut response = axum::response::Html(self.html).into_response();
        if let (Some(selector), Some(mode)) = (self.selector, self.mode) {
            let name = axum::http::HeaderName::from_static("datastar-selector");
            if let (Ok(value), Ok(mode_value)) = (
                axum::http::HeaderValue::from_str(selector),
                axum::http::HeaderValue::from_str(mode),
            ) {
                response.headers_mut().insert(name, value);
                response.headers_mut().insert(
                    axum::http::HeaderName::from_static("datastar-mode"),
                    mode_value,
                );
            }
        }
        response
    }
}

/// An action POST's response: an SSE body of Datastar SDK events.
///
/// The browser runtime processes an SSE response from an action POST
/// exactly like a stream frame, so one response type covers both
/// shapes. Carrying an optional URL, it is also the navigation
/// response: when present, the events are followed by an
/// `execute-script` event pushing that URL into history (the
/// activate/create path — the client's `pushState` duty, carried as
/// a typed event instead of hand-framed lines).
#[derive(Debug, Clone)]
pub(crate) struct SseAction {
    events: Vec<datastar::DatastarEvent>,
    url: Option<String>,
}

impl SseAction {
    /// An empty sequence — the fire-and-forget answer (cancel).
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self {
            events: Vec::new(),
            url: None,
        }
    }

    /// A sequence of patches, no navigation.
    #[must_use]
    pub(crate) fn new(events: Vec<datastar::DatastarEvent>) -> Self {
        Self { events, url: None }
    }

    /// A navigation: the patches replace the page root, the URL lands
    /// in the address bar.
    #[must_use]
    pub(crate) fn with_url(events: Vec<datastar::DatastarEvent>, url: String) -> Self {
        Self {
            events,
            url: Some(url),
        }
    }
}

impl axum::response::IntoResponse for SseAction {
    fn into_response(self) -> axum::response::Response {
        let url = self.url;
        let mut frames: Vec<Result<SseEvent, Infallible>> = self
            .events
            .iter()
            .map(|event| Ok(event.write_as_axum_sse_event()))
            .collect();
        if let Some(url) = url {
            frames.push(Ok(ExecuteScript::new(format!(
                "window.history.pushState(null, '', {url:?});"
            ))
            .auto_remove(false)
            .into_datastar_event()
            .write_as_axum_sse_event()));
        }
        Sse::new(futures::stream::iter(frames))
            .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
            .into_response()
    }
}

/// One `patch-signals` event carrying the given signal values.
fn signal_patch(pairs: &[(&str, serde_json::Value)]) -> datastar::DatastarEvent {
    let object: serde_json::Map<String, serde_json::Value> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    PatchSignals::new(serde_json::Value::Object(object).to_string()).into_datastar_event()
}

/// One `patch-elements` event appending fragment HTML — placed
/// before the session's streaming container so the container stays
/// the transcript's last child (the tail-replace target).
fn append_patch(html: String) -> datastar::DatastarEvent {
    PatchElements::new(html)
        .selector(format!("#{}", crate::contract::STREAMING_ID))
        .mode(datastar::consts::ElementPatchMode::Before)
        .into_datastar_event()
}

/// One `patch-elements` event replacing a selector's inner HTML —
/// the morph-safe wrapper swap (focus, caret, and input values
/// inside the fragment survive via Datastar's signal bindings, and
/// Idiomorph preserves everything else it can).
fn element_patch_inner(selector: String, html: String) -> datastar::DatastarEvent {
    PatchElements::new(html)
        .selector(selector)
        .mode(datastar::consts::ElementPatchMode::Inner)
        .into_datastar_event()
}

/// The activation/create response's patch sequence: the transient-
/// overlay reset (drawer, menu, both modals — the classes the old
/// wholesale page swap used to clear atomically) followed by the new
/// page shell replacing `#page-root`.
///
/// The reset is a signal patch: every overlay's visibility rides a
/// signal (`data-attr:hidden`, `data-class:drawer-open`), so one
/// patch closes all four atomically before the root is replaced.
async fn overlay_reset_patches(
    shared: &ServerShared,
    id: &SessionId,
) -> Result<Vec<datastar::DatastarEvent>, HandlerError> {
    // The same full render the GET page endpoint serves, so a
    // navigation response and a fresh load are byte-identical shapes —
    // including the settled-prefix split (the boundary-aware cursor
    // the page shell stamps) when the activated session is mid-turn.
    let view = session_view(shared, id).await?;
    let sessions_html = crate::regions::sessions::sessions_fragment(shared, None).await?;
    let (transcript_entries, cursor) = {
        let guard = shared.read_state();
        shared.settled_prefix(id, &guard)
    };
    let transcript_html = crate::templates::render(&crate::templates::TranscriptTemplate::new(
        transcript_entries,
        0,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    let controls_html =
        crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
            id.to_string(),
            view.phase != PhaseView::Idle.as_str(),
        ))
        .map_err(|e| fail(500, e.to_string()))?;
    let model_html = crate::regions::model_picker::button_fragment(shared, &id.to_string())?;
    let menu_html = crate::regions::menu::menu_fragment(&id.to_string());
    // The root fragment — NOT the full page shell. The runtime morphs
    // element patches; a document-shaped patch (DOCTYPE + <html>) is
    // parsed as a whole document and its fight with the live page
    // leaves the old subtree's stray nodes alive. The root div alone
    // replaces cleanly (and its data-signals seed resets every
    // transient overlay signal atomically).
    let page = crate::templates::render(&crate::templates::PageRootTemplate::session(
        &id.to_string(),
        view.transcript.title.clone(),
        cursor,
        sessions_html,
        transcript_html,
        controls_html,
        model_html,
        menu_html,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(vec![
        signal_patch(&[
            (
                crate::contract::SIGNAL_DRAWER_OPEN,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_MENU_OPEN,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_MODAL_OPEN,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_SESSION_NEW_OPEN,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_MCP_OPEN,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_TITLE_EDITING,
                serde_json::Value::Bool(false),
            ),
            (
                crate::contract::SIGNAL_TEXT,
                serde_json::Value::String(String::new()),
            ),
        ]),
        PatchElements::new(page)
            .selector(format!("#{}", crate::contract::PAGE_ROOT_ID))
            .mode(ElementPatchMode::Replace)
            .into_datastar_event(),
    ])
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
    /// The personas cell, for the persona modal's TUI-parity rows.
    personas_cell: jinn_slices::TypedCell<jinn_persona_msg::Personas>,
    /// The MCP runtime cell, for the MCP modal's TUI-parity rows (the
    /// effective status projection — read-only: the cell's writer is
    /// the MCP coordinator, never the web).
    mcp_runtime: jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState>,
    /// The provider registry, for the picker's TUI-parity rows (the
    /// shared builder reads it for static providers, aliases, and
    /// availability).
    provider_registry: jinn_provider_config::ProviderRegistryService,
    /// The resolved API keys, for the picker rows' availability flags.
    api_keys: jinn_provider_config::ApiKeysService,
    /// The user's config layer — the new-session form reads the
    /// `[[project.entry]]` and `[[session_lifecycle.script]]` lists
    /// from it (read-only: the web never writes jinn.toml).
    config: jinn_slices::ConfigLayer,
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
        personas_cell: jinn_slices::TypedCell<jinn_persona_msg::Personas>,
        mcp_runtime: jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState>,
        provider_registry: jinn_provider_config::ProviderRegistryService,
        api_keys: jinn_provider_config::ApiKeysService,
        config: jinn_slices::ConfigLayer,
        fanout: crate::fanout::FanoutHandle,
        port: u16,
        version: String,
    ) -> Self {
        Self {
            state,
            bus,
            session_store,
            provider_cell,
            personas_cell,
            mcp_runtime,
            provider_registry,
            api_keys,
            config,
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

    /// The personas cell, for the persona modal's rows and the switch
    /// POST's validation (the rendered rows are the accepted set).
    #[must_use]
    pub(crate) fn personas_cell(&self) -> &jinn_slices::TypedCell<jinn_persona_msg::Personas> {
        &self.personas_cell
    }

    /// The MCP runtime cell, for the MCP modal's status projection
    /// (read-only — the coordinator is the cell's only writer).
    #[must_use]
    pub(crate) fn mcp_runtime(&self) -> &jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState> {
        &self.mcp_runtime
    }

    /// The provider registry, for the picker rows' shared builder.
    #[must_use]
    pub(crate) fn provider_registry(&self) -> &jinn_provider_config::ProviderRegistryService {
        &self.provider_registry
    }

    /// The resolved API keys, for the picker rows' availability flags.
    #[must_use]
    pub(crate) fn api_keys(&self) -> &jinn_provider_config::ApiKeysService {
        &self.api_keys
    }

    /// The user's config layer, for the new-session form's project
    /// and lifecycle lists (read-only).
    #[must_use]
    pub(crate) fn config(&self) -> &jinn_slices::ConfigLayer {
        &self.config
    }

    /// The message fabric, for action publications.
    #[must_use]
    pub(crate) fn bus(&self) -> &BusService {
        &self.bus
    }

    /// The session's settled prefix under an already-held guard: the
    /// rendered entries the settled transcript carries (`0..boundary`
    /// mid-turn, the whole history otherwise) and the cursor value the
    /// render stamps — the boundary mid-turn, the full count
    /// otherwise. One read path for page fill, the activation
    /// response, and the POST swaps, so every full transcript render
    /// splits at the same boundary the fan-out's pushes split at.
    #[must_use]
    pub(crate) fn settled_prefix(
        &self,
        session_id: &SessionId,
        guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> (Vec<crate::transcript::RenderedEntry>, usize) {
        let boundary = self.fanout.settled_boundary(session_id);
        let Some(session) = guard.session.get(session_id) else {
            return (Vec::new(), 0);
        };
        let transcript = crate::transcript::TranscriptModel::from_live(
            session.history(),
            session.title(),
            &crate::regions::transcript::model_label(session.model_selection()),
            session.phase().as_str(),
            session.is_bot(),
        );
        match boundary {
            // Mid-turn: the settled prefix only — the in-flight tail
            // belongs to the streaming container, and a full render
            // carrying it too would double every tail entry until the
            // settle.
            Some(boundary) => {
                let end = boundary.min(transcript.entries.len());
                let entries = transcript.entries.get(..end).map_or_else(
                    || transcript.entries.clone(),
                    <[crate::transcript::RenderedEntry]>::to_vec,
                );
                (entries, end)
            }
            None => {
                let len = transcript.entries.len();
                (transcript.entries, len)
            }
        }
    }

    /// The boundary-aware full-transcript fragment under an
    /// already-held guard — the POST-swap shape (persona switch,
    /// skill load). Splitting at the boundary keeps those swaps from
    /// carrying the in-flight tail the streaming container already
    /// shows.
    #[must_use]
    pub(crate) fn settled_fragment(
        &self,
        session_id: &SessionId,
        guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        let (entries, _cursor) = self.settled_prefix(session_id, guard);
        crate::templates::render(&crate::templates::TranscriptTemplate::new(entries, 0))
            .unwrap_or_default()
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
        .route(crate::contract::PERSONA_PATH, post(persona))
        .route(crate::contract::MCP_TOGGLE_PATH, post(mcp_toggle))
        .route(crate::contract::TOOLS_TOGGLE_PATH, post(tools_toggle))
        .route(crate::contract::TOOLS_REVERT_PATH, post(tools_revert))
        .route(
            crate::contract::TOOLS_ROWS_ROUTE,
            get(crate::regions::tools::rows_pull_handler),
        )
        .route(crate::contract::SKILL_TOGGLE_PATH, post(skill_toggle))
        .route(crate::contract::SKILL_LOAD_PATH, post(skill_load))
        .route(crate::contract::MODELS_REFRESH_ROUTE, post(refresh_models))
        .route("/web/session/{id}/activate", post(activate))
        .route(crate::contract::RENAME_ROUTE, post(rename))
        .route(crate::contract::SESSION_CREATE_PATH, post(create_session))
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
pub(crate) fn fail(status: u16, sentence: impl Into<String>) -> HandlerError {
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
    // No viewed session on the landing page — the menu renders without
    // the persona item.
    let menu_html = crate::regions::menu::menu_fragment("");
    let html = crate::templates::render(&crate::templates::PageTemplate::landing(
        sessions_html,
        menu_html,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(Page::new(html))
}

/// `GET /web/session/{id}` — the session page: full render for one
/// session, live or archived.
///
/// Mid-turn the inline transcript carries the **settled prefix only**
/// (split at the fan-out's settled boundary) and the cursor attr
/// stamps that boundary — the streaming container above it stays
/// empty at boot, and the first flush tick refills the tail. A page
/// reconnecting mid-turn therefore converges without duplication: the
/// catch-up pull serves `since..boundary`, never the tail the stream
/// is about to re-push wholesale.
async fn page_session(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<Page, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let sessions_html = crate::regions::sessions::sessions_fragment(&shared, None).await?;
    // The settled prefix + boundary cursor, under one guard.
    let (transcript_entries, cursor) = {
        let guard = shared.read_state();
        let (entries, cursor) = shared.settled_prefix(&id, &guard);
        (entries, cursor)
    };
    let transcript_html = crate::templates::render(&crate::templates::TranscriptTemplate::new(
        transcript_entries,
        0,
    ))
    .map_err(|e| fail(500, e.to_string()))?;
    let controls_html =
        crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
            session_id.clone(),
            view.phase != PhaseView::Idle.as_str(),
        ))
        .map_err(|e| fail(500, e.to_string()))?;
    let model_html = crate::regions::model_picker::button_fragment(&shared, &session_id)?;
    let menu_html = crate::regions::menu::menu_fragment(&id.to_string());
    let html = crate::templates::render(&crate::templates::PageTemplate::session(
        &session_id,
        view.transcript.title.clone(),
        cursor,
        sessions_html,
        transcript_html,
        controls_html,
        model_html,
        menu_html,
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
/// history-shrink re-sync rule. Mid-turn the pull serves the
/// **settled** slice only: the cursor's coordinate space ends at the
/// fan-out's settled boundary, so a reconnecting page pulls the
/// settled prefix and the streaming container's tail refills from the
/// next flush tick — never pulled and pushed at once (the
/// no-duplication rule).
pub(crate) async fn transcript_pull(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<TranscriptQuery>,
) -> Result<Fragment, HandlerError> {
    let id = parse_session_id(&session_id)?;
    let view = session_view(&shared, &id).await?;
    let boundary = shared.fanout.settled_boundary(&id);
    // The rendered space the pull is answerable for: the boundary
    // mid-turn, the full count otherwise.
    let rendered_len = boundary.unwrap_or(view.transcript.len());
    let start = query.cursor_within(rendered_len);
    let end = start.unwrap_or(0);
    let entries: Vec<_> = view
        .transcript
        .entries
        .iter()
        .skip(end)
        .take(rendered_len.saturating_sub(end))
        .cloned()
        .collect();
    let html = crate::templates::render(&crate::templates::TranscriptTemplate::new(entries, end))
        .map_err(|e| fail(500, e.to_string()))?;
    Ok(Fragment::new(html))
}

// ── Actions ─────────────────────────────────────────────────────────

/// The dual form extractor: an action POST's body is either the
/// classic `application/x-www-form-urlencoded` shape (every Rust test,
/// curl, and the e2e bootstrapping post) or the Datastar browser's
/// JSON signal body (`{"sessionId": "…", "text": "…"}`). Both decode
/// into the same wire type; the urlencoded contract is unchanged.
#[derive(Debug, Clone)]
pub(crate) struct WebForm<T>(pub T);

impl<S, T> FromRequest<S> for WebForm<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = HandlerError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if content_type.starts_with("application/json") {
            let Json(value) = Json::<serde_json::Value>::from_request(req, state)
                .await
                .map_err(|e| fail(400, format!("malformed signal body: {e}")))?;
            let parsed = serde_json::from_value(value)
                .map_err(|e| fail(400, format!("malformed signal body: {e}")))?;
            Ok(WebForm(parsed))
        } else {
            let Form(form) = Form::<T>::from_request(req, state)
                .await
                .map_err(|rejection| fail(400, format!("malformed form: {rejection}")))?;
            Ok(WebForm(form))
        }
    }
}

/// `POST /web/send` — deliver text into a live session.
///
/// The response is a **SSE sequence of SDK events**, not a fragment:
/// (1) `PatchSignals` clearing the composer's `$text` — clear-only-on-
/// success falls out of the transport, because a 400 refusal carries
/// no events and the draft survives; (2) `PatchElements` appending the
/// optimistic user entry at `#transcript` — the sent message is
/// visible within the send round-trip, before the first stream token.
/// The busy controls fragment is deliberately NOT in the response:
/// the SSE `controls` push is the controls' single writer (a phase
/// read here races the actor drain — the shipped stale-controls bug),
/// and the phase-change push swaps to cancel within one batch window.
async fn send(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::SendForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_send(shared.state(), shared.bus(), &form.session_id, &form.text)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // The optimistic entry renders through the same projection the
    // transcript region uses — identical markup, so the streamed
    // assistant echo and the settle convergence dedupe it (the e2e
    // chat spec pins the no-double).
    let entry = crate::transcript::render_entry(&jinn_core_types::ChatEntry {
        id: jinn_core_types::ChatEntryId::new(),
        timing: jinn_core_types::EntryTiming::Instant {
            at: jiff::Timestamp::now(),
        },
        kind: jinn_core_types::ChatEntryKind::User {
            display: form.text.clone(),
            expanded: form.text.clone(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        },
        pin_position: None,
        context_override: jinn_core_types::ContextOverride::default(),
        context_history: Vec::new(),
        token_count: None,
        model_used: None,
        model_requested: None,
    });
    let html = crate::templates::render(&crate::templates::EntryTemplate {
        entry,
        index: usize::MAX,
    })
    .map_err(|e| fail(500, e.to_string()))?;
    Ok(SseAction::new(vec![
        signal_patch(&[(
            crate::contract::SIGNAL_TEXT,
            serde_json::Value::String(String::new()),
        )]),
        append_patch(html),
    ]))
}

/// `POST /web/cancel` — cancel a live turn. The cancel event's phase
/// settle pushes the idle controls over SSE; this response shows the
/// still-busy state until then.
async fn cancel(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::CancelForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_cancel(&shared.bus, &form.session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // No fragment rides the response: the cancel is an event (one
    // press, whole cancel) and the phase-change `controls` push is
    // the controls fragment's single writer — the shipped
    // stale-controls bug is exactly a POST response disagreeing with
    // it.
    Ok(SseAction::empty())
}

/// `POST /web/model` — switch a session's model. Responds with the
/// re-rendered titlebar button; the modal closes on the client — a
/// stale modal must not survive the switch it caused.
async fn model(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::ModelForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_model(
        shared.state(),
        shared.provider_cell(),
        shared.provider_registry(),
        shared.api_keys(),
        shared.bus(),
        &form.session_id,
        &form.provider_id,
    )
    .await
    .map_err(|error| fail(error.status(), error.sentence()))?;
    // The titlebar's model area swaps in one inner patch (the bare
    // button into the page shell's `#model-picker` host — the payload
    // must never be the button's own host-span render, which would
    // nest a duplicate id); the modal-close is a signal patch, not an
    // element patch: the modal's visibility is signal-driven.
    Ok(SseAction::new(vec![
        element_patch_inner(
            format!("#{}", crate::contract::MODEL_PICKER_ID),
            crate::regions::model_picker::button_fragment(&shared, &form.session_id)?,
        ),
        signal_patch(&[(
            crate::contract::SIGNAL_MODAL_OPEN,
            serde_json::Value::Bool(false),
        )]),
    ]))
}

/// `POST /web/persona` — switch the viewed session's persona. The web
/// mirror of the TUI persona picker's confirm; the delivery path
/// validates, applies, and publishes. The response swaps the
/// re-rendered modal (the active mark moves) and — when the chosen
/// persona carries no `<compaction>` block — the whole transcript, so
/// the loud error entry is visible without a reload (the fan-out
/// handles no chat-entry event; the POST response is the only
/// correction path). The modal-close rides the same signal patch the
/// model switch uses.
async fn persona(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::PersonaForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    let blockless = crate::delivery::handle_persona(
        shared.state(),
        shared.personas_cell(),
        shared.bus(),
        &form.session_id,
        &form.persona,
    )
    .await
    .map_err(|error| fail(error.status(), error.sentence()))?;

    let mut events = Vec::with_capacity(3);
    if blockless {
        // The chosen persona lacks a handoff block: the delivery path
        // pushed the loud error entry, and the transcript swap is what
        // makes it visible now (rendered from state that already holds
        // the entry — the sync-write-then-render ordering). The swap
        // renders the settled prefix (boundary-aware), so a swap
        // landing mid-turn never carries the tail the streaming
        // container already shows.
        let id = parse_session_id(&form.session_id)?;
        let transcript_html = {
            let guard = shared.read_state();
            shared.settled_fragment(&id, &guard)
        };
        events.push(element_patch_inner(
            format!("#{}", crate::contract::TRANSCRIPT_ID),
            transcript_html,
        ));
    }
    events.push(element_patch_inner(
        format!("#{}", crate::contract::PERSONA_MODAL_ID),
        crate::regions::persona_picker::modal_fragment(&shared, &form.session_id, "")?,
    ));
    events.push(signal_patch(&[(
        crate::contract::SIGNAL_PERSONA_MODAL_OPEN,
        serde_json::Value::Bool(false),
    )]));
    Ok(SseAction::new(events))
}

/// `POST /web/mcp` — toggle one MCP server's enablement for the
/// viewed session. The web mirror of the TUI inspector's toggle:
/// the delivery path validates, applies the XOR synchronously, and
/// publishes `McpEnablementChanged` (the full desired set — never a
/// delta). The response swaps the re-rendered modal — the row's mark
/// and badge are correct the moment the response lands, so the modal
/// stays open (per-click immediacy; there is no staged apply to
/// confirm).
async fn mcp_toggle(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::McpToggleForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_mcp_toggle(
        shared.state(),
        shared.config(),
        shared.bus(),
        &form.session_id,
        &form.server,
    )
    .await
    .map_err(|error| fail(error.status(), error.sentence()))?;
    Ok(SseAction::new(vec![element_patch_inner(
        format!("#{}", crate::contract::MCP_MODAL_ID),
        crate::regions::mcp_servers::modal_fragment(&shared, &form.session_id)?,
    )]))
}

/// The tools modal's wire shapes — request/response are region-local
/// (the contract bars touching `jinn-webui-msg` for a region feature),
/// dual-extracted through [`WebForm`] like every action POST.
#[derive(Debug, Clone, serde::Deserialize)]
struct ToolsToggleForm {
    /// The session whose tool filter to commit against.
    #[serde(alias = "sessionId")]
    session_id: String,
    /// The tool name, exactly as a rendered row carries it.
    tool: String,
}

/// The revert POST's body: the open-time snapshot the dismissal posts
/// back. `snapshot` is the JSON `NameFilter` string the open pull
/// stamped (or JSON `null` for absence); `None` covers a urlencoded
/// body that omits the field entirely, which reads the same as `null`.
#[derive(Debug, Clone, serde::Deserialize, Default)]
struct ToolsRevertForm {
    /// The session whose tool filter to restore.
    #[serde(default, alias = "sessionId")]
    session_id: String,
    /// The open-time snapshot, verbatim.
    #[serde(default)]
    snapshot: Option<String>,
}

/// `POST /web/tools` — toggle one tool's enablement for the viewed
/// session. The web mirror of the TUI picker's confirm applied per
/// click: the delivery path validates the tool against the rendered
/// rows, derives the deny filter over the post-click off-rows, and
/// writes it synchronously. The response swaps the re-rendered modal —
/// the row marks are correct the moment the response lands, so the
/// modal stays open. No publication rides the write (the TUI picker
/// publishes none).
async fn tools_toggle(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<ToolsToggleForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_tools_toggle(shared.state(), &form.session_id, &form.tool)
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // The response swaps the rows only: the marks update in place, and
    // the modal skeleton + revert stamp (patched by the open pull,
    // living outside `#tools-rows`) are never re-rendered — a toggle
    // must not re-execute the stamp's `data-init` nor nest a second
    // modal inside the open one. The fragment carries the rows
    // container itself, so the patch replaces it (an inner patch would
    // nest a second `#tools-rows`).
    Ok(SseAction::new(vec![
        PatchElements::new(crate::regions::tools::rows_fragment(
            &shared,
            &form.session_id,
        )?)
        .selector(crate::contract::TOOLS_ROWS_SELECTOR.to_owned())
        .mode(ElementPatchMode::Replace)
        .into_datastar_event(),
    ]))
}

/// `POST /web/tools/revert` — dismiss the tools modal: restore the
/// open-time filter the snapshot names, verbatim (absence included).
/// The response carries no patch: the modal is closing, and the rows'
/// next open renders from the restored state.
async fn tools_revert(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<ToolsRevertForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_tools_revert(
        shared.state(),
        &form.session_id,
        form.snapshot.as_deref(),
    )
    .map_err(|error| fail(error.status(), error.sentence()))?;
    Ok(SseAction::empty())
}

/// `POST /web/skills/toggle` — toggle one skill's enablement for the
/// viewed session. The web mirror of the TUI picker's toggle: the
/// delivery path validates the name against the session's discovered
/// set, applies the mode-preserving `permit`/`withhold` to the
/// session's skill filter synchronously, and publishes nothing (the
/// TUI toggle publishes nothing — the skill gate reads the filter
/// live). A loaded skill toggles nothing (the TUI no-op). The
/// response swaps the re-rendered modal — the row's mark is correct
/// the moment the response lands, so the modal stays open (per-click
/// immediacy).
async fn skill_toggle(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::SkillToggleForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_skill_toggle(shared.state(), &form.session_id, &form.skill)
        .map_err(|error| fail(error.status(), error.sentence()))?;
    Ok(SseAction::new(vec![element_patch_inner(
        format!("#{}", crate::contract::SKILL_MODAL_ID),
        crate::regions::skills::modal_fragment(&shared, &form.session_id)?,
    )]))
}

/// `POST /web/skills/load` — force-load a skill into the viewed
/// session's context. The web mirror of the TUI picker's `<c-l>`: the
/// delivery path pins the tool-call/result pair, auto-enables durably
/// when disabled, and publishes `MarkSessionInteracted` (the only
/// publish; an already-loaded skill appends the transient notice
/// instead of a pair). The response swaps the re-rendered modal AND
/// the transcript — the pinned pair (or the duplicate's notice) is
/// visible without a reload (the persona-switch POST precedent: the
/// fan-out handles no chat-entry event, so the POST response is the
/// only correction path).
async fn skill_load(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::SkillLoadForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_skill_load(shared.state(), shared.bus(), &form.session_id, &form.skill)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let id = parse_session_id(&form.session_id)?;
    // The pinned pair (or the duplicate's notice) rides the settled
    // prefix render — boundary-aware, so a swap landing mid-turn never
    // duplicates the tail.
    let transcript_html = {
        let guard = shared.read_state();
        shared.settled_fragment(&id, &guard)
    };
    Ok(SseAction::new(vec![
        element_patch_inner(
            format!("#{}", crate::contract::SKILL_MODAL_ID),
            crate::regions::skills::modal_fragment(&shared, &form.session_id)?,
        ),
        element_patch_inner(
            format!("#{}", crate::contract::TRANSCRIPT_ID),
            transcript_html,
        ),
    ]))
}

/// `POST /web/models/refresh` — the modal's refresh button. Publishes
/// `RefreshModels` and responds with the re-rendered status wrapper
/// immediately; the rows update when discovery lands (the fan-out's
/// `models` push). Never blocks on discovery.
async fn refresh_models(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::ModelsRefreshForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_refresh_models(shared.bus(), &form.session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // Button now; rows arrive with the discovery fan-out's `models`
    // push (a signal tick — the open modal's effect re-pulls its
    // rows). The spinner clears when the tick lands (the browser's
    // own effect on the tick signal).
    Ok(SseAction::new(vec![element_patch_inner(
        format!("#{}", crate::contract::MODEL_PICKER_ID),
        crate::regions::model_picker::button_fragment(&shared, &form.session_id)?,
    )]))
}

/// `POST /web/session/{id}/activate` — switch the active session.
///
/// Publishes `SessionLoadRequested` (the store actor hydrates an
/// archived session or activates a loaded one) and responds with an
/// **SSE sequence of SDK events**: (1) the transient-overlay reset —
/// drawer, menu, and both modals close, because the wholesale page
/// swap this replaces used to reset them atomically and morph
/// patching would preserve the open classes; (2) a `replace`-grade
/// patch of `#page-root` with the activated session's whole page
/// shell. The root's `data-init` span arrives fresh inside the
/// replacement, so the stream bind re-reads its URL — one connection,
/// closed and reopened exactly once per navigation.
async fn activate(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    form: Result<WebForm<jinn_webui_msg::SwitchForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(_) = form?;
    crate::delivery::handle_switch(shared.state(), shared.bus(), &session_id)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    // Validate the path id (the page render re-parses it; this check
    // makes a malformed id a 400 before the switch publishes).
    parse_session_id(&session_id)?;
    // A page render for the activated session. The load is async (the
    // store actor hydrates it), so a just-archived session may not be
    // in live state yet; the render falls through to the store and
    // the convergence push corrects any lag.
    let id = parse_session_id(&session_id)?;
    let url = session_url(&session_id);
    let patches = overlay_reset_patches(&shared, &id).await?;
    Ok(SseAction::with_url(patches, url))
}

/// `POST /web/session-new` — create a session through the shared
/// creation path.
///
/// Runs [`crate::delivery::handle_create`] (the validations, the
/// stash, and the shared creation function) and responds with the
/// **new session's whole page** — the activation response shape,
/// because creation activates the session instance-wide and one
/// navigation mechanism serves both. The client swaps `#page-root`,
/// updates the URL, and rebinds its stream exactly as it does after
/// an activate POST.
async fn create_session(
    AxumState(shared): AxumState<ServerShared>,
    form: Result<WebForm<jinn_webui_msg::CreateSessionForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    let created =
        crate::delivery::handle_create(shared.state(), shared.bus(), shared.config(), &form)
            .map_err(|error| fail(error.status(), error.sentence()))?;
    // The new session's page — the activation response shape, because
    // creation activates the session instance-wide and one navigation
    // mechanism serves both. The created id is live in state (the
    // shared path set it active), so no store fall-through is needed.
    let url = session_url(&created.to_string());
    let patches = overlay_reset_patches(&shared, &created).await?;
    Ok(SseAction::with_url(patches, url))
}

/// `POST /web/session/{id}/rename` — rename a live session.
///
/// Applies the TUI rename's three mutations and publishes
/// `PersistSession` (see [`crate::delivery::handle_rename`]), then
/// responds with the **sessions-list fragment** — the same markup the
/// `GET /web/sessions` pull serves — so the initiating page's sidebar
/// shows the new title in one round trip. Every other page viewing
/// the renamed session refreshes its sidebar from the addressed
/// `sessions` SSE push (the double swap on the initiating page is
/// harmless: both render the same post-rename list).
async fn rename(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    form: Result<WebForm<jinn_webui_msg::RenameForm>, HandlerError>,
) -> Result<SseAction, HandlerError> {
    let WebForm(form) = form?;
    crate::delivery::handle_rename(shared.state(), shared.bus(), &session_id, &form.title)
        .await
        .map_err(|error| fail(error.status(), error.sentence()))?;
    let id = parse_session_id(&session_id)?;
    // One render, two consumers: the addressed refresh push and the
    // POST response MUST carry the same list shape. The merged
    // fragment (live + store rows) is the only shape the GET pull and
    // the POST ever serve; pushing the live-only shape here would
    // wipe every store-only row from connected sidebars one frame
    // before the response restores them — the shrink-then-refill
    // flicker. (The fan-out actor's own SessionLoadCompleted /
    // SessionArchived refreshes keep the live-only shape deliberately:
    // those events change only live state, and their next pull or
    // page render carries the store truth.)
    let list_html = crate::regions::sessions::sessions_fragment(&shared, None).await?;
    shared
        .fanout
        .push_sessions_refresh_from(&id, list_html.clone());
    // The initiating page's sidebar is the response's element patch;
    // its titlebar text and tab title follow from the two signal
    // patches (the title span and the document title both bind
    // `$title`).
    Ok(SseAction::new(vec![
        element_patch_inner(format!("#{}", crate::contract::SESSIONS_ID), list_html),
        signal_patch(&[(
            crate::contract::SIGNAL_TITLE,
            serde_json::Value::String(form.title.clone()),
        )]),
        signal_patch(&[(
            crate::contract::SIGNAL_TITLE_COMMITTING,
            serde_json::Value::Bool(false),
        )]),
    ]))
}

// ── SSE ─────────────────────────────────────────────────────────────

/// The canonical page URL for one session id — the navigation
/// response's `pushState` target.
fn session_url(session_id: &str) -> String {
    format!("{}{session_id}", crate::contract::SESSION_PAGE_PREFIX)
}

/// `GET /web/stream/{id}` — the live push for one session, in the
/// Datastar protocol.
///
/// Order is the correctness contract: **catch-up first, subscribe
/// second.** The endpoint renders the entries after the client's
/// cursor from state and emits them as `append` patches at
/// `#transcript`, then subscribes to the broadcast and filters by the
/// path's session id — a tab left on an old session never receives
/// another session's fragments. A fragment published between catch-up
/// and subscribe is re-delivered by the next `transcript-full`
/// convergence push; the pull endpoint remains the truth, so no
/// ordering here can lose data permanently.
///
/// The cursor comes from the `Last-Event-ID` header (the Datastar
/// fetch transport replays it on every retry — verified in the bundle)
/// and falls back to the `?since=` query parameter, which the page's
/// `data-init` bind carries from the page shell's rendered cursor: a
/// fresh load must not re-append the entries the page already
/// rendered. A reconnect keeps its last `id:` and ignores the query.
async fn stream(
    AxumState(shared): AxumState<ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<StreamQuery>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    // A malformed id streams nothing; the immediate close is the error
    // surface (a browser stream has no error body).
    let Ok(id) = parse_session_id(&session_id) else {
        return Sse::new(futures::stream::empty::<Result<SseEvent, Infallible>>()).into_response();
    };

    // The header wins (a reconnecting client); the query is the fresh
    // page's rendered cursor; zero is the no-information default.
    let cursor: usize = headers
        .get(axum::http::HeaderName::from_static("last-event-id"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .or(query.since)
        .unwrap_or(0);
    // Catch-up: the entries after the client's cursor, rendered under
    // one guard and dropped before the stream is built — clipped to
    // the session's settled boundary when a turn is in flight, so the
    // catch-up serves the settled prefix only and the streaming
    // container's tail refills from the next flush tick (never pulled
    // and pushed at once). An unknown session catches up empty — the
    // session may still be loading; the first `transcript-full` push
    // fills it.
    let catchup = {
        let boundary = shared.fanout.settled_boundary(&id);
        let guard = shared.read_state();
        guard.session.get(&id).map(|session| {
            let transcript = crate::transcript::TranscriptModel::from_live(
                session.history(),
                session.title(),
                &crate::regions::transcript::model_label(session.model_selection()),
                session.phase().as_str(),
                session.is_bot(),
            );
            // The answerable space: the boundary mid-turn, the full
            // count otherwise — then the shrink-re-read rule inside it.
            let total = boundary.unwrap_or(transcript.len()).min(transcript.len());
            let start = cursor.min(total);
            let slice = transcript
                .entries
                .get(start..total)
                .map_or_else(Vec::new, <[crate::transcript::RenderedEntry]>::to_vec);
            (slice, start, total)
        })
    };
    let catchup_events = catchup.map_or_else(Vec::new, |(entries, start, total)| {
        render_catchup_events(&entries, start, total)
    });

    // Subscribe AFTER the catch-up snapshot: a fragment published in
    // between re-arrives via the next convergence push. Each stream
    // counts its own sessions pushes — the tick signal bumps per
    // connection, never globally.
    let rx = shared.fanout.subscribe();
    let session_filter = id.clone();
    let tick = std::sync::atomic::AtomicU64::new(0);
    let live = tokio_stream::wrappers::BroadcastStream::new(rx)
        .filter_map(move |result| {
            futures::future::ready(result.ok().and_then(|envelope: SseEnvelope| {
                if envelope.session_id != session_filter {
                    return None;
                }
                let n = tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                crate::push::sdk_events_of(&envelope, n).ok()
            }))
        })
        .map(
            |events| -> futures::stream::Iter<
                std::vec::IntoIter<Result<axum::response::sse::Event, Infallible>>,
            > {
                futures::stream::iter(
                    events
                        .into_iter()
                        .map(|event| Ok(event.write_as_axum_sse_event()))
                        .collect::<Vec<_>>(),
                )
            },
        );
    let catchup = futures::stream::iter(catchup_events.into_iter().map(|event| {
        futures::stream::iter(vec![Ok(event.write_as_axum_sse_event())])
            as futures::stream::Iter<
                std::vec::IntoIter<Result<axum::response::sse::Event, Infallible>>,
            >
    }));
    let stream = futures::StreamExt::chain(catchup, live).flatten();
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
        .into_response()
}

/// The stream endpoint's query: the fresh page's rendered cursor,
/// carried by the `data-init` bind as `?since=N`. Ignored whenever a
/// `Last-Event-ID` header is present (a reconnecting client's cursor
/// is newer information).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StreamQuery {
    /// The rendered-entry cursor the page's inline transcript already
    /// carries.
    #[serde(default)]
    pub since: Option<usize>,
}

/// Builds the catch-up events for a reconnect: every entry after
/// `start` as a patch that inserts it **before** the streaming
/// container (the entries are settled transcript content — the
/// container holds the in-flight tail and must stay the transcript's
/// last child), then one cursor-park frame that parks the cursor at
/// `total` even when the last render failed, so a reconnect does not
/// re-pull forever. The trailing frame patches no signal any effect
/// reads (its `onlyIfMissing` payload is a no-op on a connected
/// page) — a reconnect appends without scrolling; its `id:` line is
/// its real payload (the browser's resume position).
pub(crate) fn render_catchup_events(
    entries: &[crate::transcript::RenderedEntry],
    start: usize,
    total: usize,
) -> Vec<datastar::DatastarEvent> {
    let mut events: Vec<datastar::DatastarEvent> = entries
        .iter()
        .enumerate()
        .filter_map(|(offset, entry)| {
            let template = crate::templates::EntryTemplate {
                entry: entry.clone(),
                index: start + offset,
            };
            let html = crate::templates::render(&template).ok()?;
            Some(
                PatchElements::new(html)
                    .selector(format!("#{}", crate::contract::STREAMING_ID))
                    .mode(datastar::consts::ElementPatchMode::Before)
                    .id((start + offset + 1).to_string())
                    .into_datastar_event(),
            )
        })
        .collect();
    // The trailing cursor-park: a signal patch whose `id:` parks the
    // browser's resume cursor at `total` (the settled boundary
    // mid-turn — never the live count). The payload is reader-less —
    // `onlyIfMissing` over an always-declared signal, so no signal
    // value ever changes and no `data-effect` anywhere fires: a
    // reconnect's catch-up appends land without scrolling.
    events.push(
        PatchSignals::new(
            serde_json::json!({ crate::contract::SIGNAL_MENU_OPEN: false }).to_string(),
        )
        .only_if_missing(true)
        .id(total.max(start).to_string())
        .into_datastar_event(),
    );
    events
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
