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
use axum::extract::RawQuery;
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

impl SessionsRegion {
    /// The sidebar toggle + rows fragment for the queried view: the
    /// segmented Sessions | Library control over whichever list the
    /// pull names — the GET pull's render shape, narrowed by `?q=`
    /// when the client names a filter.
    pub(crate) async fn sidebar_fragment(
        shared: &ServerShared,
        view: jinn_webui_msg::SessionsView,
        filter: Option<&str>,
    ) -> Result<String, HandlerError> {
        match view {
            jinn_webui_msg::SessionsView::Sessions => sessions_fragment(shared, filter).await,
            jinn_webui_msg::SessionsView::Library => library_fragment(shared, filter).await,
        }
    }
}

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
        // The region render is viewer-agnostic: the wire discards this
        // HTML (the tick signal is the push) and the viewed row is
        // client-side signal styling, so the page's session plays no
        // part in the rows.
        let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.is_bot()))
            .collect();
        let unread_marks = unread_marks_of(guard);
        let working = working_set_of(guard);
        let rows = picker_rows(
            live_listed_entries(guard),
            &bots,
            None,
            unread_marks.as_ref(),
            working.as_ref(),
            &memberships_of(guard),
        );
        crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "sessions", "webui template render failed; \
                     serving a blank fragment");
                String::new()
            })
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
                   color: var(--fg); background: var(--bg); border: 1px solid var(--border);
                   border-radius: 5px; width: calc(100% - 20px); }
#sessions-filter:focus { outline: none; border-color: var(--accent); }
/* The row base: both list shapes share it — the library row is the
   same anchor with its own class hook (the kebab needs a positioned
   ancestor on either list). */
.srow, .lrow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;
               border-bottom: 1px solid var(--border); cursor: pointer; }
.srow:hover, .lrow:hover { background: var(--bg-hover); }
.srow.unread, .lrow.unread { background: var(--mark); }
.srow.unread .stitle, .lrow.unread .stitle { color: var(--on-fill); }
.srow.unread .smeta, .lrow.unread .smeta { color: var(--on-mark-muted); }
/* The viewed fill: the session THIS page has open — two pages viewing
   different sessions each paint their own row, and the landing page
   paints none. Placed AFTER `.unread` so equal specificity resolves
   in the viewed fill's favor should the two classes ever co-occur
   (they do, transiently: a settle marks a session while its page is
   rendering, and the page's clear event races the next list pull —
   in that window the row the user is looking at must read as the
   viewed one, not as unread). */
.srow.active, .lrow.active { background: var(--accent); }
.srow.active .stitle, .lrow.active .stitle { color: var(--on-accent); font-weight: 600; }
.srow.active .smeta, .lrow.active .smeta { color: var(--on-accent); }
.srow .stitle, .lrow .stitle { display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.srow .smeta, .lrow .smeta { font-size: 11px; color: var(--muted); }
.srow.bot .smeta::after, .lrow.bot .smeta::after { content: " · bot"; color: var(--bot); }
.srow.active .smeta::after, .lrow.active .smeta::after { color: var(--on-accent); }
.srow.unread .smeta::after, .lrow.unread .smeta::after { color: var(--on-mark-muted); }
/* The working spinner: the row-level mark that a turn is in flight —
   streaming, tools, or cancelling, one class for all three (the busy
   definition is PhaseView::is_working; the web list never re-derives
   it). Placed after .active/.unread so a working row wearing a fill
   keeps its fill; the spinner itself carries the accent color the
   composer's busy spinner uses, then the fill-scoped ink rules give
   it the fill's dark ink on accent and unread rows — every element
   placed on a fill sets its own color. */
.srow.working .working, .lrow.working .working { display: inline-block; width: 10px; height: 10px;
                          margin-right: 6px; vertical-align: -1px;
                          border: 2px solid var(--accent); border-top-color: transparent;
                          border-radius: 50%;
                          animation: webui-spin .8s linear infinite; }
.srow.active .working, .lrow.active .working { border-color: var(--on-accent); border-top-color: transparent; }
.srow.unread .working, .lrow.unread .working { border-color: var(--on-fill); border-top-color: transparent; }
/* The switch interstitial: the full-viewport loading overlay a
   session-row click raises (the switching signal drives it) until the
   activation response's page swap re-seeds the signal. Stacked above
   the menu panel (z-index 70) — the click may come from the drawer or
   the menu, and the overlay must cover both. Static shell markup,
   hidden by default like the scrims; the click-through it blocks is
   the point: a second activate mid-switch must not fire. */
#switch-overlay { position: fixed; inset: 0; background: var(--scrim);
                  z-index: 80; display: flex; align-items: center;
                  justify-content: center; }
#switch-overlay[hidden] { display: none; }
.switch-card { background: var(--panel); border: 1px solid var(--border);
               border-radius: 8px; padding: 18px 26px;
               display: flex; flex-direction: column; align-items: center;
               gap: 12px; }
.switch-spinner { display: inline-block; width: 18px; height: 18px;
                  border: 3px solid var(--accent); border-top-color: transparent;
                  border-radius: 50%;
                  animation: webui-spin .8s linear infinite; }
.switch-label { color: var(--fg); font-size: 14px; }
/* The error-notice banner: the failed activate/create POST's visible
   half — the fetch-error listener on the page root fills it (the
   switch signal clears and the failure names itself here), styled
   only by tokens like every surface. Served hidden from the server
   like the scrims; `data-show` owns the visibility once bound. */
#error-notice { position: fixed; top: 12px; left: 50%; transform: translateX(-50%);
                z-index: 80; background: var(--bg-badge-err); color: var(--err);
                border: 1px solid var(--err); border-radius: 8px;
                padding: 10px 18px; font-size: 14px; max-width: min(560px, 92vw); }
#error-notice[hidden] { display: none; }
/* The sidebar's view toggle: the segmented Sessions | Library control
   above the filter input. One segment binds its active class to the
   sidebarview signal; the flex layout gives both segments equal
   halves of the column. */
#sessions-view-toggle { display: flex; margin: 4px 10px; gap: 4px; }
#sessions-view-toggle button { flex: 1; padding: 6px 8px; font: inherit; font-size: 13px;
                               color: var(--muted); background: var(--bg);
                               border: 1px solid var(--border); border-radius: 5px;
                               cursor: pointer; }
#sessions-view-toggle button.theme-active { color: var(--on-accent); background: var(--accent);
                                            border-color: var(--accent); }
/* The per-row action surface: the kebab button and its popup. The
   button sits at the row's right edge, floating over the title's
   ellipsis (the row keeps its own layout); the popup anchors under
   it, above the sibling rows, and scrolls when its entries outgrow
   the cap. Stacked above the drawer (z-40) and below the switch
   overlay (z-80) so a phone drawer's open state still shows it. */
.srow, .lrow { position: relative; }
.srow-menu { position: absolute; top: 4px; right: 6px; width: 24px; height: 24px;
             min-width: 24px; min-height: 24px; padding: 0; line-height: 24px;
             text-align: center; font: inherit; color: var(--muted);
             background: transparent; border: none; border-radius: 5px;
             cursor: pointer; }
.srow-menu:hover { background: var(--bg-hover); color: var(--fg); }
.srow.active .srow-menu { color: var(--on-accent); }
.srow-menu-popup { position: absolute; top: 26px; right: 6px; z-index: 50;
                   display: flex; flex-direction: column; min-width: 160px;
                   max-height: 240px; overflow-y: auto;
                   background: var(--panel); border: 1px solid var(--border);
                   border-radius: 8px; box-shadow: 0 6px 18px var(--scrim); }
.srow-menu-popup[hidden] { display: none; }
.srow-menu-popup button { padding: 8px 12px; font: inherit; font-size: 13px; min-height: 32px;
                          text-align: left; color: var(--fg); background: transparent;
                          border: none; border-bottom: 1px solid var(--border);
                          cursor: pointer; white-space: nowrap; }
.srow-menu-popup button:last-child { border-bottom: none; }
.srow-menu-popup button:hover { background: var(--bg-hover); }
@media (max-width: 767px) {
  #sidebar { position: fixed; top: 0; bottom: 0; left: 0; width: min(80vw, 320px);
             z-index: 40; transform: translateX(-100%); transition: transform .25s ease; }
  #page-root.drawer-open #sidebar { transform: translateX(0); }
  #drawer-scrim { position: fixed; inset: 0; background: var(--scrim); z-index: 30;
                  display: block; }
  #drawer-scrim[hidden] { display: none; }
  .srow, .lrow { padding: 12px 14px; }
  .srow .stitle, .lrow .stitle { min-height: 22px; }
  #sessions-filter { margin: 8px 10px; padding: 9px 10px; font-size: 16px;
                     width: calc(100% - 20px); }
  /* Phone-only bumps for the new elements: the kebab and each popup
     entry get a ≥32px tap target, and the popup's stacking slot is
     restated where the drawer exists (above it at 40, below the
     switch overlay at 80). */
  .srow-menu { width: 32px; height: 32px; line-height: 32px; top: 8px; right: 8px; }
  .srow-menu-popup { z-index: 50; top: 40px; right: 8px; }
  .srow-menu-popup button { min-height: 32px; }
}
"#;

/// `GET /web/sessions` — the sidebar fragment (the segmented toggle
/// over the queried view's list), narrowed by `?q=` when the client
/// names a filter. The viewed (blue) row is client-side signal
/// styling on `$viewedSession` — the pull renders one viewer-agnostic
/// shape.
async fn pull_handler(
    AxumState(shared): AxumState<ServerShared>,
    AxQuery(query): AxQuery<SessionsQuery>,
    RawQuery(raw): RawQuery,
) -> Result<Fragment, HandlerError> {
    // The filter rides the signal dump (our debounce rewrite); both
    // shapes degrade gracefully when absent.
    let q = crate::server::filter_from_query_or_signal(
        query.q.clone(),
        raw.as_deref(),
        crate::contract::SIGNAL_SESSIONS_FILTER,
    );
    let q = (!q.is_empty()).then_some(q.as_str());
    // The view rides the same signal dump the filter does (the live
    // client's @get URL is the constant route, so a top-level `view=`
    // exists only on curls and the Rust tests — the dump wins when
    // present, the exact precedence `filter_from_query_or_signal`
    // runs).
    let dump_view =
        crate::server::signal_from_query(raw.as_deref(), crate::contract::SIGNAL_SIDEBAR_VIEW);
    let view = jinn_webui_msg::SessionsQuery {
        q: query.q.clone(),
        view: Some(dump_view.unwrap_or_else(|| query.view().as_query_value().to_owned())),
    }
    .view();
    let fragment = SessionsRegion::sidebar_fragment(&shared, view, q).await?;
    Ok(Fragment::new(fragment).with_swap("#sessions", "inner"))
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

/// The unread-mark count for the given session map — the titlebar
/// sessions button's badge number. Zero when no registry is attached
/// (no registry means no mark write paths, so the count degrades to
/// zero rather than failing — the same signal-not-correctness rule
/// [`unread_marks_of`] follows).
pub(crate) fn unread_count_of(guard: &StateReadGuard<'_>) -> usize {
    unread_marks_of(guard).map_or(0, |marks| marks.count())
}

/// The sessions-list fragment from a state guard — the fan-out's
/// refresh shape.
///
/// Live rows only: the fan-out renders on the actor's hot path, and
/// the store's summary set changes rarely enough that the next page
/// pull (or a `SessionLoadCompleted`-triggered page refresh) carries
/// the archived truth. **No viewer**: the fan-out serves every page
/// at once and the envelope's HTML is dead on the wire (only the
/// `sessionsTick` signal rides), so the render is deliberately
/// discarded and carries no active row at all.
pub fn sessions_live_fragment(guard: &StateReadGuard<'_>) -> String {
    let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
        .session
        .iter()
        .map(|(id, session)| (id.clone(), session.is_bot()))
        .collect();
    let unread_marks = unread_marks_of(guard);
    let working = working_set_of(guard);
    let rows = picker_rows(
        live_listed_entries(guard),
        &bots,
        None,
        unread_marks.as_ref(),
        working.as_ref(),
        &memberships_of(guard),
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
/// chain in, and trees rank strongest-match first. The rows carry no
/// viewer styling: the viewed (blue) row binds client-side to
/// `$viewedSession`, so one list shape serves every page and a
/// navigation recolors it with zero requests. Store summaries are
/// awaited on the request path (request-scoped; no command
/// round-trip); a store failure degrades to the live-only list — a
/// web session list must not 500 because a disk read hiccuped.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub async fn sessions_fragment(
    shared: &ServerShared,
    filter: Option<&str>,
) -> Result<String, HandlerError> {
    let (mut entries, mut bots, unread_marks, working, mut memberships) = {
        let guard = shared.read_state();
        let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.is_bot()))
            .collect();
        let unread_marks = unread_marks_of(&guard);
        let working = working_set_of(&guard);
        let memberships = memberships_of(&guard);
        (
            live_listed_entries(&guard),
            bots,
            unread_marks,
            working,
            memberships,
        )
    };

    // Archived rows: skip ids already live; the merged tree is the one
    // shape both render paths consume. The store's own list already
    // merges its live sessions the same way, so the append is the
    // delta, not a duplicate. Library members are dropped here —
    // `append_summaries` serves both lists, and the library needs its
    // members, so the sessions view filters its own delta.
    if let Ok(summaries) = shared.load_summaries().await {
        let summaries: Vec<_> = summaries
            .into_iter()
            .filter(|summary| !summary.in_library)
            .collect();
        append_summaries(&mut entries, &mut bots, &mut memberships, summaries);
    }
    let rows = picker_rows(
        entries,
        &bots,
        filter,
        unread_marks.as_ref(),
        working.as_ref(),
        &memberships,
    );

    crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
        .map_err(|e| crate::server::map_render_error(&e))
}

/// The library list for a page render or a GET pull: every library
/// member, live and archived alike, most recently updated first — a
/// flat list (no tree nesting), because membership is per session.
///
/// The store's `library_members` query is the source of truth for
/// archived members; the live state supplies each loaded member's
/// current title, bot mark, unread mark, working spinner, and
/// membership flag (the store row a loaded session shadows may lag a
/// live rename or the just-flipped flag — the same precedence the
/// sessions list runs). A store failure degrades to the live-only
/// member list — a library that must not 500 because a disk read
/// hiccuped, the sessions list's own rule.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub async fn library_fragment(
    shared: &ServerShared,
    filter: Option<&str>,
) -> Result<String, HandlerError> {
    let (mut bots, unread_marks, working, mut memberships) = {
        let guard = shared.read_state();
        let bots: std::collections::HashMap<jinn_core_types::SessionId, bool> = guard
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.is_bot()))
            .collect();
        (
            bots,
            unread_marks_of(&guard),
            working_set_of(&guard),
            memberships_of(&guard),
        )
    };
    let mut entries = Vec::new();

    // The store's member rows first — archived members included, the
    // shape live state cannot see. The store's query returns members
    // most recently updated first.
    if let Ok(summaries) = shared.library_members().await {
        append_summaries(&mut entries, &mut bots, &mut memberships, summaries);
    }
    // The live members the store's query may have shadowed or missed
    // (a fresh live session flagged on this page whose persist has not
    // landed). Live state is the truth for a session it holds.
    {
        let guard = shared.read_state();
        let served: std::collections::HashSet<jinn_core_types::SessionId> = entries
            .iter()
            .map(|entry| entry.session_id.clone())
            .collect();
        entries.extend(member_live_entries(&guard, &served));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    let rows = library_rows(
        entries,
        &bots,
        filter,
        unread_marks.as_ref(),
        working.as_ref(),
        &memberships,
    );

    crate::templates::render(&crate::templates::LibraryTemplate::with_contract(rows))
        .map_err(|e| crate::server::map_render_error(&e))
}

/// The live sessions as picker entries, in the store's tree-aware
/// order (loaded first, tree recency, children under parents), library
/// members excluded — the TUI `is_listed` rule (`loaded && !in_library`)
/// mirrored: membership hides a session from the sessions list on
/// every surface. Every field — title, parent, state, timestamps,
/// project — reads from the session itself, so a live session renders
/// exactly what it would render as a stored one.
fn live_listed_entries(guard: &StateReadGuard<'_>) -> Vec<SessionTreeEntry> {
    let theme = guard.frontend.theme.clone();
    let mut entries: Vec<SessionTreeEntry> = guard
        .session
        .iter()
        .filter(|(_, session)| !session.in_library())
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

/// The live library members as picker entries, skipping ids the
/// served set already covers — the live truth that outranks the
/// store's (possibly stale) row for a loaded member. Most recently
/// updated first, matching the store query's own order.
fn member_live_entries(
    guard: &StateReadGuard<'_>,
    served: &std::collections::HashSet<jinn_core_types::SessionId>,
) -> Vec<SessionTreeEntry> {
    let theme = guard.frontend.theme.clone();
    let mut entries: Vec<SessionTreeEntry> = guard
        .session
        .iter()
        .filter(|(id, session)| session.in_library() && !served.contains(id))
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
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
    entries
}

/// The membership flags the live session map holds — the
/// `SessionListItem.in_library` input for rows state serves, and the
/// kebab entry's label source. No registry attached degrades to an
/// empty map: a state without the library write paths has no members.
fn memberships_of(
    guard: &StateReadGuard<'_>,
) -> std::collections::HashMap<jinn_core_types::SessionId, bool> {
    guard
        .session
        .iter()
        .map(|(id, session)| (id.clone(), session.in_library()))
        .collect()
}

/// Appends the store's summaries that state does not already hold,
/// then re-applies the tree-aware order and the project column width
/// over the merged set — the same load path the TUI picker runs.
/// Summary bot-ness and membership merge into the caller's maps for
/// the rows the store alone serves.
fn append_summaries(
    entries: &mut Vec<SessionTreeEntry>,
    bots: &mut std::collections::HashMap<jinn_core_types::SessionId, bool>,
    memberships: &mut std::collections::HashMap<jinn_core_types::SessionId, bool>,
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
        memberships.insert(summary.session_id.clone(), summary.in_library);
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
///
/// The rows carry no viewer flag: the viewed (blue) row is client-side
/// signal styling on `$viewedSession`, so one row shape serves every
/// page and the instance's active pointer plays no part anywhere.
fn picker_rows(
    entries: Vec<SessionTreeEntry>,
    bots: &std::collections::HashMap<jinn_core_types::SessionId, bool>,
    filter: Option<&str>,
    unread_marks: Option<&jinn_webui_msg::UnreadSessionMarks>,
    working: Option<&std::collections::HashMap<jinn_core_types::SessionId, bool>>,
    memberships: &std::collections::HashMap<jinn_core_types::SessionId, bool>,
) -> Vec<jinn_webui_msg::SessionListItem> {
    let matches_filter = filter.is_some_and(|text| !text.is_empty());
    let visible: Vec<(SessionTreeEntry, usize, bool)> = if matches_filter {
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
            .map(|(entry, depth)| {
                let in_library = memberships.get(&entry.session_id).copied().unwrap_or(false);
                (entry, depth, in_library)
            })
            .collect()
    } else {
        entries
            .into_iter()
            .map(|entry| {
                let in_library = memberships.get(&entry.session_id).copied().unwrap_or(false);
                (entry, 0, in_library)
            })
            .collect()
    };

    visible
        .into_iter()
        .map(
            |(entry, depth, in_library)| jinn_webui_msg::SessionListItem {
                is_bot: bots.get(&entry.session_id).copied().unwrap_or(false),
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
                in_library,
            },
        )
        .collect()
}

/// Renders the library's flat member list as HTML rows — the same row
/// shape the sessions list renders (the kebab button included), with
/// no tree construction: the library is flat by definition.
///
/// A filter runs the picker's own per-row search hook
/// ([`jinn_session_store_msg::session_row`]'s search over the title)
/// — whitespace-split fuzzy terms, every term must match — applied
/// per row in flat form, the same matching function the sessions
/// tree runs, just without ancestor-chain or tree-ranking steps
/// nothing flat has.
fn library_rows(
    entries: Vec<SessionTreeEntry>,
    bots: &std::collections::HashMap<jinn_core_types::SessionId, bool>,
    filter: Option<&str>,
    unread_marks: Option<&jinn_webui_msg::UnreadSessionMarks>,
    working: Option<&std::collections::HashMap<jinn_core_types::SessionId, bool>>,
    memberships: &std::collections::HashMap<jinn_core_types::SessionId, bool>,
) -> Vec<jinn_webui_msg::SessionListItem> {
    let visible: Vec<SessionTreeEntry> = match filter.filter(|text| !text.is_empty()) {
        Some(filter_text) => {
            let wrapped = make_items_with_hooks(
                entries,
                jinn_picker::PickerItemHooks::new()
                    .row(jinn_session_store_msg::session_row)
                    .search(|entry: &SessionTreeEntry| entry.title.clone()),
            );
            let mut tree = TreePickerState::with_items(wrapped);
            tree.insert_text(filter_text);
            (0..tree.filtered_count())
                .filter_map(|idx| tree.filtered_item(idx).map(|item| item.entry().clone()))
                .collect()
        }
        None => entries,
    };

    visible
        .into_iter()
        .map(|entry| {
            let in_library = memberships.get(&entry.session_id).copied().unwrap_or(true);
            jinn_webui_msg::SessionListItem {
                is_bot: bots.get(&entry.session_id).copied().unwrap_or(false),
                loaded: entry.session_state == SessionState::Loaded,
                unread: unread_marks
                    .as_ref()
                    .is_some_and(|marks| marks.is_marked(&entry.session_id)),
                working: working
                    .as_ref()
                    .is_some_and(|set| set.get(&entry.session_id).copied().unwrap_or(false)),
                session_id: entry.session_id,
                title: entry.title,
                depth: 0,
                in_library,
            }
        })
        .collect()
}
