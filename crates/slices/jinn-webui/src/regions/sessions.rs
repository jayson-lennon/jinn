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
//! Both render paths share [`grouped_and_sorted`]: loaded rows form
//! the top group, archived rows the bottom one, within loaded the
//! active session leads and the rest sort by title — the TUI's
//! loaded-before-archived ordering law, mirrored here. The pull path
//! additionally narrows by `?q=` (server-side fuzzy title match, the
//! same matcher the TUI session picker uses).
//!
//! Client half: `webui/client/src/regions/sessions.ts` — the row
//! click delegation and the [`crate::contract::SESSIONS_ID`] swap.

use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use fuzzy_matcher::FuzzyMatcher as _;
use fuzzy_matcher::skim::SkimMatcherV2;
use jinn_kernel::common::state::StateReadGuard;
use jinn_webui_msg::SessionsQuery;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;
use crate::server::Fragment;
use crate::server::HandlerError;
use crate::server::ServerShared;

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

/// The sidebar's stylesheet: the column itself and the session rows.
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
.srow.active { border-left: 3px solid var(--accent); background: #20242c; }
.srow .stitle { display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.srow .smeta { font-size: 11px; color: var(--muted); }
.srow.bot .smeta::after { content: " · bot"; color: #d4a72c; }
"#;

/// `GET /web/sessions` — the sessions fragment (live + archived),
/// narrowed by `?q=` when the client names a filter.
async fn pull_handler(
    AxumState(shared): AxumState<ServerShared>,
    AxQuery(query): AxQuery<SessionsQuery>,
) -> Result<Fragment, HandlerError> {
    Ok(Fragment::new(
        sessions_fragment(&shared, query.filter_text()).await?,
    ))
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
    let rows = guard
        .session
        .iter()
        .map(|(id, session)| live_row(id, session, &active_id))
        .collect();
    let rows = grouped_and_sorted(rows);
    crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
        .unwrap_or_else(|error| {
            tracing::warn!(%error, what = "sessions", "webui template render failed; \
                 serving a blank fragment");
            String::new()
        })
}

/// The sessions list for a page render or a GET pull: live rows from
/// state, archived rows from the store. `filter` narrows the merged
/// list to titles fuzzy-matching it, matched with the TUI session
/// picker's matcher. Every row's `active` flag is computed per row
/// from the instance's active session id, so an archived row can
/// never wear the active styling on the active session's page. Store
/// summaries are awaited on the request path (request-scoped; no
/// command round-trip); a store failure degrades to the live-only
/// list — a web session list must not 500 because a disk read
/// hiccuped.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub async fn sessions_fragment(
    shared: &ServerShared,
    filter: Option<&str>,
) -> Result<String, HandlerError> {
    let mut rows = {
        let guard = shared.read_state();
        let active_id = guard.session.active_session_id().clone();
        guard
            .session
            .iter()
            .map(|(id, session)| live_row(id, session, &active_id))
            .collect::<Vec<_>>()
    };

    // Archived rows: skip ids already live; the merged list is the one
    // shape the template renders. An archived row is never the
    // instance's active session (the active session is always live),
    // so its `active` flag is false by construction.
    if let Ok(summaries) = shared.load_summaries().await {
        for summary in summaries {
            if rows.iter().any(|row| row.session_id == summary.session_id) {
                continue;
            }
            rows.push(jinn_webui_msg::SessionListItem {
                session_id: summary.session_id,
                title: summary.title,
                active: false,
                is_bot: summary.is_bot,
                loaded: false,
            });
        }
    }

    let rows = filtered_and_grouped(rows, filter);

    crate::templates::render(&crate::templates::SessionsTemplate::with_contract(rows))
        .map_err(|e| crate::server::map_render_error(&e))
}

/// Builds one live row. Every field computes from this row's own id
/// and data — the active flag from the row's id against the instance's
/// active session id, never from a page-level comparison.
fn live_row(
    id: &jinn_core_types::SessionId,
    session: &jinn_session_state::ChatSessionState,
    active_id: &jinn_core_types::SessionId,
) -> jinn_webui_msg::SessionListItem {
    jinn_webui_msg::SessionListItem {
        session_id: id.clone(),
        title: session.title().unwrap_or("Untitled Session").to_owned(),
        active: id == active_id,
        is_bot: session.is_bot(),
        loaded: true,
    }
}

/// Narrows the rows to titles fuzzy-matching `filter`, then applies
/// the grouped order. An absent or empty filter short-circuits to the
/// grouping alone — the matcher's empty-pattern behavior never
/// defines the unfiltered case.
fn filtered_and_grouped(
    rows: Vec<jinn_webui_msg::SessionListItem>,
    filter: Option<&str>,
) -> Vec<jinn_webui_msg::SessionListItem> {
    match filter {
        None => grouped_and_sorted(rows),
        Some(text) => {
            let matcher = SkimMatcherV2::default();
            let surviving: Vec<_> = rows
                .into_iter()
                .filter(|row| matcher.fuzzy_match(&row.title, text).is_some())
                .collect();
            grouped_and_sorted(surviving)
        }
    }
}

/// The list's one ordering: loaded rows first, archived rows under
/// them; within loaded, the active session leads, then titles A→Z
/// (case-insensitive); within archived, titles A→Z. Both render
/// paths — the pull fragment and the fan-out's live fragment — call
/// this, so a pushed list and a pulled one share one shape.
fn grouped_and_sorted(
    mut rows: Vec<jinn_webui_msg::SessionListItem>,
) -> Vec<jinn_webui_msg::SessionListItem> {
    rows.sort_by(|a, b| {
        b.loaded
            .cmp(&a.loaded)
            .then_with(|| b.active.cmp(&a.active))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    rows
}
