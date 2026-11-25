//! The session archive region — the sidebar's Archive button and its
//! modal over every stored session.
//!
//! Parts declared: GET pull route only. No settle render (the modal is
//! pull-opened, never part of a settle pass) and no SSE event (the
//! modal re-pulls on open and on every filter keystroke; nothing is
//! pushed to it). No page fill (the modal swaps into its own host
//! span, the picker pattern).
//!
//! The rows are the whole session store as the TUI session picker
//! shows it: every session — loaded **and** archived — tree-nested
//! (subagent children under the parent that spawned them), with the
//! project column and a loaded/archived tag. The sidebar's sessions
//! list carries loaded sessions only; this picker is how an archived
//! one is reached.
//!
//! Filtering is the TUI picker's own: [`TreePickerState::insert_text`]
//! over the same `SessionTreeEntry` rows the TUI builds —
//! whitespace-split fuzzy terms (each must match a title), matching
//! children surfacing their ancestor chain, visible trees ranked by
//! best match. Nothing here re-implements matching; the picker's
//! behavior is delegated, so the two surfaces cannot disagree.
//!
//! The result is bounded: at most [`ROW_CAP`] rows render, taken as a
//! prefix of the picker's own order (the strongest matches first; a
//! prefix of a DFS list keeps every shown child's parent). The modal's
//! DOM is bounded by that cap regardless of store size — the property
//! the old 3 MB sidebar merge lacked. The filter's re-pull is
//! **rows-only** (`GET /web/session-archive/rows?filter=…` into
//! `#archive-rows`, debounce-modified): the filter input is never
//! re-rendered mid-typing — the skills rows-only rule.

use axum::extract::Query as AxQuery;
use axum::extract::RawQuery;
use axum::extract::State as AxumState;
use jinn_picker::PickerItemHooks;
use jinn_picker::make_items_with_hooks;
use jinn_selection_widget::TreePickerState;
use jinn_session_store_msg::SessionState;
use jinn_session_store_msg::SessionTreeEntry;
use jinn_webui_msg::SseKind;
use serde::Deserialize;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The most rows one render carries — a prefix of the picker's order.
/// Bounds the modal's DOM and the response size independently of how
/// many sessions the store holds.
pub(crate) const ROW_CAP: usize = 50;

/// The session archive region.
pub struct SessionArchiveRegion;

impl WebRegion for SessionArchiveRegion {
    fn id(&self) -> &'static str {
        "session-archive"
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
            pattern: crate::contract::ARCHIVE_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into its own host span in the page shell
        // (`ARCHIVE_MODAL_HOST_ID`), not a page-shell inline region —
        // the host-not-container rule every modal follows, so opening
        // it can never destroy the button that opened it.
        None
    }

    fn css(&self) -> &'static str {
        ARCHIVE_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the settle-render part: the modal is
        // pull-opened from the sidebar button and never appears in a
        // page render or a settle push.
        String::new()
    }
}

/// `GET /web/session-archive` — the archive modal fragment. The
/// client pulls this on open (the sidebar's Archive button).
///
/// # Errors
///
/// Returns the handler error; the caller maps it to its status.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let html = open_fragment(&shared).await?;
    Ok(crate::server::Fragment::new(html)
        .with_swap(crate::contract::ARCHIVE_MODAL_HOST_SELECTOR, "inner"))
}

/// The open pull's body — every row the store holds, unfiltered,
/// bounded — split from the axum wrapper so the region tests drive it
/// directly.
///
/// # Errors
///
/// Returns the handler error when the store or a render fails.
pub(crate) async fn open_fragment(
    shared: &crate::server::ServerShared,
) -> Result<String, crate::server::HandlerError> {
    modal_fragment(shared, "").await
}

/// `GET /web/session-archive/rows?filter=…` — the modal effect's
/// re-pull target. It answers with the **rows fragment only**,
/// swapping the rows container inside the open modal: the skeleton
/// (and the filter input it carries) is never re-rendered, so a slow
/// pull from an earlier keystroke landing mid-typing cannot reset the
/// input to that pull's stale query — the keystroke-drop bug.
///
/// # Errors
///
/// Returns the handler error; the caller maps it to its status.
pub(crate) async fn rows_pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxQuery(query): AxQuery<ArchiveFilterQuery>,
    RawQuery(raw): RawQuery,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    // The effect's `@get` carries the signals dump, not a `filter`
    // query — the same signal fallback the skills rows pull rides.
    let filter = crate::server::filter_from_query_or_signal(
        query.filter,
        raw.as_deref(),
        crate::contract::SIGNAL_ARCHIVE_FILTER,
    );
    Ok(
        crate::server::Fragment::new(rows_fragment(&shared, &filter).await?)
            // The fragment carries its own rows container, so the swap
            // replaces that element rather than its inner HTML — an
            // inner swap would nest a second `#archive-rows` inside the
            // open one.
            .with_swap(crate::contract::ARCHIVE_ROWS_SELECTOR, "outer"),
    )
}

/// The rows pull's query: the optional `filter` parameter.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArchiveFilterQuery {
    /// The filter text; absent or empty renders the most recent rows
    /// in tree order.
    #[serde(default)]
    pub filter: Option<String>,
}

/// Every session the store holds as picker entries, merged the way the
/// TUI picker merges them: live state wins for a loaded session (its
/// truth outranks the store's possibly stale row), the store serves
/// every other. Tree-aware order (loaded trees first, tree recency).
async fn every_entry(shared: &crate::server::ServerShared) -> Vec<SessionTreeEntry> {
    // Live entries first, under a short-lived guard dropped before the
    // store await (the guard discipline — a guard must never cross an
    // `.await`).
    let mut entries = {
        let guard = shared.state().read();
        let theme = guard.frontend.theme.clone();
        let mut live: Vec<SessionTreeEntry> = guard
            .session
            .iter()
            .map(|(id, session)| {
                SessionTreeEntry::new(
                    id.clone(),
                    session.title().unwrap_or("Untitled Session").to_owned(),
                    *session.updated_at(),
                    theme.clone(),
                    session.session_state(),
                    session.parent_session().clone(),
                    session.project().cloned(),
                )
            })
            .collect();
        jinn_session_store::session_entries::sort_entries_tree_aware(&mut live);
        live
    };
    // Then the store's rows for everything not live.
    let live_ids: std::collections::HashSet<jinn_core_types::SessionId> = entries
        .iter()
        .map(|entry| entry.session_id.clone())
        .collect();
    let theme = entries
        .first()
        .map_or_else(jinn_theme::default_theme, |first| first.theme.clone());
    if let Ok(summaries) = shared.load_archive_rows().await {
        entries.extend(
            summaries
                .into_iter()
                .filter(|summary| !live_ids.contains(&summary.session_id))
                .map(|summary| {
                    SessionTreeEntry::new(
                        summary.session_id,
                        summary.title,
                        summary.updated_at,
                        theme.clone(),
                        summary.session_state,
                        summary.parent_session,
                        summary.project,
                    )
                }),
        );
        jinn_session_store::session_entries::sort_entries_tree_aware(&mut entries);
    }
    jinn_session_store_msg::apply_project_column_width(&mut entries);
    entries
}

/// One visible row of the archive picker — a picker entry with its
/// tree depth and the title's matched-character runs.
pub(crate) struct ArchiveRow {
    /// The session's id.
    pub session_id: jinn_core_types::SessionId,
    /// Display title.
    pub title: String,
    /// Tree depth (0 for a root) — children indent under parents.
    pub depth: usize,
    /// The project column's display text (empty when none).
    pub project: String,
    /// The width every row's project column shares (the longest
    /// project name), so the columns align.
    pub project_width: usize,
    /// Whether the session is loaded (vs archived-only).
    pub loaded: bool,
    /// The title's matched byte indices under the filter — the
    /// picker's own fuzzy match; empty when unfiltered.
    pub match_indices: Vec<usize>,
}

/// The filtered, bounded rows — the modal and rows fragments' shared
/// body. An empty filter keeps the picker's unfiltered tree order; a
/// filter runs the picker's own re-filter. Either way the result is
/// the first [`ROW_CAP`] visible entries, each with its tree depth.
pub(crate) fn archive_rows(entries: Vec<SessionTreeEntry>, filter: &str) -> Vec<ArchiveRow> {
    let wrapped = make_items_with_hooks(
        entries,
        PickerItemHooks::new()
            .row(jinn_session_store_msg::session_row)
            .search(|entry: &SessionTreeEntry| entry.title.clone()),
    );
    let mut tree = TreePickerState::with_items(wrapped);
    if !filter.is_empty() {
        tree.insert_text(filter);
    }
    (0..tree.filtered_count())
        .filter_map(|idx| {
            let entry = tree.filtered_item(idx)?.entry();
            let depth = tree.visible_entry(idx)?.depth;
            Some(ArchiveRow {
                session_id: entry.session_id.clone(),
                title: entry.title.clone(),
                depth,
                project: entry.project_display.clone(),
                project_width: entry.project_width,
                loaded: entry.session_state == SessionState::Loaded,
                match_indices: tree
                    .filtered_match_indices(idx)
                    .map_or_else(Vec::new, <[usize]>::to_vec),
            })
        })
        .take(ROW_CAP)
        .collect()
}

/// The modal fragment: the overlay with the filter input and the
/// unfiltered, bounded rows.
///
/// # Errors
///
/// Returns the handler error when the store or a render fails.
pub(crate) async fn modal_fragment(
    shared: &crate::server::ServerShared,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let entries = every_entry(shared).await;
    let rows = archive_rows(entries, filter);
    crate::templates::render(&crate::templates::ArchiveModalTemplate::with_contract(
        filter.to_owned(),
        render_rows(&rows)?,
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The rows fragment: the filtered rows alone under their container —
/// the modal effect's re-pull's swap body. Never the skeleton: the
/// filter input must not re-render mid-typing.
///
/// # Errors
///
/// Returns the handler error when the store or a render fails.
pub(crate) async fn rows_fragment(
    shared: &crate::server::ServerShared,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let entries = every_entry(shared).await;
    crate::templates::render(&crate::templates::ArchiveRowsTemplate::with_contract(
        render_rows(&archive_rows(entries, filter))?,
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// Renders the row templates for the picker's visible rows — match
/// highlighting is computed here, over the same tree the rows came
/// from, so the highlighted runs are the picker's own matched
/// characters.
///
/// # Errors
///
/// Returns the handler error when a row render fails.
fn render_rows(rows: &[ArchiveRow]) -> Result<Vec<String>, crate::server::HandlerError> {
    rows.iter()
        .map(|row| {
            let segments =
                jinn_selection_widget::split_matched_runs(&row.title, &row.match_indices);
            crate::templates::render(&crate::templates::ArchiveRowTemplate::from_row(
                row, segments,
            ))
            .map_err(|error| crate::server::map_render_error(&error))
        })
        .collect()
}

/// The session archive region's stylesheet: the modal overlay, the
/// filter input, and the rows. The modal geometry mirrors the applets
/// modal's (the same overlay language, at the same stack level —
/// registered after it, so ties resolve in this region's favor); every
/// colour is a `var(--token)` (the palette law: literals live in
/// BASE_CSS).
#[cfg(test)]
mod session_archive_tests;

pub const ARCHIVE_CSS: &str = r#"
.archive-modal { position: fixed; inset: 0; background: var(--scrim);
                 display: flex; align-items: flex-start;
                 justify-content: center; padding-top: 10vh; z-index: 40; }
.archive-modal[hidden] { display: none; }
.archive-modal-inner { background: var(--panel); border: 1px solid var(--border);
                       border-radius: 8px; width: min(680px, 92vw);
                       max-height: 70vh; display: flex; flex-direction: column; }
.archive-modal-head { display: flex; gap: 8px; padding: 10px;
                      border-bottom: 1px solid var(--border); }
#archive-filter { flex: 1; background: var(--bg); color: var(--fg);
                  border: 1px solid var(--border); border-radius: 6px;
                  padding: 6px 8px; font: inherit; font-size: 13px; }
#archive-rows { overflow-y: auto; padding: 4px 0; }
.archive-row { display: flex; align-items: center; gap: 10px; padding: 7px 12px;
               color: var(--fg); text-decoration: none; }
.archive-row:hover { background: var(--bg-hover); }
.archive-row .archive-project { color: var(--muted); flex: 0 0 auto;
                                font-size: .9em; }
.archive-row .archive-title { flex: 1 1 auto; overflow: hidden;
                              text-overflow: ellipsis; white-space: nowrap; }
.archive-row .archive-state { color: var(--muted); font-size: .85em;
                              flex: 0 0 auto; }
.archive-row.archived .archive-title { color: var(--muted); }
.archive-empty { padding: 12px; color: var(--muted); }
.archive-row mark { background: transparent; color: var(--accent);
                    font-weight: 600; }
#archive-button { width: 100%; margin: 6px 0; padding: 6px 8px;
                  background: var(--bg); color: var(--fg);
                  border: 1px solid var(--border); border-radius: 6px;
                  font: inherit; cursor: pointer; }
#archive-button:hover { background: var(--bg-hover); }
@media (max-width: 767px) {
  .archive-row { min-height: 40px; }
  #archive-filter { min-height: 40px; font-size: 16px; }
}
"#;
