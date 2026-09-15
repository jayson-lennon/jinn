//! Tree-wide aggregate statistics for session trees.
//!
//! When sessions form a tree (parent/child via `parent_session`), this module
//! provides functions to find the tree root and compute aggregate stats across
//! the entire tree - not just the active session's descendants.
//!
//! Archived sessions are represented as lightweight [`FrozenTreeNode`] snapshots
//! that preserve tree structure and stats without holding full session state.

use std::collections::{HashMap, HashSet};

use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::compute_turn_count;
use crate::feat::session::token_stats::TokenStats;
use crate::protocol::SessionId;

/// Aggregate statistics for an entire session tree.
///
/// Sums tokens, cost, and turns across ALL sessions in the tree (root + all
/// descendants), regardless of which session is currently active.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeAggregateStats {
    /// Total tokens sent across all sessions in the tree.
    pub total_sent: u64,
    /// Total tokens received across all sessions in the tree.
    pub total_received: u64,
    /// Total cost across all sessions in the tree.
    pub total_cost: f64,
    /// Total turns across all sessions in the tree.
    pub total_turns: u32,
    /// Number of sessions in the tree.
    pub session_count: usize,
    /// Effective sent total (provider-reported prompt_tokens else estimate).
    pub effective_sent: u64,
    /// Sum of provider-reported prompt_tokens over measured turns.
    pub measured_sent: u64,
    /// Sum of provider-reported cache-hit counts.
    pub cached_total: u64,
}

/// A lightweight snapshot of an archived session's stats.
///
/// Created at archive time, before the session is removed from the in-memory
/// `SessionMap`. Used by [`aggregate_tree_stats`] to include archived sessions
/// in the tree summary without requiring disk I/O.
#[derive(Debug, Clone)]
pub struct FrozenTreeNode {
    /// The archived session's ID.
    pub session_id: SessionId,
    /// Parent session ID - `None` for root sessions.
    pub parent_session_id: Option<SessionId>,
    /// Total tokens sent across all requests in this session.
    pub total_sent: u64,
    /// Total tokens received across all responses in this session.
    pub total_received: u64,
    /// Total cost in USD across all requests in this session.
    pub total_cost: f64,
    /// Total turns (user messages) in this session.
    pub total_turns: u32,
    /// Effective sent total (provider-reported prompt_tokens else estimate).
    pub effective_sent: u64,
    /// Sum of provider-reported prompt_tokens over measured turns.
    pub measured_sent: u64,
    /// Sum of provider-reported cache-hit counts.
    pub cached_total: u64,
}

/// Create a `FrozenTreeNode` snapshot from a live session.
///
/// Computes token stats, cost, and turn count from the session's current state.
/// Used by the archive flow to preserve stats before the session is removed
/// from memory.
pub fn snapshot_frozen_node(session: &ChatSessionState) -> FrozenTreeNode {
    let token_stats = TokenStats::from_ledger(session.token_ledger());
    FrozenTreeNode {
        session_id: session.session_id().clone(),
        parent_session_id: session.parent_session().clone(),
        total_sent: token_stats.total_sent,
        total_received: token_stats.total_received,
        total_cost: TokenStats::total_cost(session.token_ledger()),
        total_turns: compute_turn_count(session.history(), session.fork_ordinal()),
        effective_sent: token_stats.effective_sent,
        measured_sent: token_stats.measured_sent,
        cached_total: token_stats.cached_total,
    }
}

/// Find the root of the session tree containing `session_id`.
///
/// Walks up via `parent_session` links until reaching a session with no parent
/// (or whose parent is not in any map). Checks both live sessions and frozen
/// nodes to resolve parent links across archived sessions. Guards against
/// cycles with a visited set.
///
/// Returns `session_id` itself if it has no parent (single session or root).
pub fn find_tree_root<S: ::std::hash::BuildHasher>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    frozen_nodes: &HashMap<SessionId, FrozenTreeNode, S>,
    session_id: &SessionId,
) -> SessionId {
    let mut visited = HashSet::new();
    let mut current = session_id.clone();

    loop {
        // Cycle protection: if we've visited this node, it's the root.
        if !visited.insert(current.clone()) {
            return current;
        }

        // Look up parent from live session or frozen node.
        let parent_id = if let Some(session) = sessions.get(&current) {
            session.parent_session().clone()
        } else if let Some(frozen) = frozen_nodes.get(&current) {
            frozen.parent_session_id.clone()
        } else {
            // Not found anywhere - treat as root.
            return current;
        };

        let Some(parent_id) = parent_id else {
            // No parent - this is the root.
            return current;
        };

        // Check that the parent exists in either live sessions or frozen nodes.
        if !sessions.contains_key(&parent_id) && !frozen_nodes.contains_key(&parent_id) {
            // Orphan - parent was removed. This node is the root.
            return current;
        }

        current = parent_id;
    }
}

/// Compute aggregate statistics for the entire session tree containing `session_id`.
///
/// 1. Finds the tree root via [`find_tree_root`].
/// 2. Collects ALL sessions in the tree (BFS from root), including frozen nodes.
/// 3. Sums token stats, cost, and turns.
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
pub fn aggregate_tree_stats<S: ::std::hash::BuildHasher>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    frozen_nodes: &HashMap<SessionId, FrozenTreeNode, S>,
    session_id: &SessionId,
) -> TreeAggregateStats {
    let root = find_tree_root(sessions, frozen_nodes, session_id);

    // BFS to collect all sessions and frozen nodes in the tree.
    let mut tree_sessions = Vec::new();
    let mut tree_frozen = Vec::new();
    let mut queue = vec![root];
    let mut visited = HashSet::new();

    while let Some(id) = queue.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }

        if let Some(session) = sessions.get(&id) {
            tree_sessions.push(session);
            // Find live children.
            for (child_id, child) in sessions {
                if child.parent_session().as_ref() == Some(&id) && !visited.contains(child_id) {
                    queue.push(child_id.clone());
                }
            }
            // Find frozen children.
            for (frozen_id, frozen) in frozen_nodes {
                if frozen.parent_session_id.as_ref() == Some(&id) && !visited.contains(frozen_id) {
                    queue.push(frozen_id.clone());
                }
            }
        } else if let Some(frozen) = frozen_nodes.get(&id) {
            tree_frozen.push(frozen);
            // Find live children of this frozen node.
            for (child_id, child) in sessions {
                if child.parent_session().as_ref() == Some(&id) && !visited.contains(child_id) {
                    queue.push(child_id.clone());
                }
            }
            // Find frozen children of this frozen node.
            for (frozen_id, frozen_child) in frozen_nodes {
                if frozen_child.parent_session_id.as_ref() == Some(&id)
                    && !visited.contains(frozen_id)
                {
                    queue.push(frozen_id.clone());
                }
            }
        }
    }

    // Aggregate stats from live sessions.
    let mut stats = TreeAggregateStats::default();

    for session in &tree_sessions {
        let token_stats = TokenStats::from_ledger(session.token_ledger());
        stats.total_sent += token_stats.total_sent;
        stats.total_received += token_stats.total_received;
        stats.total_cost += TokenStats::total_cost(session.token_ledger());
        stats.total_turns += compute_turn_count(session.history(), session.fork_ordinal());
        stats.effective_sent += token_stats.effective_sent;
        stats.measured_sent += token_stats.measured_sent;
        stats.cached_total += token_stats.cached_total;
    }

    // Aggregate stats from frozen nodes.
    for frozen in &tree_frozen {
        stats.total_sent += frozen.total_sent;
        stats.total_received += frozen.total_received;
        stats.total_cost += frozen.total_cost;
        stats.total_turns += frozen.total_turns;
        stats.effective_sent += frozen.effective_sent;
        stats.measured_sent += frozen.measured_sent;
        stats.cached_total += frozen.cached_total;
    }

    stats.session_count = tree_sessions.len() + tree_frozen.len();

    stats
}
