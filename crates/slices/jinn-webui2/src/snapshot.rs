//! The snapshot: everything a freshly-connected page needs.
//!
//! Built on demand from `State` — the active session, its phase, model
//! label, title, and the newest part of its transcript, plus the
//! loaded-session rows the sidebar shows and the library members its
//! Library view shows. Nothing is cached; a snapshot is always a read of
//! the current state, so a reconnect can never resume onto stale data.
//!
//! The transcript is a bounded tail, not the whole history: the newest
//! entries within the window rule in [`crate::page`], with a count of how
//! many precede them. Older entries load a page at a time as the reader
//! scrolls up.

use crate::{page, revision::RevisionTracker, settle::SettleTracker};
use jinn_core_types::SessionId;
use jinn_kernel::{State, StateReadGuard};
use jinn_session_msg::SessionOrigin;
use jinn_webui2_msg::{
    PhaseBody, SessionRowBody, SessionRowOrigin, SnapshotBody, WEBUI2_PROTOCOL_VERSION,
};

/// The title a session row shows when the session has none.
const UNTITLED: &str = "Untitled Session";

/// A snapshot together with the window of entries it carried, so the
/// caller records as delivered exactly what the frame holds.
#[derive(Debug, Clone)]
pub struct SnapshotFrame {
    /// The body, as the frame carries it.
    pub body: SnapshotBody,
    /// The transcript window the body's `entries` came from.
    pub window: crate::page::EntryWindow,
}

/// Reads the current snapshot out of state, stamped with the session's revision.
///
/// One read of state: the transcript tail, the session's phase, model and
/// title, and the sidebar rows are all drawn from the same instant, so a
/// frame can never describe two different moments.
#[must_use]
pub fn snapshot(
    state: &State,
    revisions: &RevisionTracker,
    settles: &SettleTracker,
) -> SnapshotFrame {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    let (sessions, library, hydrating) = rows_from(&guard, &active_id, settles);
    let revision = revisions.current(&active_id);
    let theme = persisted_theme(&guard);

    let Some(active) = guard.session.get(&active_id) else {
        return SnapshotFrame {
            window: crate::page::EntryWindow::empty(),
            body: SnapshotBody {
                protocol: WEBUI2_PROTOCOL_VERSION,
                session_id: Some(active_id.clone()),
                phase: PhaseBody {
                    session_id: active_id,
                    phase: jinn_slices::PhaseView::Idle,
                    queue_len: 0,
                },
                model_label: String::new(),
                title: String::new(),
                revision,
                entries: Vec::new(),
                entries_before: 0,
                sessions,
                library,
                hydrating,
                theme,
            },
        };
    };

    let window = page::tail(active.history());
    let entries_before = window.entries_before;
    // The body is built first: the window's entries are cloned into it
    // before the window itself moves into the frame.
    let body = SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(active_id.clone()),
        phase: PhaseBody {
            session_id: active_id,
            phase: active.phase(),
            queue_len: active.queue_len(),
        },
        model_label: active.model_selection().to_string(),
        title: active.title().unwrap_or(UNTITLED).to_owned(),
        revision,
        entries: window.entries.clone(),
        entries_before,
        sessions,
        library,
        hydrating,
        theme,
    };
    SnapshotFrame { body, window }
}

/// Builds both sidebar lists from live state: the loaded sessions that are
/// not library members, as a tree, and the loaded library members, as a flat
/// list. The third element is whether startup hydration is still running —
/// read in the same instant as the rows, so a frame can never say "complete"
/// beside a partial list, or "loading" beside the final one.
///
/// The Sessions list is the terminal sidebar's tree, through the same
/// builder: roots newest-first, every child under its parent — a fork, a
/// subagent, or a bot alike, because what nests a row is its parent link,
/// not how it was made. Each row carries its depth so the page indents it
/// without building a tree of its own.
///
/// One builder, one order: the page is sent the tree the terminal would
/// draw, and never re-derives it.
fn rows_from(
    guard: &StateReadGuard<'_>,
    active_id: &SessionId,
    settles: &SettleTracker,
) -> (Vec<SessionRowBody>, Vec<SessionRowBody>, bool) {
    let mut listed = ListedRows::default();
    let mut library = Vec::new();
    for (id, session) in guard.session.iter() {
        let row = SessionRowBody {
            session_id: id.clone(),
            title: session.title().unwrap_or(UNTITLED).to_owned(),
            is_active: id == active_id,
            is_busy: session.phase().is_working(),
            in_library: session.in_library(),
            last_settled_at: settles.last_settled(id),
            parent_session: session.parent_session().clone(),
            origin: row_origin(session.origin()),
            depth: 0,
        };
        // Two lists, one rule: a member is the Library's and never the
        // tree's, and the tree takes every other loaded session. An archived
        // session still in the map is in neither, and is not sent.
        match (session.is_listed(), session.in_library()) {
            (true, _) => listed.push(session, id, row),
            (false, true) => library.push(row),
            (false, false) => {}
        }
    }
    // The flag the terminal's throbber reads on every tick. A push-only page
    // cannot poll it, so it rides on every list frame instead.
    (
        listed.in_tree_order(),
        library,
        guard.session.is_startup_hydrating(),
    )
}

/// The wire's spelling of a session's kind.
///
/// The contract crate cannot name the session crate's origin (a message
/// crate never depends on another), so the twin is written here, variant for
/// variant, and the two spellings are pinned to each other by a test.
fn row_origin(origin: SessionOrigin) -> SessionRowOrigin {
    match origin {
        SessionOrigin::User => SessionRowOrigin::User,
        SessionOrigin::Fork => SessionRowOrigin::Fork,
        SessionOrigin::Subagent => SessionRowOrigin::Subagent,
        SessionOrigin::Bot => SessionRowOrigin::Bot,
    }
}

/// The listed sessions on their way into the tree: one tree entry and one
/// wire row each, paired by id.
#[derive(Default)]
struct ListedRows {
    /// What the tree builder orders by: identity, creation time, and the
    /// parent link.
    entries: Vec<jinn_session_list::SessionEntry>,
    /// The rows to send, keyed by the session each describes.
    rows: std::collections::HashMap<SessionId, SessionRowBody>,
}

impl ListedRows {
    /// Adds one listed session.
    fn push(
        &mut self,
        session: &jinn_session_state::ChatSessionState,
        id: &SessionId,
        row: SessionRowBody,
    ) {
        self.entries.push(jinn_session_list::SessionEntry {
            kind: jinn_session_list::SessionEntryKind::Session,
            id: id.clone(),
            title: String::new(),
            is_active: row.is_active,
            created_at: *session.created_at(),
            is_idle: !row.is_busy,
            // The builder reads none of the display flags below; they are
            // the tree vocabulary's shape, not facts this surface uses.
            last_entry_is_error: false,
            parent_id: row.parent_session.clone(),
            depth: 0,
            ancestor_continuations: Vec::new(),
            is_last_child: false,
            is_subagent: row.origin == SessionRowOrigin::Subagent,
            is_bot: row.origin == SessionRowOrigin::Bot,
            is_bot_prepping: false,
            bot_fires_on_parent_completion: false,
            has_live_term: false,
            is_in_flight: false,
        });
        self.rows.insert(id.clone(), row);
    }

    /// The rows in the tree's order, each stamped with its depth.
    ///
    /// The visual-parent map is empty. The terminal sidebar repairs a child
    /// off a removed parent by walking it to a loaded ancestor, and keeps the
    /// overrides in its own slice cell — which this crate cannot read. So
    /// here a row whose parent is not listed is a root, where the terminal
    /// would have nested it under a grandparent. That is the one place this
    /// list and the terminal's disagree, and it is why the map is empty
    /// rather than the builder being used wrong.
    fn in_tree_order(mut self) -> Vec<SessionRowBody> {
        let ordered = jinn_session_list::visible_session_tree(
            self.entries,
            &std::collections::HashMap::new(),
        );
        ordered
            .into_iter()
            // Every entry was pushed beside its row, so the pairing always
            // holds; a lookup that missed would be a row the frame silently
            // dropped, which is the one failure a list must never hide.
            .filter_map(|entry| {
                let depth = u32::try_from(entry.depth).unwrap_or(u32::MAX);
                let row = self.rows.remove(&entry.id)?;
                Some(SessionRowBody { depth, ..row })
            })
            .collect()
    }
}

/// The persisted color mode: `light`, `dark`, or `system`. An unset or
/// unrecognized value reads as `system`, the stylesheet's default.
#[must_use]
pub fn persisted_theme(guard: &StateReadGuard<'_>) -> String {
    match guard.frontend.app_state.web_theme.as_deref() {
        Some(mode @ ("light" | "dark" | "system")) => mode.to_owned(),
        _ => "system".to_owned(),
    }
}

/// Reads the loaded-session rows on their own (for a `Sessions` push), with
/// the hydration flag read in the same instant as the rows.
#[must_use]
pub fn session_rows(
    state: &State,
    settles: &SettleTracker,
) -> (Vec<SessionRowBody>, Vec<SessionRowBody>, bool) {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    rows_from(&guard, &active_id, settles)
}
