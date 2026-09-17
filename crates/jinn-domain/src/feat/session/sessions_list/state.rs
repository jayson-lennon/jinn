//! State types for the sessions sidebar section.

use std::collections::{HashMap, HashSet};

use crate::common::app_state::AppState;
use crate::feat::session::phase_machine::PhaseKind;
use crate::protocol::SessionId;

pub use jinn_sidebar_msg::{SessionEntry, SessionEntryKind, SessionsSectionState};

/// Collects all loaded sessions in tree order (DFS).
///
/// Roots are sorted by `created_at` descending (newest first).
/// Children under each parent are sorted by `created_at` ascending
/// (oldest child first - they were forked first). Orphaned sessions
/// (parent not in loaded sessions) are treated as roots.
///
/// Only includes sessions with `SessionState::Loaded` - archived sessions
/// Resolved parent-child tree for sidebar session entries.
struct SessionTree {
    /// Entries indexed by session ID.
    entry_map: HashMap<SessionId, SessionEntry>,
    /// Root session IDs (no effective parent), sorted newest-first.
    roots: Vec<SessionId>,
    /// Parent → child ID mapping, children sorted oldest-first.
    children_map: HashMap<SessionId, Vec<SessionId>>,
}

/// Builds the session tree from loaded sessions.
///
/// Resolves effective parents (direct or visual), patches `parent_id` fields,
/// sorts roots descending by `created_at` and children ascending.
fn build_session_tree(
    entries: Vec<SessionEntry>,
    visual_parents: &HashMap<SessionId, SessionId>,
) -> SessionTree {
    let mut entry_map: HashMap<SessionId, SessionEntry> =
        entries.into_iter().map(|e| (e.id.clone(), e)).collect();

    let mut children_map: HashMap<SessionId, Vec<SessionId>> = HashMap::new();
    let mut roots: Vec<SessionId> = Vec::new();
    let mut effective_parents: HashMap<SessionId, SessionId> = HashMap::new();

    for entry in entry_map.values() {
        let effective_parent = match &entry.parent_id {
            Some(pid) if entry_map.contains_key(pid) => Some(pid.clone()),
            Some(pid) => visual_parents
                .get(pid)
                .or_else(|| visual_parents.get(&entry.id))
                .filter(|vp| entry_map.contains_key(*vp))
                .cloned(),
            None => None,
        };

        match effective_parent {
            Some(ref pid) => {
                children_map
                    .entry(pid.clone())
                    .or_default()
                    .push(entry.id.clone());
                effective_parents.insert(entry.id.clone(), pid.clone());
            }
            None => {
                roots.push(entry.id.clone());
            }
        }
    }

    // Patch entry.parent_id to reflect effective visual parent.
    for (id, ep) in effective_parents {
        if let Some(entry) = entry_map.get_mut(&id) {
            entry.parent_id = Some(ep);
        }
    }

    // Sort roots descending by created_at (newest first).
    roots.sort_by(|a, b| {
        let ea = entry_map.get(a).map(|e| e.created_at).unwrap_or_default();
        let eb = entry_map.get(b).map(|e| e.created_at).unwrap_or_default();
        eb.cmp(&ea)
    });

    // Sort each parent's children ascending by created_at (oldest first).
    for children in children_map.values_mut() {
        children.sort_by(|a, b| {
            let ea = entry_map.get(a).map(|e| e.created_at).unwrap_or_default();
            let eb = entry_map.get(b).map(|e| e.created_at).unwrap_or_default();
            ea.cmp(&eb)
        });
    }

    SessionTree {
        entry_map,
        roots,
        children_map,
    }
}
/// Performs a depth-first traversal of the session tree to produce a flat list
/// with tree metadata (depth, ancestor continuations, last-child flags).
fn dfs_flatten(tree: &SessionTree) -> Vec<SessionEntry> {
    let mut result: Vec<SessionEntry> = Vec::new();
    let mut visited: HashSet<SessionId> = HashSet::new();
    let root_count = tree.roots.len();

    for (i, root_id) in tree.roots.iter().enumerate() {
        if !visited.insert(root_id.clone()) {
            continue;
        }
        let Some(mut entry) = tree.entry_map.get(root_id).cloned() else {
            continue;
        };
        entry.depth = 0;
        entry.ancestor_continuations = vec![];
        entry.is_last_child = i == root_count - 1;
        result.push(entry);
        dfs_children(
            root_id,
            &tree.children_map,
            &tree.entry_map,
            &mut result,
            vec![],
            &mut visited,
            i == root_count - 1,
        );
    }

    result
}

/// are not in the `SessionMap` and thus excluded automatically.
///
/// # Panics
///
/// Panics if a session exists in the map but its parent does not.
pub fn sorted_open_sessions(state: &AppState) -> Vec<SessionEntry> {
    sorted_open_sessions_split(&state.session, &state.frontend)
}

/// Split-borrow variant of [`sorted_open_sessions`] for use inside tcaps views.
pub fn sorted_open_sessions_split(
    session: &crate::common::session_map::SessionMap,
    frontend: &crate::feat::ui::frontend_state::FrontendState,
) -> Vec<SessionEntry> {
    let active_id = session.active_session_id();

    // Collect all loaded sessions into entries.
    let entries: Vec<SessionEntry> = session
        .iter()
        .filter(|(_, session)| {
            session.session_state() == crate::feat::session::chat_session::SessionState::Loaded
        })
        .map(|(id, session)| SessionEntry {
            kind: SessionEntryKind::Session,
            id: id.clone(),
            title: session.title().unwrap_or("Untitled Session").to_owned(),
            is_active: id == active_id,
            created_at: *session.created_at(),
            is_idle: matches!(session.phase(), PhaseKind::Idle) && !session.is_busy(),
            last_entry_is_error: session
                .history()
                .last()
                .is_some_and(|e| matches!(&e.kind, crate::protocol::ChatEntryKind::Error(..))),
            parent_id: session.parent_session().clone(),
            depth: 0,
            ancestor_continuations: vec![],
            is_last_child: false,
            is_subagent: session.origin()
                == crate::feat::session::chat_session::SessionOrigin::Subagent,
            has_live_term: frontend
                .slices()
                .and_then(|s| {
                    s.reader::<jinn_term_msg::TerminalTabState>(&jinn_term_msg::term_tabs_slot())
                })
                .is_some_and(|cell| cell.read().live_terms.contains(id)),
        })
        .collect();

    let visual_parents = frontend.with_sections(
        |s| s.sessions.visual_parents.clone(),
        std::collections::HashMap::new,
    );
    let tree = build_session_tree(entries, &visual_parents);

    dfs_flatten(&tree)
}

/// Updates the visual-parent index when a session is about to be removed.
///
/// Must be called BEFORE the session is removed from the `SessionMap`.
/// Resolves the nearest loaded ancestor for any orphaned children and
/// updates the `visual_parents` index accordingly.
///
/// Also handles transitive chains: if any existing `visual_parents` entries
/// point to the removed session as their effective ancestor, those entries
/// are updated to point to the resolved ancestor instead.
pub fn update_visual_parents_on_removal(state: &mut AppState, removed_id: &SessionId) {
    update_visual_parents_on_removal_split(&mut state.session, &mut state.frontend, removed_id);
}

/// Split-borrow variant of [`update_visual_parents_on_removal`] for use inside tcaps views.
pub fn update_visual_parents_on_removal_split(
    session: &mut crate::common::session_map::SessionMap,
    frontend: &mut crate::feat::ui::frontend_state::FrontendState,
    removed_id: &SessionId,
) {
    let effective_ancestor = {
        // Resolve the nearest loaded ancestor for the session being removed.
        let Some(removed_session) = session.get(removed_id) else {
            return;
        };

        match removed_session.parent_session() {
            // Direct parent is loaded - use it.
            Some(pid) if session.contains(pid) => Some(pid.clone()),
            // Direct parent not loaded - check if it has a visual_parents entry.
            Some(pid) => frontend.with_sections(
                |s| {
                    s.sessions.visual_parents.get(pid).cloned().or_else(|| {
                        // The parent's parent may not be in visual_parents,
                        // but the removed session itself might have been reparented.
                        s.sessions.visual_parents.get(removed_id).cloned()
                    })
                },
                || None,
            ),
            // No parent at all - check if the removed session itself has a visual parent.
            None => frontend.with_sections(
                |s| s.sessions.visual_parents.get(removed_id).cloned(),
                || None,
            ),
        }
    };

    frontend.update_sections(|s| {
        let visual_parents = &mut s.sessions.visual_parents;

        // Find direct children of the removed session and reparent them.
        let orphan_ids: Vec<SessionId> = session
            .iter()
            .filter(|(_, s)| s.parent_session().as_ref() == Some(removed_id))
            .map(|(id, _)| id.clone())
            .collect();

        for orphan_id in orphan_ids {
            match &effective_ancestor {
                Some(ancestor_id) => {
                    visual_parents.insert(orphan_id, ancestor_id.clone());
                }
                None => {
                    visual_parents.remove(&orphan_id);
                }
            }
        }

        // Update transitive entries: any session already bypassing the removed session.
        let keys_to_update: Vec<SessionId> = visual_parents
            .iter()
            .filter(|(_, v)| *v == removed_id)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys_to_update {
            match &effective_ancestor {
                Some(ancestor_id) => {
                    visual_parents.insert(key, ancestor_id.clone());
                }
                None => {
                    visual_parents.remove(&key);
                }
            }
        }
    });
}

/// Removes stale `visual_parents` entries when a session is loaded back.
///
/// When a session is unarchived/loaded, its children no longer need to bypass
/// it in the tree. This function removes any `visual_parents` entries whose
/// **value** equals the loaded session's ID.
///
/// Entries where the loaded session is the **key** are preserved - the loaded
/// session may itself have a hidden parent that it needs to be reparented under.
pub fn clear_visual_parents_on_load(state: &mut AppState, loaded_id: &SessionId) {
    clear_visual_parents_on_load_split(&mut state.frontend, loaded_id);
}

/// Split-borrow variant of [`clear_visual_parents_on_load`] for use inside tcaps views.
pub fn clear_visual_parents_on_load_split(
    frontend: &mut crate::feat::ui::frontend_state::FrontendState,
    loaded_id: &SessionId,
) {
    frontend.update_sections(|s| {
        s.sessions.visual_parents.retain(|_k, v| v != loaded_id);
    });
}

/// Recursively appends children of `parent_id` to `result` in DFS order.
///
/// `ancestor_continuations` tracks whether each ancestor level has younger
/// siblings - used to draw `│` continuation lines.
fn dfs_children(
    parent_id: &SessionId,
    children_map: &HashMap<SessionId, Vec<SessionId>>,
    entry_map: &HashMap<SessionId, SessionEntry>,
    result: &mut Vec<SessionEntry>,
    ancestor_continuations: Vec<bool>,
    visited: &mut HashSet<SessionId>,
    parent_is_last: bool,
) {
    let children = children_map.get(parent_id).cloned().unwrap_or_default();

    if children.is_empty() {
        return;
    }

    // Determine continuation lines.
    let mut continuations = ancestor_continuations;
    continuations.push(!parent_is_last);

    for (i, child_id) in children.iter().enumerate() {
        if !visited.insert(child_id.clone()) {
            continue; // cycle guard
        }
        let is_last = i == children.len() - 1;
        let Some(mut entry) = entry_map.get(child_id).cloned() else {
            continue;
        };
        entry.depth = continuations.len();
        entry.ancestor_continuations.clone_from(&continuations);
        entry.is_last_child = is_last;
        result.push(entry);
        dfs_children(
            child_id,
            children_map,
            entry_map,
            result,
            continuations.clone(),
            visited,
            is_last,
        );
    }
}
