//! The sessions region — the sidebar's session list.
//!
//! Parts declared: guard render + GET pull route + SSE event + page
//! fill (all five except a distinct action surface; its client half
//! is the row-click activate, which swaps the page rather than the
//! region). The list merges live rows from state with archived rows
//! from the store on the pull path; the SSE path renders live rows
//! only (the actor's hot path — the next page pull or a
//! load-completion-triggered refresh carries the archived truth).
//!
//! The list IS the TUI session picker rendered as HTML. Rows are
//! [`jinn_session_store_msg::SessionTreeEntry`]s ordered by the
//! store's tree-aware sort (loaded trees first, then by tree recency,
//! children under parents) — the same list the picker loads. A `?q=`
//! filter runs the picker's own
//! [`jinn_selection_widget::TreePickerState`]: whitespace-split fuzzy
//! terms where every term must match, an ancestor chain pulled in
//! behind a matching child, and strongest-tree-first ranking. One
//! matching implementation, the TUI's; the webui adds none.
//!
//! Browser half: the list's Datastar bindings (`page.html` + rows) — the
//! click delegation and the [`crate::contract::SESSIONS_ID`] swap.

use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use jinn_kernel::common::state::StateReadGuard;
use jinn_picker::make_items_with_hooks;
use jinn_selection_widget::TreePickerState;
use jinn_session_store_msg::SessionState;
use jinn_session_store_msg::SessionTreeEntry;
use jinn_webui_msg::SessionsQuery;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;
use crate::server::Fragment;
use crate::server::HandlerError;
use crate::server::ServerShared;

/// The working-set for the given session map: whether each live
/// session's phase is working, read through
/// [`jinn_slices::PhaseView::is_working`] — the codebase's single busy
/// definition, the same one the TUI throbber and the composer's
/// cancel-button branch read. The sessions surface adds no phase
/// predicate of its own.
///
/// No registry attached → no phase can be non-Idle in a state without
/// the write paths that set it, so the map degrades to all-idle rather
/// than failing — the same signal-not-correctness rule the unread
/// marks follow.
fn working_set_of(
    guard: &StateReadGuard<'_>,
) -> Option<std::collections::HashMap<jinn_core_types::SessionId, bool>> {
    guard.session.slices()?;
    Some(
        guard
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.phase().is_working()))
            .collect(),
    )
}

/// The sessions region.
pub struct SessionsRegion;

impl WebRegion for SessionsRegion {
    fn id(&self) -> &'static str {
        "sessions"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: true,
            pull_route: true,
            sse_event: true,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::Sessions]
    }

    fn settle_kinds(&self) -> &'static [SseKind] {
        // A settle carries a fresh sessions list only when the settle
        // itself changes list facts (activation lands with the settle).
        // It does not: phase settles push transcript + composer, and
        // the list refreshes on its own events.
        &[]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::SESSIONS_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::SESSIONS_ID)
    }

    fn css(&self) -> &'static str {
        SIDEBAR_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        guard: &StateReadGuard<'_>,
    ) -> String {
        sessions_live_fragment(guard)
    }
}

/// The sessions region's stylesheet: the sidebar column and the
/// session rows at desktop widths (unchanged), then — appended after,
/// inside the phone media query — the sidebar's mobile presentation:
/// the off-canvas drawer the menu's drawer-open class summons, its
/// scrim, and the phone touch-target bumps. Everything the media
/// query touches is phone-only; the ≥768px world reads exactly the
/// desktop rules above it.
pub const SIDEBAR_CSS: &str = r#"
#sidebar { width: 280px; min-width: 220px; border-right: 1px solid var(--border);
           background: var(--panel); display: flex; flex-direction: column;
           min-height: 0; }
#sidebar h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .08em;
              color: var(--muted); padding: 12px 14px 4px; margin: 0; }
#sessions { overflow-y: auto; flex: 1; min-height: 0; }
#sessions-filter { margin: 4px 10px 8px; padding: 5px 8px; font: inherit; font-size: 13px;
                   color: var(--fg); background: #14161a; border: 1px solid var(--border);
                   border-radius: 5px; width: calc(100% - 20px); }
#sessions-filter:focus { outline: none; border-color: var(--accent); }
.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;
        border-bottom: 1px solid var(--border); cursor: pointer; }
.srow:hover { background: #23272f; }
.srow.active { background: var(--accent); }
.srow.active .stitle { color: #14161a; font-weight: 600; }
.srow.active .smeta { color: #14161a; }
/* The unread mark: a settle landed while another session was viewed.
   Placed after `.active` so equal specificity resolves in unread's
   favor should the two classes ever co-occur (flag discipline keeps
   them apart; the CSS is the backstop). */
.srow.unread { background: #e3b341; }
.srow.unread .stitle { color: #14161a; }
.srow.unread .smeta { color: #3d3320; }
.srow .stitle { display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.srow .smeta { font-size: 11px; color: var(--muted); }
.srow.bot .smeta::after { content: " · bot"; color: #d4a72c; }
.srow.active .smeta::after { color: #14161a; }
.srow.unread .smeta::after { color: #3d3320; }
/* The working spinner: the row-level mark that a turn is in flight —
   streaming, tools, or cancelling, one class for all three (the busy
   definition is PhaseView::is_working; the web list never re-derives
   it). Placed after .active/.unread so a working row wearing a fill
   keeps its fill; the spinner itself carries the accent color the
   composer's busy spinner uses, then the fill-scoped ink rules give
   it the fill's dark ink on accent and unread rows — every element
   placed on a fill sets its own color. */
.srow.working .working { display: inline-block; width: 10px; height: 10px;
                          margin-right: 6px; vertical-align: -1px;
                          border: 2px solid var(--accent); border-top-color: transparent;
                          border-radius: 50%;
                          animation: webui-spin .8s linear infinite; }
.srow.active .working { border-color: #14161a; border-top-color: transparent; }
.srow.unread .working { border-color: #14161a; border-top-color: transparent; }
@media (max-width: 767px) {
  #sidebar { position: fixed; top: 0; bottom: 0; left: 0; width: min(80vw, 320px);
             z-index: 40; transform: translateX(-100%); transition: transform .25s ease; }
  #page-root.drawer-open #sidebar { transform: translateX(0); }
  #drawer-scrim { position: fixed; inset: 0; background: rgba(0,0,0,.55); z-index: 30;
                  display: block; }
  #drawer-scrim[hidden] { display: none; }
  .srow { padding: 12px 14px; }
  .srow .stitle { min-height: 22px; }
  #sessions-filter { margin: 8px 10px; padding: 9px 10px; font-size: 16px;
                     width: calc(100% - 20px); }
}
"#;

/// `GET /web/sessions` — the sessions fragment (live + archived),
/// narrowed by `?q=` when the client names a filter.
async fn pull_handler(
    AxumState(shared): AxumState<ServerShared>,
    AxQuery(query): AxQuery<SessionsQuery>,
) -> Result<Fragment, HandlerError> {
    Ok(
        Fragment::new(sessions_fragment(&shared, query.filter_text()).await?)
            .with_swap("#sessions", "inner"),
    )
}

/// The unread marks for the given session map: the shared cell's
/// payload, read through the map's attached registry. `None` when no
/// registry is attached — a state without the webui cell renders no
/// marks rather than failing (the mark is signal, not correctness).
fn unread_marks_of(guard: &StateReadGuard<'_>) -> Option<jinn_webui_msg::UnreadSessionMarks> {
    let slices = guard.session.slices()?;
    slices
        .reader::<jinn_webui_msg::UnreadSessionMarks>(&jinn_webui_msg::webui_unread_slot())
        .map(|cell| cell.read().clone())
}

/// The sessions-list fragment from a state guard — the fan-out's
/// refresh shape.
///
/// Live rows only: the fan-out renders on the actor's hot path, and
/// the store's summary set changes rarely enough that the next page
/// pull (or a `SessionLoadCompleted`-triggered page refresh) carries
/// the archived truth.
pub fn sessions_live_fragment(guard: &StateReadGuard<'_>) -> String {
    let active_id = guard.session.active_session_id().clone();
    let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
        .session
        .iter()
        .map(|(id, session)| (id.clone(), session.is_bot()))
        .collect();
    let unread_marks = unread_marks_of(guard);
    let working = working_set_of(guard);
    let rows = picker_rows(
        live_entries(guard),
        &bots,
        None,
        &active_id,
        unread_marks.as_ref(),
        working.as_ref(),
    );
    crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
        .unwrap_or_else(|error| {
            tracing::warn!(%error, what = "sessions", "webui template render failed; \
                 serving a blank fragment");
            String::new()
        })
}

/// The sessions list for a page render or a GET pull: live rows from
/// state, archived rows from the store — the picker's full tree.
/// `filter` runs the TUI session picker's matching (its
/// `TreePickerState`, verbatim): every whitespace-split term must
/// fuzzy-match a row's title, a matching child pulls its ancestor
/// chain in, and trees rank strongest-match first. Every row's
/// `active` flag is computed per row from the instance's active
/// session id, so an archived row can never wear the active styling
/// on the active session's page. Store summaries are awaited on the
/// request path (request-scoped; no command round-trip); a store
/// failure degrades to the live-only list — a web session list must
/// not 500 because a disk read hiccuped.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub async fn sessions_fragment(
    shared: &ServerShared,
    filter: Option<&str>,
) -> Result<String, HandlerError> {
    let (mut entries, mut bots, active_id, unread_marks, working) = {
        let guard = shared.read_state();
        let active_id = guard.session.active_session_id().clone();
        let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.is_bot()))
            .collect();
        let unread_marks = unread_marks_of(&guard);
        let working = working_set_of(&guard);
        (live_entries(&guard), bots, active_id, unread_marks, working)
    };

    // Archived rows: skip ids already live; the merged tree is the one
    // shape both render paths consume. The store's own list already
    // merges its live sessions the same way, so the append is the
    // delta, not a duplicate.
    if let Ok(summaries) = shared.load_summaries().await {
        append_summaries(&mut entries, &mut bots, summaries);
    }
    let rows = picker_rows(
        entries,
        &bots,
        filter,
        &active_id,
        unread_marks.as_ref(),
        working.as_ref(),
    );

    crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
        .map_err(|e| crate::server::map_render_error(&e))
}

/// The live sessions as picker entries, in the store's tree-aware
/// order (loaded first, tree recency, children under parents). Every
/// field — title, parent, state, timestamps, project — reads from the
/// session itself, so a live session renders exactly what it would
/// render as a stored one.
fn live_entries(guard: &StateReadGuard<'_>) -> Vec<SessionTreeEntry> {
    let theme = guard.frontend.theme.clone();
    let mut entries: Vec<SessionTreeEntry> = guard
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
    jinn_session_store::session_entries::sort_entries_tree_aware(&mut entries);
    jinn_session_store_msg::apply_project_column_width(&mut entries);
    entries
}

/// Appends the store's summaries that state does not already hold,
/// then re-applies the tree-aware order and the project column width
/// over the merged set — the same load path the TUI picker runs.
/// Summary bot-ness merges into `bots` for the rows the store alone
/// serves.
fn append_summaries(
    entries: &mut Vec<SessionTreeEntry>,
    bots: &mut std::collections::HashMap<jinn_core_types::SessionId, bool>,
    summaries: Vec<jinn_session_store_msg::SessionSummary>,
) {
    let live_ids: std::collections::HashSet<jinn_core_types::SessionId> = entries
        .iter()
        .map(|entry| entry.session_id.clone())
        .collect();
    let theme = entries
        .first()
        .map_or_else(jinn_theme::default_theme, |first| first.theme.clone());
    for summary in summaries {
        if live_ids.contains(&summary.session_id) {
            continue;
        }
        bots.insert(summary.session_id.clone(), summary.is_bot);
        entries.push(SessionTreeEntry::new(
            summary.session_id,
            summary.title,
            summary.updated_at,
            theme.clone(),
            summary.session_state,
            summary.parent_session,
            summary.project,
        ));
    }
    jinn_session_store::session_entries::sort_entries_tree_aware(entries);
    jinn_session_store_msg::apply_project_column_width(entries);
}

/// Renders the entry list as HTML rows — the list IS the picker.
///
/// An absent or empty filter keeps the picker's unfiltered shape: the
/// tree-aware order as loaded. A filter runs
/// [`TreePickerState::insert_text`], the picker's own re-filter:
/// whitespace-split fuzzy terms (every term must match a row's
/// title), ancestor chains of matching children pulled in, visible
/// trees ranked by their best match score. The rows come back in the
/// picker's DFS order with tree depth metadata, so the web list nests
/// children under parents exactly as the picker draws them.
fn picker_rows(
    entries: Vec<SessionTreeEntry>,
    bots: &std::collections::HashMap<jinn_core_types::SessionId, bool>,
    filter: Option<&str>,
    active_id: &jinn_core_types::SessionId,
    unread_marks: Option<&jinn_webui_msg::UnreadSessionMarks>,
    working: Option<&std::collections::HashMap<jinn_core_types::SessionId, bool>>,
) -> Vec<jinn_webui_msg::SessionListItem> {
    let matches_filter = filter.is_some_and(|text| !text.is_empty());
    let visible: Vec<(SessionTreeEntry, usize)> = if matches_filter {
        let filter_text = filter.unwrap_or_default();
        let wrapped = make_items_with_hooks(
            entries,
            jinn_picker::PickerItemHooks::new()
                .row(jinn_session_store_msg::session_row)
                .search(|entry: &SessionTreeEntry| entry.title.clone()),
        );
        let mut tree = TreePickerState::with_items(wrapped);
        tree.insert_text(filter_text);
        (0..tree.filtered_count())
            .filter_map(|idx| {
                let depth = tree.visible_entry(idx)?.depth;
                tree.filtered_item(idx)
                    .map(|item| (item.entry().clone(), depth))
            })
            .collect()
    } else {
        entries.into_iter().map(|entry| (entry, 0)).collect()
    };

    visible
        .into_iter()
        .map(|(entry, depth)| jinn_webui_msg::SessionListItem {
            is_bot: bots.get(&entry.session_id).copied().unwrap_or(false),
            active: entry.session_id == *active_id,
            loaded: entry.session_state == SessionState::Loaded,
            unread: unread_marks
                .as_ref()
                .is_some_and(|marks| marks.is_marked(&entry.session_id)),
            // The working flag is live-phase truth: only a session in
            // state carries it (the working-set is built from the live
            // map), so an archived row defaults to idle — an archived
            // session has no turn in flight by definition.
            working: working
                .as_ref()
                .is_some_and(|set| set.get(&entry.session_id).copied().unwrap_or(false)),
            session_id: entry.session_id,
            title: entry.title,
            depth,
        })
        .collect()
}
