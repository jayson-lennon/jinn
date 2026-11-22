//! The tools region — the hamburger menu's Tools item and its modal,
//! the web mirror of the TUI `<leader>st` picker.
//!
//! Parts declared: a GET pull route plus the signal-only `tools` SSE
//! kind, no settle render, and no page fill (`container_id` is
//! `None`: the modal swaps into its own host span, the host-not-
//! container rule every modal follows). The open pull renders the
//! whole modal; a `toolsTick` re-pull (one per MCP runtime write)
//! fetches the **rows route** and swaps `#tools-rows` only, so the
//! revert stamp never re-initializes mid-open.
//!
//! The rows are TUI-parity by construction: built by the shared seed
//! vocabulary ([`jinn_tools_msg::modal_rows`]) over the viewed
//! session's registry view, its provider name, its bot facts, and its
//! **live** tool filter — the same provider and bot gates and the same
//! case-insensitive sort the TUI picker seeds from, with every
//! enablement mark derived by the same predicate the prompt applies.
//! An absent registry yields zero rows without erroring.
//!
//! The modal carries the persona/model filter bar: the pull and the
//! rows route both take an optional `?filter=` and apply the shared
//! fuzzy filter ([`jinn_tools_msg::filter_rows`], the TUI picker's
//! match core over `{name} {description}`) before rendering, with
//! matched runs marked. The filter rides the **rows** route only — the
//! revert stamp in the modal skeleton is never re-rendered by
//! filtering. Toggle responses echo the client's filter back and
//! render the filtered view; validation still reads the full row set
//! (the view filter never reaches the accepted-set derivation).
//!
//! A row click commits immediately (in `delivery.rs`): the session's
//! tool filter becomes a deny filter over the modal's off-rows with
//! the clicked tool flipped — the TUI confirm's semantic applied per
//! click, allow-mode demotion included. Dismissing the modal posts a
//! revert restoring the filter the session held at open: the open
//! pull stamps it into the `toolsRevert` signal through the stamp
//! element the fragment renders beside the modal, and the guarded
//! dismissal handlers post it back. Neither POST publishes
//! `PersistSession` — the TUI picker path publishes none, and the web
//! adds no persistence behavior beyond the TUI's.
//!
//! The menu item lives in the menu region (this feature's opening
//! gesture); this region owns the modal fragment, its pull, and its
//! CSS.

use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use jinn_core_types::NameFilter;
use jinn_tools_msg::WebToolRow;
use serde::Deserialize;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The tools region.
pub struct ToolsRegion;

impl WebRegion for ToolsRegion {
    fn id(&self) -> &'static str {
        "tools"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: true,
            page_fill: false,
        }
    }

    fn event_kinds(&self) -> &'static [jinn_webui_msg::SseKind] {
        // The `tools` signal: MCP connection changes re-pull the open
        // modal's rows. Signal only — the rows route remains the
        // renderer, and the re-pull swaps the rows fragment only so
        // the revert stamp never re-initializes mid-open.
        &[jinn_webui_msg::SseKind::Tools]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::TOOLS_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into its own host span in the page shell
        // (`TOOLS_MODAL_HOST_ID`), not a page-shell inline region — the
        // host-not-container rule every modal follows, so opening it
        // can never destroy the menu item that opened it.
        None
    }

    fn css(&self) -> &'static str {
        TOOLS_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the modal reads the
        // registry cell, config snapshot, and session state through
        // `ServerShared`, not one guard, so its fragment cannot come
        // from a guard. The trait method exists for registry
        // completeness; it renders nothing.
        String::new()
    }
}

/// `GET /web/tools/{id}` — the tools modal fragment. The client pulls
/// this on open (the menu item); the server renders the TUI-parity row
/// set for the viewed session and stamps the open-time filter into the
/// revert signal through the fragment's stamp element.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
/// The pulls' query: the optional `filter` parameter — the shared
/// fuzzy filter over tool name and description.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsFilterQuery {
    /// The filter text; absent or empty renders every row.
    #[serde(default)]
    pub filter: Option<String>,
}

/// `GET /web/tools/{id}?filter=…` — the tools modal fragment. The
/// client pulls this on open (the menu item); the server renders the
/// TUI-parity row set for the viewed session and stamps the open-time
/// filter into the revert signal through the fragment's stamp element.
/// The filter renders the seeded rows' initial view (the tick and
/// toggle re-renders carry their own).
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<ToolsFilterQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = query.filter.unwrap_or_default();
    Ok(
        crate::server::Fragment::new(modal_fragment(&shared, &session_id, &filter)?)
            .with_swap(crate::contract::TOOLS_MODAL_HOST_SELECTOR, "inner"),
    )
}

/// `GET /web/tools/{id}/rows?filter=…` — the tick effect's and the
/// filter's re-pull target. It answers with the **rows fragment
/// only**, swapping `#tools-rows` inside the open modal: the skeleton
/// and the revert stamp are never re-rendered, so the stamp's
/// `data-init` never re-runs mid-open and the open-time snapshot the
/// dismissal restores survives. The filter rides here for the same
/// reason — every keystroke re-renders the rows, never the stamp.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) async fn rows_pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<ToolsFilterQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = query.filter.unwrap_or_default();
    // The fragment carries its own rows container, so the swap
    // replaces that element rather than its inner HTML — an inner swap
    // would nest a second `#tools-rows` inside the open one.
    Ok(
        crate::server::Fragment::new(rows_fragment(&shared, &session_id, &filter)?)
            .with_swap(crate::contract::TOOLS_ROWS_SELECTOR, "outer"),
    )
}

/// The open-time filter snapshot and the seed inputs for one render,
/// captured under one short-lived guard (dropped before any await,
/// per the guard discipline). The write surface is the session state
/// the web shares with the TUI; the registry resolves through the same
/// `AppState` accessor the TUI seed uses.
struct Seed {
    /// The session's registry tools, gated and sorted for display.
    rows: Vec<WebToolRow>,
    /// The session's live tool filter at open — the revert's payload.
    /// `None` means absence, restored as absence.
    tool_filter: Option<NameFilter>,
}

/// Captures the seed under one guard: the registry view for the viewed
/// session, its provider name, its bot facts, and its live filter.
#[must_use]
fn seed(shared: &crate::server::ServerShared, session_id: &str) -> Seed {
    let Some(id) = jinn_core_types::SessionId::try_from_string(session_id) else {
        return Seed {
            rows: Vec::new(),
            tool_filter: None,
        };
    };
    let guard = shared.read_state();
    let Some(session) = guard.session.get(&id) else {
        return Seed {
            rows: Vec::new(),
            tool_filter: None,
        };
    };
    let provider_name = session.model_selection().provider_name().to_owned();
    let is_bot = session.is_bot();
    let bot_has_parent = session.parent_session().is_some();
    let tool_filter = session.tool_filter().cloned();
    let definitions: Vec<jinn_core_types::ToolDefinition> = guard
        .tool_registry()
        .map_or_default(|registry| registry.read().tools_for_session(&id));
    Seed {
        rows: jinn_tools_msg::modal_rows(
            &definitions,
            &provider_name,
            is_bot,
            bot_has_parent,
            tool_filter.as_ref(),
        ),
        tool_filter,
    }
}

/// The modal fragment: the overlay with the filter input, the
/// filtered TUI-parity rows, and the revert stamp, rendered from the
/// shared seed.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn modal_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let seed = seed(shared, session_id);
    let rows = jinn_tools_msg::filter_rows(&seed.rows, filter);
    let rendered: Vec<String> = rows
        .into_iter()
        .map(|filtered| {
            crate::templates::render(&crate::templates::ToolsRowTemplate::from_filtered(filtered))
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, what = "tools-row", "webui template render failed; \
                     skipping the row");
                    String::new()
                })
        })
        .collect();
    crate::templates::render(&crate::templates::ToolsModalTemplate::with_contract(
        rendered,
        filter.to_owned(),
        seed.tool_filter.as_ref(),
        session_id,
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The rows-only fragment: what a toggle response and the tick/filter
/// re-pull swap into `#tools-rows`. The stamp and the modal skeleton
/// are deliberately outside this fragment — a toggle re-render must
/// never re-execute the stamp's `data-init` (it would reset the
/// open-time snapshot the dismissal needs) nor nest a second
/// `#tools-modal` inside the open one (an inner patch of the modal's
/// own id would). `filter` renders the caller's current view.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn rows_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let rows = jinn_tools_msg::filter_rows(&seed(shared, session_id).rows, filter);
    let rendered: Vec<String> = rows
        .into_iter()
        .map(|filtered| {
            crate::templates::render(&crate::templates::ToolsRowTemplate::from_filtered(filtered))
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, what = "tools-row", "webui template render failed; \
                     skipping the row");
                    String::new()
                })
        })
        .collect();
    crate::templates::render(&crate::templates::ToolsRowsTemplate::with_contract(
        rendered,
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The tools region's stylesheet: the modal overlay, the filter bar
/// (the persona/model input grammar, styled by the contract-policed
/// id), and the rows. The modal geometry mirrors the MCP modal's (the
/// same overlay language, at the same stack level); the rows carry the
/// TUI picker's ✓/✗ mark.
pub const TOOLS_CSS: &str = r#"
.tools-modal { position: fixed; inset: 0; background: var(--scrim);
             display: flex; align-items: flex-start;
             justify-content: center; padding-top: 10vh; z-index: 40; }
.tools-modal[hidden] { display: none; }
.tools-modal-inner { background: var(--panel); border: 1px solid var(--border);
                   border-radius: 8px; width: min(560px, 92vw);
                   max-height: 70vh; display: flex; flex-direction: column; }
.tools-modal-head { display: flex; gap: 8px; padding: 10px;
                  border-bottom: 1px solid var(--border); }
#tools-filter { flex: 1; background: var(--bg); color: var(--fg);
              border: 1px solid var(--border); border-radius: 6px;
              padding: 6px 8px; font: inherit; }
#tools-rows { overflow-y: auto; padding: 4px 0; }
.tools-row { display: flex; gap: 8px; padding: 7px 12px; cursor: pointer;
           align-items: baseline; }
.tools-row:hover { background: var(--bg-hover); }
.tools-row .tools-row-marker { width: 1.2em; color: var(--accent); }
.tools-row .tools-row-summary { color: var(--muted); flex: 1; }
.tools-row.tools-row-off .tools-row-marker { color: var(--muted); opacity: .7; }
.tools-row.tools-row-off .tools-row-name { color: var(--muted); }
.tools-row mark { background: transparent; color: var(--accent);
                font-weight: 600; }
@media (max-width: 767px) {
  .tools-row { min-height: 40px; padding: 10px 12px; }
  #tools-filter { min-height: 40px; }
}
"#;

#[cfg(test)]
mod tools_region_tests;
