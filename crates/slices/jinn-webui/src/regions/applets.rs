//! The applets region — the hamburger menu's Applets item and its
//! modal over the saved-app catalog.
//!
//! Parts declared: GET pull route only. No settle render (the modal
//! is pull-opened, never part of a settle pass) and no SSE event
//! (the applets slice's own republisher folds the actors' events
//! into its cells; the web never subscribes — one declarant per
//! schema, the fan-out stays out). No page fill (the modal swaps
//! into its own host span, the persona/skills pattern).
//!
//! The rows are the saved-app catalog as the TUI `<leader>sa` picker
//! shows it: one row per catalog entry in the store actor's
//! recency-newest-first order, each a `target="_blank"` link to the
//! applets server's resolved bind (`http://<bind>/applets/<id>/`).
//! The link is the whole open gesture — the operator already
//! configured the applets bind on a network they can reach, so no
//! proxy and no host-side launch exists here. A catalog entry whose
//! id fails `is_valid_app_id` renders the disabled row (no `href`):
//! the gate closes the modal's one injection surface against a
//! hand-edited `applets.json`.
//!
//! Freshness on open rides the GET itself: the pull snapshots both
//! cells, publishes `RescanApplets` and `WakeAppletsServer` on the
//! bus, and waits — bounded by [`CELLS_WAIT_DEADLINE`] — until the
//! catalog or the serving phase differs from the snapshot, then
//! renders from the cells as they stand. On deadline the fragment is
//! whatever the cells hold, never an error: a GET cannot await a bus
//! round-trip, and the deadline trades one short pause on open for
//! TUI-parity freshness without a new SSE kind.
//!
//! Filtering is server-side over the applet title with the shared
//! fuzzy match core (`jinn_selection_widget::fuzzy_filter`, the
//! `SkimMatcherV2` matcher the persona modal's `filter_rows` rides —
//! per-term AND, score-descending). The filter's re-pull is
//! **rows-only** (`GET /web/applet-picker/rows?filter=…` into
//! `#applet-rows`, debounce-modified): the filter input is never
//! re-rendered mid-typing — the skills/tools rows-only rule.
//!
//! When the picker cell's `serving` is `None` (the dormant phase),
//! every row renders as a disabled `<span>` with the offline
//! sentence as its `title`, and the status line reads
//! `<server offline>`; online, the status line reads
//! `serving http://<bind>`. An empty catalog renders exactly one
//! "No applets saved yet" row, the status line still speaking.
//!
//! The menu item lives in the menu region (this feature's opening
//! gesture); this region owns the modal fragment and its pulls.

use axum::extract::Query as AxQuery;
use axum::extract::RawQuery;
use axum::extract::State as AxumState;
use jinn_applets_msg::AppletEntry;
use jinn_applets_msg::AppletsCatalog;
use jinn_webui_msg::SseKind;
use serde::Deserialize;
use std::time::Duration;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// How long the open pull waits for the rescan/wake round-trip before
/// rendering the cells as they stand. The happy path is the store
/// and server actors' dispatch latency (microseconds); the budget
/// only ever binds on a wedged actor or a first open whose wake has
/// not landed yet.
pub(crate) const CELLS_WAIT_DEADLINE: Duration = Duration::from_millis(1000);

/// How often the bounded wait polls the cells for a change.
const CELLS_WAIT_POLL: Duration = Duration::from_millis(25);

/// The applets region.
pub struct AppletsRegion;

impl WebRegion for AppletsRegion {
    fn id(&self) -> &'static str {
        "applets"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: false,
            page_fill: false,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::APPLET_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into its own host span in the page shell
        // (`APPLET_MODAL_HOST_ID`), not a page-shell inline region —
        // the host-not-container rule every modal follows, so opening
        // it can never destroy the menu item that opened it.
        None
    }

    fn css(&self) -> &'static str {
        APPLETS_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the settle-render part: the modal is
        // pull-opened from the menu item and never appears in a page
        // render or a settle push.
        String::new()
    }
}

/// One cell snapshot under one short-lived guard, dropped before any
/// await (the guard discipline — a `parking_lot` guard must never
/// cross an `.await`).
fn catalog_snapshot(shared: &crate::server::ServerShared) -> AppletsCatalog {
    shared.applets_catalog().read().clone()
}

/// The serving phase's snapshot under one short-lived guard.
fn serving_snapshot(shared: &crate::server::ServerShared) -> Option<String> {
    let guard = shared.applets_picker().read();
    guard.serving.as_ref().map(|address| address.bind.clone())
}

/// `GET /web/applet-picker` — the applets modal fragment. The client
/// pulls this on open (the menu item). The handler snapshots the two
/// cells, publishes the rescan and the wake on the bus, waits bounded
/// for either cell to change, then renders the cells as they stand —
/// on deadline whatever they hold, never an error.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let html = open_fragment(&shared).await?;
    Ok(crate::server::Fragment::new(html)
        .with_swap(crate::contract::APPLET_MODAL_HOST_SELECTOR, "inner"))
}

/// The open pull's body — snapshot, publish the rescan/wake pair,
/// wait bounded on the first observed cell change, render — split
/// from the axum wrapper so the region tests drive it directly.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) async fn open_fragment(
    shared: &crate::server::ServerShared,
) -> Result<String, crate::server::HandlerError> {
    // Snapshot first (guards open and close inside the block), then
    // publish, then wait bounded on the first observed change.
    let catalog_before = catalog_snapshot(shared);
    let serving_before = serving_snapshot(shared);
    shared.bus().publish(jinn_applets_msg::RescanApplets).await;
    shared
        .bus()
        .publish(jinn_applets_msg::WakeAppletsServer)
        .await;
    let changed = await_cells_changed(shared, (&catalog_before, serving_before.as_deref())).await;
    if !changed {
        tracing::debug!(
            budget_ms = CELLS_WAIT_DEADLINE.subsec_millis(),
            "the applets rescan/wake landed no cell change within the budget; \
             rendering the cells as they stand"
        );
    }
    modal_fragment(shared, "")
}

/// `GET /web/applet-picker/rows?filter=…` — the modal effect's
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
    AxQuery(query): AxQuery<AppletFilterQuery>,
    RawQuery(raw): RawQuery,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    // The effect's `@get` carries the signals dump, not a `filter`
    // query — the same signal fallback the skills rows pull rides.
    let filter = crate::server::filter_from_query_or_signal(
        query.filter,
        raw.as_deref(),
        crate::contract::SIGNAL_APPLETS_FILTER,
    );
    Ok(
        crate::server::Fragment::new(rows_fragment(&shared, &filter)?)
            // The fragment carries its own rows container, so the swap
            // replaces that element rather than its inner HTML — an
            // inner swap would nest a second `#applet-rows` inside the
            // open one.
            .with_swap(crate::contract::APPLET_ROWS_SELECTOR, "outer"),
    )
}

/// The pull's query: the optional `filter` parameter — the shared
/// fuzzy filter over the applet title.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppletFilterQuery {
    /// The filter text; absent or empty renders every row in catalog
    /// order.
    #[serde(default)]
    pub filter: Option<String>,
}

/// Polls the two cells until either differs from the snapshot it was
/// given, bounded by [`CELLS_WAIT_DEADLINE`]. Each poll iteration
/// opens and closes its guards inside a block and sleeps guard-free —
/// the await-holding-lock rule. Returns whether a change was observed.
async fn await_cells_changed(
    shared: &crate::server::ServerShared,
    before: (&AppletsCatalog, Option<&str>),
) -> bool {
    let (catalog_before, serving_before) = before;
    let deadline = tokio::time::Instant::now() + CELLS_WAIT_DEADLINE;
    loop {
        {
            // One short guard per cell per poll; both drop before the
            // sleep — no guard crosses an `.await`.
            let catalog_now = catalog_snapshot(shared);
            let serving_now = serving_snapshot(shared);
            if &catalog_now != catalog_before || serving_now.as_deref() != serving_before {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(CELLS_WAIT_POLL).await;
    }
}

/// Builds the applet URL from the picker cell's resolved bind: the
/// same formula the applets slice's `browser::applet_url` uses
/// (resolved bind + `/applets/<id>/` with the trailing slash, so
/// relative sibling references in the artifact resolve). Re-derived
/// here from the contract crate's `ServingAddress` — `jinn-webui`
/// never depends on the applets implementation crate.
#[must_use]
fn applet_href(bind: &str, entry: &AppletEntry) -> Option<String> {
    // The id gate: the scan drops invalid ids, but a hand-edited
    // `applets.json` could still carry one — a refused id renders the
    // disabled row instead of an href, closing the modal's one
    // injection surface.
    if !jinn_applets_msg::is_valid_app_id(&entry.id) {
        return None;
    }
    Some(format!("http://{bind}/applets/{}/", entry.id))
}

/// The filtered rows, pre-rendered — the modal and rows fragments'
/// shared body. An empty filter returns the catalog's own order (the
/// store actor's recency sort); otherwise the shared fuzzy match
/// core orders by score descending. A disabled row (offline, or an
/// id failing the gate) renders the no-href span in both cases.
fn rendered_rows(shared: &crate::server::ServerShared, filter: &str) -> Vec<String> {
    let catalog = catalog_snapshot(shared);
    let serving = serving_snapshot(shared);

    // The fuzzy filter over the titles — the same SkimMatcherV2
    // per-term-AND, score-descending core the persona modal's
    // `filter_rows` rides. Empty filter keeps catalog order; the
    // scored run keeps `fuzzy_filter`'s order (score descending,
    // ties by input order).
    let rows: Vec<(&AppletEntry, Vec<usize>)> = if filter.is_empty() {
        catalog
            .entries
            .iter()
            .map(|entry| (entry, Vec::new()))
            .collect()
    } else {
        let titles: Vec<String> = catalog.entries.iter().map(|e| e.title.clone()).collect();
        let title_refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        jinn_selection_widget::fuzzy_filter(&title_refs, filter)
            .into_iter()
            .filter_map(|hit| {
                catalog
                    .entries
                    .get(hit.index)
                    .map(|entry| (entry, hit.match_indices))
            })
            .collect()
    };

    let bind = serving.as_deref();
    rows.iter()
        .map(|(entry, match_indices)| {
            let href = bind.and_then(|bind| applet_href(bind, entry));
            let segments = jinn_selection_widget::split_matched_runs(&entry.title, match_indices);
            crate::templates::render(&crate::templates::AppletRowTemplate::from_entry(
                entry,
                href.as_deref(),
                bind.is_none(),
                segments,
            ))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "applet-row", "webui template render failed; \
                     skipping the row");
                String::new()
            })
        })
        .collect()
}

/// The modal fragment: the overlay with the filter input, the status
/// line, and the filtered rows — with the single "No applets saved
/// yet" row when the catalog is empty (the status line still speaks).
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn modal_fragment(
    shared: &crate::server::ServerShared,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let catalog = catalog_snapshot(shared);
    let rows = if catalog.entries.is_empty() {
        vec![
            crate::templates::render(&crate::templates::AppletEmptyTemplate)
                .map_err(|error| crate::server::map_render_error(&error))?,
        ]
    } else {
        rendered_rows(shared, filter)
    };
    let (online, bind) = match serving_snapshot(shared) {
        Some(bind) => (true, bind),
        None => (false, String::new()),
    };
    crate::templates::render(&crate::templates::AppletModalTemplate::with_contract(
        rows,
        online,
        bind,
        filter.to_owned(),
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The rows fragment: the filtered rows alone under their container —
/// the modal effect's re-pull's swap body. Never the skeleton: the
/// filter input must not re-render mid-typing.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn rows_fragment(
    shared: &crate::server::ServerShared,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    crate::templates::render(&crate::templates::AppletRowsTemplate::with_contract(
        rendered_rows(shared, filter),
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The applets region's stylesheet: the modal overlay, the status
/// line, and the rows. The modal geometry mirrors the skills modal's
/// (the same overlay language, at the same stack level — registered
/// after it, so ties resolve in this region's favor); every colour is
/// a `var(--token)` (the palette law: literals live in BASE_CSS).
pub const APPLETS_CSS: &str = r#"
.applet-modal { position: fixed; inset: 0; background: var(--scrim);
                display: flex; align-items: flex-start;
                justify-content: center; padding-top: 10vh; z-index: 40; }
.applet-modal[hidden] { display: none; }
.applet-modal-inner { background: var(--panel); border: 1px solid var(--border);
                      border-radius: 8px; width: min(560px, 92vw);
                      max-height: 70vh; display: flex; flex-direction: column; }
.applet-modal-head { display: flex; gap: 8px; padding: 10px;
                     border-bottom: 1px solid var(--border); }
#applet-filter { flex: 1; background: var(--bg); color: var(--fg);
                 border: 1px solid var(--border); border-radius: 6px;
                 padding: 6px 8px; font: inherit; }
#applet-status { color: var(--muted); font-size: .9em; white-space: nowrap;
                 align-self: center; }
#applet-status.applet-status-online { color: var(--ok); }
#applet-rows { overflow-y: auto; padding: 4px 0; }
.applet-row { display: block; padding: 7px 12px; color: var(--fg);
              text-decoration: none; }
.applet-row:hover { background: var(--bg-hover); }
.applet-row-disabled { color: var(--muted); cursor: default; }
.applet-row-disabled:hover { background: transparent; }
.applet-empty { padding: 12px; color: var(--muted); }
.applet-row mark { background: transparent; color: var(--accent);
                   font-weight: 600; }
@media (max-width: 767px) {
  .applet-row { min-height: 40px; padding: 10px 12px; }
  #applet-filter { min-height: 40px; }
}
"#;

#[cfg(test)]
mod applets_region_tests;
