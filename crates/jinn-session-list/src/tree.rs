//! Deterministic visible-session-tree construction.

use std::collections::{HashMap, HashSet};

use jinn_core_types::SessionId;

use crate::SessionEntry;

/// Resolved parent-child tree for visible session rows.
struct SessionTree {
    entries: HashMap<SessionId, SessionEntry>,
    roots: Vec<SessionId>,
    children: HashMap<SessionId, Vec<SessionId>>,
}

/// Resolves direct and visual parents, then orders roots newest-first and
/// children oldest-first before flattening them into visible DFS order.
///
/// Missing direct parents are treated as roots unless a visual-parent override
/// points to another loaded row. Every row is emitted at most once even when
/// malformed input contains a cycle.
#[must_use]
pub fn visible_session_tree(
    entries: Vec<SessionEntry>,
    visual_parents: &HashMap<SessionId, SessionId>,
) -> Vec<SessionEntry> {
    let tree = build_tree(entries, visual_parents);
    flatten_tree(&tree)
}

fn build_tree(
    entries: Vec<SessionEntry>,
    visual_parents: &HashMap<SessionId, SessionId>,
) -> SessionTree {
    let mut entries = entries
        .into_iter()
        .map(|entry| (entry.id.clone(), entry))
        .collect::<HashMap<_, _>>();
    let mut roots = Vec::new();
    let mut children: HashMap<SessionId, Vec<SessionId>> = HashMap::new();
    let mut effective_parents = HashMap::new();

    for (id, entry) in &entries {
        match effective_parent(id, entry, &entries, visual_parents) {
            Some(parent_id) => {
                children
                    .entry(parent_id.clone())
                    .or_default()
                    .push(id.clone());
                effective_parents.insert(id.clone(), parent_id);
            }
            None => roots.push(id.clone()),
        }
    }

    for (id, parent_id) in effective_parents {
        if let Some(entry) = entries.get_mut(&id) {
            entry.parent_id = Some(parent_id);
        }
    }
    sort_roots(&mut roots, &entries);
    sort_children(&mut children, &entries);

    SessionTree {
        entries,
        roots,
        children,
    }
}

fn effective_parent(
    id: &SessionId,
    entry: &SessionEntry,
    entries: &HashMap<SessionId, SessionEntry>,
    visual_parents: &HashMap<SessionId, SessionId>,
) -> Option<SessionId> {
    match &entry.parent_id {
        Some(parent_id) if entries.contains_key(parent_id) => Some(parent_id.clone()),
        Some(parent_id) => visual_parents
            .get(parent_id)
            .or_else(|| visual_parents.get(id))
            .filter(|candidate| entries.contains_key(*candidate))
            .cloned(),
        None => None,
    }
}

fn sort_roots(roots: &mut [SessionId], entries: &HashMap<SessionId, SessionEntry>) {
    roots.sort_by_key(|id| std::cmp::Reverse(created_at(entries, id)));
}

fn sort_children(
    children: &mut HashMap<SessionId, Vec<SessionId>>,
    entries: &HashMap<SessionId, SessionEntry>,
) {
    for ids in children.values_mut() {
        ids.sort_by_key(|id| created_at(entries, id));
    }
}

fn created_at(entries: &HashMap<SessionId, SessionEntry>, id: &SessionId) -> jiff::Timestamp {
    entries
        .get(id)
        .map(|entry| entry.created_at)
        .unwrap_or_default()
}

fn flatten_tree(tree: &SessionTree) -> Vec<SessionEntry> {
    let mut flattened = Vec::new();
    let mut visited = HashSet::new();
    for (index, root_id) in tree.roots.iter().enumerate() {
        push_root(
            tree,
            root_id,
            index + 1 == tree.roots.len(),
            &mut flattened,
            &mut visited,
        );
    }
    flattened
}

fn push_root(
    tree: &SessionTree,
    root_id: &SessionId,
    is_last: bool,
    flattened: &mut Vec<SessionEntry>,
    visited: &mut HashSet<SessionId>,
) {
    if !visited.insert(root_id.clone()) {
        return;
    }
    let Some(mut root) = tree.entries.get(root_id).cloned() else {
        return;
    };
    root.depth = 0;
    root.ancestor_continuations.clear();
    root.is_last_child = is_last;
    let root_id = root.id.clone();
    flattened.push(root);
    push_children(tree, &root_id, vec![], is_last, flattened, visited);
}

fn push_children(
    tree: &SessionTree,
    parent_id: &SessionId,
    ancestor_continuations: Vec<bool>,
    parent_is_last: bool,
    flattened: &mut Vec<SessionEntry>,
    visited: &mut HashSet<SessionId>,
) {
    let child_ids = tree.children.get(parent_id).cloned().unwrap_or_default();
    if child_ids.is_empty() {
        return;
    }
    let mut continuations = ancestor_continuations;
    continuations.push(!parent_is_last);
    let child_count = child_ids.len();
    for (index, child_id) in child_ids.into_iter().enumerate() {
        if visited.insert(child_id.clone())
            && let Some(mut child) = tree.entries.get(&child_id).cloned()
        {
            let is_last = index + 1 == child_count;
            child.depth = continuations.len();
            child.ancestor_continuations.clone_from(&continuations);
            child.is_last_child = is_last;
            flattened.push(child);
            push_children(
                tree,
                &child_id,
                continuations.clone(),
                is_last,
                flattened,
                visited,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionEntryKind;

    fn entry(
        id: SessionId,
        created_at: jiff::Timestamp,
        parent_id: Option<SessionId>,
    ) -> SessionEntry {
        let title = id.to_string();
        SessionEntry {
            kind: SessionEntryKind::Session,
            id,
            title,
            is_active: false,
            created_at,
            is_idle: true,
            last_entry_is_error: false,
            parent_id,
            depth: 0,
            ancestor_continuations: vec![],
            is_last_child: false,
            is_subagent: false,
            is_attendant: false,
            is_attendant_prepping: false,
            attendant_fires_on_parent_completion: false,
            has_live_term: false,
            is_in_flight: false,
        }
    }

    #[rstest::rstest]
    fn roots_are_newest_first_and_children_oldest_first() {
        // Given two roots and two children under the older root.
        let old_root = SessionId::new();
        let new_root = SessionId::new();
        let older_child = SessionId::new();
        let newer_child = SessionId::new();
        let entries = vec![
            entry(old_root.clone(), jiff::Timestamp::UNIX_EPOCH, None),
            entry(
                new_root.clone(),
                jiff::Timestamp::UNIX_EPOCH + jiff::Span::new().seconds(2),
                None,
            ),
            entry(
                older_child.clone(),
                jiff::Timestamp::UNIX_EPOCH,
                Some(old_root.clone()),
            ),
            entry(
                newer_child.clone(),
                jiff::Timestamp::UNIX_EPOCH + jiff::Span::new().seconds(1),
                Some(old_root.clone()),
            ),
        ];

        // When building the visible tree.
        let visible = visible_session_tree(entries, &HashMap::new());

        // Then roots are newest-first and children are oldest-first.
        assert_eq!(
            visible.iter().map(|entry| &entry.id).collect::<Vec<_>>(),
            vec![&new_root, &old_root, &older_child, &newer_child]
        );
    }

    #[rstest::rstest]
    fn visual_parent_replaces_a_missing_direct_parent() {
        // Given a hidden direct parent and its visible replacement.
        let hidden = SessionId::new();
        let visible_parent = SessionId::new();
        let child = SessionId::new();
        let entries = vec![
            entry(visible_parent.clone(), jiff::Timestamp::UNIX_EPOCH, None),
            entry(
                child.clone(),
                jiff::Timestamp::UNIX_EPOCH,
                Some(hidden.clone()),
            ),
        ];
        let visual_parents = HashMap::from([(hidden, visible_parent.clone())]);

        // When building the visible tree.
        let visible = visible_session_tree(entries, &visual_parents);

        // Then the child is nested under the replacement parent.
        assert!(matches!(
            visible.as_slice(),
            [_, child_entry]
                if child_entry.parent_id == Some(visible_parent) && child_entry.depth == 1
        ));
    }

    #[rstest::rstest]
    fn cyclic_parents_emit_no_rows_without_roots() {
        // Given a malformed two-node parent cycle.
        let left = SessionId::new();
        let right = SessionId::new();
        let entries = vec![
            entry(
                left.clone(),
                jiff::Timestamp::UNIX_EPOCH,
                Some(right.clone()),
            ),
            entry(
                right.clone(),
                jiff::Timestamp::UNIX_EPOCH,
                Some(left.clone()),
            ),
        ];

        // When building the visible tree.
        let visible = visible_session_tree(entries, &HashMap::new());

        // Then no row is emitted because the malformed graph has no root.
        assert_eq!(visible, [] as [SessionEntry; 0]);
    }
}
