//! Session tree entries — the one tree-aware order every session list
//! consumes, and the builder that turns store summaries into entries in
//! that order.
//!
//! Pure functions over [`SessionTreeEntry`]: the terminal picker's loader
//! and the `/web2` session list and Archive picker all build their rows
//! through [`session_tree_entries`], so no surface can order a session
//! tree differently from another.

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_theme::Theme;

use crate::projections::SessionSummary;
use crate::session_tree_entry::{SessionTreeEntry, apply_project_column_width};

/// Builds picker entries from store summaries, in the tree-aware order.
///
/// The whole of a picker load after the read: each summary becomes an
/// entry, the entries are sorted so whole trees move as a unit
/// ([`sort_entries_tree_aware`]), and the project column is padded to the
/// list's widest name.
#[must_use]
pub fn session_tree_entries(
    summaries: Vec<SessionSummary>,
    theme: &Theme,
) -> Vec<SessionTreeEntry> {
    let mut entries: Vec<SessionTreeEntry> = summaries
        .into_iter()
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
        })
        .collect();
    sort_entries_tree_aware(&mut entries);
    apply_project_column_width(&mut entries);
    entries
}

/// Sorts session entries so that whole trees move as a unit.
///
/// Each tree's position is determined by the most recent `updated_at`
/// across all nodes in the tree. Loaded trees appear before Archived trees.
/// Within each tree, children are sorted by `updated_at` descending.
/// The one order both the TUI picker load and the `/web2` session list
/// consume, so the two surfaces cannot disagree.
///
/// # Panics
///
/// Panics only on an internal index/entry desync, which the function
/// builds itself from the same slice — unreachable for well-formed
/// input.
#[expect(clippy::expect_used, reason = "indices from enumerate over entries")]
pub fn sort_entries_tree_aware(entries: &mut Vec<SessionTreeEntry>) {
    if entries.is_empty() {
        return;
    }

    // 1. Index by session_id.
    let id_to_idx: HashMap<SessionId, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.session_id.clone(), i))
        .collect();

    // 2 & 3. Build parent→children map and identify roots.
    let mut children_map: HashMap<SessionId, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        match &entry.parent_id {
            Some(pid) if id_to_idx.contains_key(pid) => {
                children_map.entry(pid.clone()).or_default().push(idx);
            }
            _ => {
                roots.push(idx);
            }
        }
    }

    // 4. Compute tree-max updated_at for each root.
    let tree_max: HashMap<SessionId, jiff::Timestamp> = roots
        .iter()
        .map(|&root_idx| {
            let max_ts = compute_subtree_max(root_idx, &children_map, entries);
            let root = entries.get(root_idx).expect("index from enumerate");
            (root.session_id.clone(), max_ts)
        })
        .collect();

    // 5 & 6. Sort roots: session_state ascending (Loaded < Archived → Loaded first), then tree-max updated_at descending.
    roots.sort_by(|&a, &b| {
        let entry_a = entries.get(a).expect("index from enumerate");
        let entry_b = entries.get(b).expect("index from enumerate");
        entry_a
            .session_state
            .cmp(&entry_b.session_state)
            .then_with(|| {
                let max_b = tree_max.get(&entry_b.session_id).expect("key from entries");
                let max_a = tree_max.get(&entry_a.session_id).expect("key from entries");
                max_b.cmp(max_a)
            })
    });

    // 7. Flatten in DFS order, sorting children by updated_at descending.
    let mut sorted = Vec::with_capacity(entries.len());
    for &root_idx in &roots {
        emit_tree(root_idx, &children_map, entries, &mut sorted);
    }
    *entries = sorted;
}

/// Returns the maximum `updated_at` across a node and all its descendants.
#[expect(clippy::expect_used, reason = "infallible")]
fn compute_subtree_max(
    idx: usize,
    children_map: &HashMap<SessionId, Vec<usize>>,
    entries: &[SessionTreeEntry],
) -> jiff::Timestamp {
    let entry = entries.get(idx).expect("index from enumerate");
    let mut max_ts = entry.updated_at;
    if let Some(children) = children_map.get(&entry.session_id) {
        for &child_idx in children {
            let child_max = compute_subtree_max(child_idx, children_map, entries);
            max_ts = max_ts.max(child_max);
        }
    }
    max_ts
}

/// Emits a node and all its descendants in DFS order.
/// Children within each parent are sorted by `updated_at` descending.
#[expect(clippy::expect_used, reason = "infallible")]
fn emit_tree(
    idx: usize,
    children_map: &HashMap<SessionId, Vec<usize>>,
    entries: &[SessionTreeEntry],
    result: &mut Vec<SessionTreeEntry>,
) {
    let entry = entries.get(idx).expect("index from enumerate");
    result.push(entry.clone());
    if let Some(mut children) = children_map.get(&entry.session_id).cloned() {
        children.sort_by(|&a, &b| {
            let ca = entries.get(a).expect("index from enumerate");
            let cb = entries.get(b).expect("index from enumerate");
            cb.updated_at.cmp(&ca.updated_at)
        });
        for &child_idx in &children {
            emit_tree(child_idx, children_map, entries, result);
        }
    }
}
