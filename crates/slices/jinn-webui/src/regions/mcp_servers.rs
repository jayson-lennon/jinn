//! The MCP servers region — the hamburger menu's MCP item and its
//! modal.
//!
//! Parts declared: GET pull route + SSE event. No settle render (the
//! modal is pull-opened, never part of a settle pass) and no page
//! fill (the modal swaps into its own host span, like the persona
//! modal — `container_id` is `None` because the host is not one of
//! the page shell's inline regions).
//!
//! The rows are TUI-parity: built by the shared row builder
//! (`jinn_mcp_msg::build_rows` over the `[mcp.<name>]` config
//! section, the viewed session's enabled set, and the MCP runtime
//! cell's effective status projection) — the same builder the TUI
//! inspector consumes, sorted case-insensitively by name. Singleton
//! servers read the effective projection, not the per-session bucket:
//! a running singleton read naively would show perpetually "starting"
//! (the bug `refresh_inspector_snapshot` documents and works around).
//!
//! The modal carries the persona/model filter bar: the pull takes an
//! optional `?filter=` and applies the shared fuzzy filter
//! ([`jinn_mcp_msg::filter_rows`], the TUI inspector's match core over
//! `{name} {description}`) before rendering, with matched runs marked.
//! The filter narrows the view, never the accepted set (a toggle
//! validates against the configured set regardless of the view). The
//! toggle response echoes the client's filter back and renders the
//! filtered view.
//!
//! The modal fragment renders the row list for the viewed session —
//! name, launch summary, enabled mark, live status badge. A toggle
//! POST (in `delivery.rs`) writes the session's enabled set
//! synchronously and publishes the existing `McpEnablementChanged`
//! (the full desired set — never a delta), answering with the
//! re-rendered modal.
//!
//! Live status rides the push seam: the coordinator emits
//! `McpRuntimeStored` strictly after each runtime-cell status write,
//! the fan-out subscribes that event (never the raw status events —
//! two publishers against one cell writer) and bumps the `mcpTick`
//! signal; an open modal's effect re-pulls its rows. The GET pull
//! route remains the renderer; the pushed frame is a signal only.
//!
//! The menu item lives in the menu region (this feature's opening
//! gesture); this region owns the modal fragment and its pull.

use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::RawQuery;
use axum::extract::State as AxumState;
use jinn_mcp_msg::McpServerRow;
use jinn_webui_msg::SseKind;
use serde::Deserialize;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The MCP servers region.
pub struct McpServersRegion;

impl WebRegion for McpServersRegion {
    fn id(&self) -> &'static str {
        "mcp-servers"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: true,
            page_fill: false,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::McpStatus]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::MCP_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into its own host span in the page shell
        // (`MCP_MODAL_HOST_ID`), not a page-shell inline region — the
        // host-not-container rule every modal follows, so opening it
        // can never destroy the menu item that opened it.
        None
    }

    fn css(&self) -> &'static str {
        MCP_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the modal reads the
        // config snapshot, session state, and the MCP runtime cell
        // through `ServerShared`, not one guard, so its fragment
        // cannot come from a guard. The trait method exists for
        // registry completeness; it renders nothing.
        String::new()
    }
}

/// The pull's query: the optional `filter` parameter — the shared
/// fuzzy filter over server name and description.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpFilterQuery {
    /// The filter text; absent or empty renders every row.
    #[serde(default)]
    pub filter: Option<String>,
}

/// `GET /web/mcp-servers/{id}?filter=…` — the MCP servers modal
/// fragment. The client pulls this on open (the menu item), on every
/// filter keystroke, and on every `mcpTick` bump; the server renders
/// every configured server's current row, so the wire always carries
/// the live picture.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<McpFilterQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = query.filter.unwrap_or_default();
    Ok(
        crate::server::Fragment::new(modal_fragment(&shared, &session_id, &filter)?)
            .with_swap(crate::contract::MCP_MODAL_HOST_SELECTOR, "inner"),
    )
}

/// `GET /web/mcp-servers/rows/{id}?filter=…` — the modal effect's
/// re-pull target. It answers with the **rows fragment only**,
/// swapping the rows container inside the open modal: the skeleton
/// (and the filter input it carries) is never re-rendered, so a slow
/// pull from an earlier keystroke landing mid-typing cannot reset the
/// input to that pull's stale query — the keystroke-drop bug.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) async fn rows_pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<McpFilterQuery>,
    RawQuery(raw): RawQuery,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = crate::server::filter_from_query_or_signal(
        query.filter,
        raw.as_deref(),
        crate::contract::SIGNAL_MCP_FILTER,
    );
    // The fragment carries its own rows container, so the swap
    // replaces that element rather than its inner HTML — an inner swap
    // would nest a second `#mcp-rows` inside the open one.
    Ok(
        crate::server::Fragment::new(rows_fragment(&shared, &session_id, &filter)?)
            .with_swap(crate::contract::MCP_ROWS_SELECTOR, "outer"),
    )
}

/// The viewed session's enabled MCP set, read under one short-lived
/// guard (dropped before any await, per the guard discipline).
#[must_use]
fn enabled_set(
    shared: &crate::server::ServerShared,
    session_id: &str,
) -> std::collections::BTreeSet<String> {
    let Some(id) = jinn_core_types::SessionId::try_from_string(session_id) else {
        return std::collections::BTreeSet::new();
    };
    let guard = shared.read_state();
    guard
        .session
        .get(&id)
        .map_or_default(|session| session.enabled_mcp_servers().clone())
}

/// Builds the TUI-parity rows for one render: the shared builder over
/// the config snapshot's `[mcp.<name>]` section, the session's
/// enabled set, and the runtime cell's effective status projection.
/// Config rows are `(name, description_for_picker())` pairs — the
/// same shape `configured_servers` hands the TUI. Guards are taken
/// under one block and dropped before rendering; the status read is
/// the effective projection so singletons report their shared
/// connection, not a per-session bucket that never fills.
#[must_use]
fn server_rows(shared: &crate::server::ServerShared, session_id: &str) -> Vec<McpServerRow> {
    // The `(name, description_for_picker())` pairs the TUI's
    // `configured_servers` helper hands its own row build — the same
    // shape the shared builder consumes.
    let servers: Vec<(String, String)> = shared
        .config()
        .get::<jinn_preferences_config::schemas::mcp::McpServersConfig>()
        .map_or_default(|config| {
            config
                .0
                .into_iter()
                .map(|(name, server)| (name, server.description_for_picker()))
                .collect()
        });
    let enabled = enabled_set(shared, session_id);
    let statuses: std::collections::BTreeMap<String, jinn_mcp_msg::McpConnectionStatus> = {
        let id = jinn_core_types::SessionId::try_from_string(session_id);
        let runtime = shared.mcp_runtime().read();
        id.as_ref().map_or_default(|id| {
            runtime
                .effective_statuses(id)
                .iter()
                .map(|(name, status)| (name.clone(), *status))
                .collect()
        })
    };
    jinn_mcp_msg::build_rows(&servers, &enabled, &statuses)
}

/// The modal fragment: the overlay with the filter input and the
/// filtered TUI-parity rows — name, launch summary, enabled mark,
/// live status badge. The filter narrows the view only: a toggle's
/// accepted set stays the whole configured set.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn modal_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    crate::templates::render(&crate::templates::McpModalTemplate::with_contract(
        rendered_rows(shared, session_id, filter),
        filter.to_owned(),
        session_id,
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The filtered rows, pre-rendered — the modal and rows fragments'
/// shared body.
fn rendered_rows(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Vec<String> {
    jinn_mcp_msg::filter_rows(&server_rows(shared, session_id), filter)
        .into_iter()
        .map(|filtered| {
            crate::templates::render(&crate::templates::McpRowTemplate::from_filtered(filtered))
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, what = "mcp-row", "webui template render failed; \
                         skipping the row");
                    String::new()
                })
        })
        .collect()
}

/// The rows fragment: the filtered rows alone under their container —
/// the modal effect's re-pull and the toggle POST's swap body. Never
/// the skeleton: the filter input must not re-render mid-typing.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn rows_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    crate::templates::render(&crate::templates::McpRowsTemplate::with_contract(
        rendered_rows(shared, session_id, filter),
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The MCP region's stylesheet: the modal overlay and the rows. The
/// modal geometry mirrors the persona modal's (the same overlay
/// language, at the same stack level); the rows add the enabled mark
/// and the status badge styling.
pub const MCP_CSS: &str = r#"
.mcp-modal { position: fixed; inset: 0; background: var(--scrim);
             display: flex; align-items: flex-start;
             justify-content: center; padding-top: 10vh; z-index: 40; }
.mcp-modal[hidden] { display: none; }
.mcp-modal-inner { background: var(--panel); border: 1px solid var(--border);
                   border-radius: 8px; width: min(560px, 92vw);
                   max-height: 70vh; display: flex; flex-direction: column; }
.mcp-modal-head { display: flex; gap: 8px; padding: 10px;
                  border-bottom: 1px solid var(--border); }
#mcp-filter { flex: 1; background: var(--bg); color: var(--fg);
              border: 1px solid var(--border); border-radius: 6px;
              padding: 6px 8px; font: inherit; }
#mcp-rows { overflow-y: auto; padding: 4px 0; }
.mcp-row { display: flex; gap: 8px; padding: 7px 12px; cursor: pointer;
           align-items: baseline; }
.mcp-row:hover { background: var(--bg-hover); }
.mcp-row .mcp-row-marker { width: 1.2em; color: var(--muted); }
.mcp-row .mcp-row-summary { color: var(--muted); flex: 1; }
.mcp-row .mcp-row-status { font-size: .85em; padding: 1px 8px;
                           border-radius: 999px;
                           border: 1px solid var(--border); }
.mcp-row .mcp-row-status.mcp-status-running { color: var(--accent);
                                              border-color: var(--accent); }
.mcp-row .mcp-row-status.mcp-status-starting { color: var(--muted); }
.mcp-row .mcp-row-status.mcp-status-off { color: var(--muted); opacity: .7; }
.mcp-row .mcp-row-status.mcp-status-dead { color: var(--err);
                                           border-color: var(--err); }
.mcp-row.mcp-row-enabled .mcp-row-name { font-weight: 600; }
.mcp-row.mcp-row-enabled .mcp-row-marker { color: var(--accent); }
.mcp-row mark { background: transparent; color: var(--accent);
                font-weight: 600; }
@media (max-width: 767px) {
  .mcp-row { min-height: 40px; padding: 10px 12px; }
  #mcp-filter { min-height: 40px; }
}
"#;

#[cfg(test)]
mod mcp_region_tests;
