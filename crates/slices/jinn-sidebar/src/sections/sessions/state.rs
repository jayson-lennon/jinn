//! Sidebar-owned sessions-list state adapter.

use std::collections::{HashMap, HashSet};

use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
pub use jinn_session_list::{SessionEntry, SessionEntryKind};
pub use jinn_sidebar_msg::SessionsSectionState;

use jinn_session_msg::{PhaseKind, SessionOrigin};

/// A cheap, clone-free summary of everything the sessions tree depends on.
///
/// The tree build clones every session's title and then clones every entry
/// again, so doing it per frame is expensive with many sessions. Comparing two
/// of these instead costs one pass of O(1) reads per session — no `String`
/// clones, no allocation — and only a mismatch pays for a rebuild.
///
/// The title is summarised by length plus its first and last few bytes, which
/// catches a rename (and any length change) without reading the whole string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListKey {
    id: SessionId,
    title_len: usize,
    title_head: u64,
    title_tail: u64,
    is_active: bool,
    created_at: jiff::Timestamp,
    is_idle: bool,
    last_entry_is_error: bool,
    parent_id: Option<SessionId>,
    is_subagent: bool,
    is_attendant: bool,
    is_attendant_prepping: bool,
    attendant_fires_on_parent_completion: bool,
    has_live_term: bool,
    is_in_flight: bool,
}

impl SessionListKey {
    /// Summarize a title by length and its boundary bytes.
    fn title_digest(title: &str) -> (usize, u64, u64) {
        const EDGE: usize = 8;
        let bytes = title.as_bytes();
        let head = &bytes[..bytes.len().min(EDGE)];
        let tail = &bytes[bytes.len().saturating_sub(EDGE)..];
        (bytes.len(), fold_bytes(head), fold_bytes(tail))
    }

    /// Read the per-session scalars the tree depends on.
    fn of_session(
        id: &SessionId,
        session: &jinn_session_state::ChatSessionState,
        active_id: &SessionId,
        has_live_term: bool,
        is_in_flight: bool,
    ) -> Self {
        let title = session.title().unwrap_or("Untitled Session");
        let (title_len, title_head, title_tail) = Self::title_digest(title);
        Self {
            id: id.clone(),
            title_len,
            title_head,
            title_tail,
            is_active: id == active_id,
            created_at: *session.created_at(),
            is_idle: matches!(session.phase(), PhaseKind::Idle),
            last_entry_is_error: session.history().last().is_some_and(|entry| {
                matches!(&entry.kind, jinn_core_types::ChatEntryKind::Error(..))
            }),
            parent_id: session.parent_session().clone(),
            is_subagent: session.origin() == SessionOrigin::Subagent,
            is_attendant: session.is_attendant(),
            // Same expression the tree build uses, so the key and the tree
            // cannot disagree about whether the paused marker should render.
            is_attendant_prepping: session.is_attendant() && session.attendant_is_prepping(),
            attendant_fires_on_parent_completion: session.is_attendant()
                && session.attendant_fires_on_parent_completion(),
            has_live_term,
            is_in_flight,
        }
    }
}

/// Fold a few bytes into a `u64` for cheap comparison.
fn fold_bytes(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |acc, b| acc.rotate_left(8) ^ u64::from(*b))
}

/// Collects loaded sessions in visible depth-first order.
pub fn sorted_open_sessions(state: &AppState) -> Vec<SessionEntry> {
    sorted_open_sessions_split(&state.session, &state.frontend)
}

/// A cheap summary of the current session list, for memoization.
///
/// Reads only O(1) scalars per session: no title clones, no tree build.
pub fn session_list_key(state: &AppState) -> Vec<SessionListKey> {
    let active_id = state.session.active_session_id();
    state
        .session
        .iter()
        .filter(|(_, session)| {
            session.session_state() == jinn_session_store_msg::SessionState::Loaded
        })
        .map(|(id, session)| {
            let has_live_term = state
                .frontend
                .slices()
                .and_then(|slices| {
                    slices
                        .reader::<jinn_term_msg::TerminalTabState>(&jinn_term_msg::term_tabs_slot())
                })
                .is_some_and(|cell| cell.read().live_terms.contains(id));
            let in_flight = is_in_flight(&state.frontend, id);
            SessionListKey::of_session(id, session, active_id, has_live_term, in_flight)
        })
        .collect()
}

/// Split-borrow variant used by sidebar actors and other slice-owned adapters.
pub fn sorted_open_sessions_split(
    session: &jinn_session_state::SessionMap,
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
) -> Vec<SessionEntry> {
    let active_id = session.active_session_id();
    let entries = session
        .iter()
        .filter(|(_, session)| {
            session.session_state() == jinn_session_store_msg::SessionState::Loaded
        })
        .map(|(id, session)| SessionEntry {
            kind: SessionEntryKind::Session,
            id: id.clone(),
            title: session.title().unwrap_or("Untitled Session").to_owned(),
            is_active: id == active_id,
            created_at: *session.created_at(),
            is_idle: matches!(session.phase(), PhaseKind::Idle),
            last_entry_is_error: session.history().last().is_some_and(|entry| {
                matches!(&entry.kind, jinn_core_types::ChatEntryKind::Error(..))
            }),
            parent_id: session.parent_session().clone(),
            depth: 0,
            ancestor_continuations: vec![],
            is_last_child: false,
            is_subagent: session.origin() == SessionOrigin::Subagent,
            is_attendant: session.is_attendant(),
            is_attendant_prepping: session.is_attendant() && session.attendant_is_prepping(),
            attendant_fires_on_parent_completion: session.is_attendant()
                && session.attendant_fires_on_parent_completion(),
            has_live_term: frontend
                .slices()
                .and_then(|slices| {
                    slices
                        .reader::<jinn_term_msg::TerminalTabState>(&jinn_term_msg::term_tabs_slot())
                })
                .is_some_and(|cell| cell.read().live_terms.contains(id)),
            is_in_flight: is_in_flight(frontend, id),
        })
        .collect();
    let visual_parents = frontend.with_sections(
        |sections| sections.sessions.visual_parents.clone(),
        std::collections::HashMap::new,
    );
    jinn_session_list::visible_session_tree(entries, &visual_parents)
}

/// Marks sessions as having a disposal operation dispatched and unfinished.
///
/// Called at the moment the disposal command is dispatched, never at keypress
/// time, so a rejected validation leaves no in-flight indication behind.
/// The visible row at which the sessions section's cursor is drawn.
///
/// The cursor names a session; drawing, scrolling and anchoring all need the
/// row it lands on. Resolving that is the one direction the id-cursor does
/// need, and it goes through the session-list crate's own inverse so the two
/// directions cannot drift: `None` when the session is no longer listed, which
/// is what a session archived while the sidebar was unfocused looks like.
#[must_use]
pub fn visible_row_of(state: &AppState, id: &SessionId) -> Option<usize> {
    jinn_session_list::visible_index_of(session_tree_nodes(state), &visual_parents(state), id)
}

/// Stores the session drawn at `row` as the cursor.
pub fn select_row(state: &mut AppState, row: usize, sessions: &[SessionEntry]) {
    if let Some(entry) = sessions.get(row) {
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(entry.id.clone()));
    }
}

/// The section's visual-parent map: a session whose direct parent is unloaded
/// is drawn under its nearest loaded ancestor.
fn visual_parents(state: &AppState) -> HashMap<SessionId, SessionId> {
    state
        .frontend
        .with_sections(|s| s.sessions.visual_parents.clone(), HashMap::new)
}

/// The loaded sessions reduced to identity and tree position.
fn session_tree_nodes(state: &AppState) -> Vec<jinn_session_list::SessionTreeNode> {
    state
        .session
        .iter()
        .filter(|(_, session)| {
            session.session_state() == jinn_session_store_msg::SessionState::Loaded
        })
        .map(|(id, session)| jinn_session_list::SessionTreeNode {
            id: id.clone(),
            created_at: *session.created_at(),
            parent_id: session.parent_session().clone(),
        })
        .collect()
}

pub fn mark_in_flight(state: &AppState, ids: &[SessionId]) {
    state
        .frontend
        .update_sections(|sections| sections.sessions.begin_in_flight(ids));
}

/// Clears the in-flight mark for a session whose disposal has concluded.
pub fn clear_in_flight(
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
    id: &SessionId,
) {
    frontend.update_sections(|sections| sections.sessions.end_in_flight(id));
}

/// Whether a session currently has a disposal operation in flight.
pub fn is_in_flight(
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
    id: &SessionId,
) -> bool {
    frontend.with_sections(|sections| sections.sessions.is_in_flight(id), || false)
}

/// Repairs visual parents before a sidebar-owned removal operation.
pub fn update_visual_parents_on_removal(
    state: &mut AppState,
    removed_id: &jinn_core_types::SessionId,
) {
    update_visual_parents_on_removal_split(&mut state.session, &mut state.frontend, removed_id);
}

/// Split-borrow variant of [`update_visual_parents_on_removal`].
pub fn update_visual_parents_on_removal_split(
    session: &mut jinn_session_state::SessionMap,
    frontend: &mut jinn_kernel::state::frontend_state::FrontendState,
    removed_id: &jinn_core_types::SessionId,
) {
    let (removed_parent, loaded_ids, direct_child_ids) = {
        let Some(removed_session) = session.get(removed_id) else {
            return;
        };
        (
            removed_session.parent_session().clone(),
            session
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>(),
            session
                .iter()
                .filter(|(_, child)| child.parent_session().as_ref() == Some(removed_id))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
        )
    };
    frontend.update_sections(|sections| {
        jinn_session_list::repair_visual_parents_on_removal(
            &mut sections.sessions.visual_parents,
            removed_id,
            removed_parent.as_ref(),
            &loaded_ids,
            direct_child_ids,
        );
    });
}

/// Repairs visual parents after a removed row is no longer in the session map.
pub fn repair_visual_parents_after_removal(
    session: &jinn_session_state::SessionMap,
    frontend: &mut jinn_kernel::state::frontend_state::FrontendState,
    removed_id: &jinn_core_types::SessionId,
    removed_parent: Option<&jinn_core_types::SessionId>,
) {
    let (loaded_ids, direct_child_ids) = {
        (
            session
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>(),
            session
                .iter()
                .filter(|(_, child)| child.parent_session().as_ref() == Some(removed_id))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
        )
    };
    frontend.update_sections(|sections| {
        jinn_session_list::repair_visual_parents_on_removal(
            &mut sections.sessions.visual_parents,
            removed_id,
            removed_parent,
            &loaded_ids,
            direct_child_ids,
        );
    });
}

/// Clears visual-parent bypasses after a session becomes visible again.
pub fn clear_visual_parents_on_load(state: &mut AppState, loaded_id: &jinn_core_types::SessionId) {
    clear_visual_parents_on_load_split(&mut state.frontend, loaded_id);
}

/// Split-borrow variant of [`clear_visual_parents_on_load`].
pub fn clear_visual_parents_on_load_split(
    frontend: &mut jinn_kernel::state::frontend_state::FrontendState,
    loaded_id: &jinn_core_types::SessionId,
) {
    frontend.update_sections(|sections| {
        jinn_session_list::clear_visual_parents_on_load(
            &mut sections.sessions.visual_parents,
            loaded_id,
        );
    });
}
