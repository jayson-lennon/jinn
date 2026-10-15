//! Tests for tree-wide aggregate statistics.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::HashMap;

use crate::tree_aggregate::{FrozenTreeNode, aggregate_tree_stats, find_tree_root};
use crate::{ChatSessionState, SessionMap};
use jinn_core_types::{ChatEntry, SessionId, WorkingInterval};
use jinn_token_count_msg::TokenRecord;

/// Helper: create an empty session with the given ID.
fn make_session(id: SessionId) -> ChatSessionState {
    let mut state = ChatSessionState::new();
    state.set_session_id(id);
    state
}

/// Helper: create a session with token records and history entries.
fn make_session_with_stats(
    id: SessionId,
    tokens_sent: u32,
    tokens_received: u32,
    cost: Option<f64>,
    turn_count: u32,
) -> ChatSessionState {
    let mut state = ChatSessionState::new();
    state.set_session_id(id);

    if tokens_sent > 0 || tokens_received > 0 {
        state.push_token_record(TokenRecord {
            model_used: None,
            timestamp: jiff::Timestamp::now(),
            tokens_sent,
            tokens_received,
            cost,
            prompt_tokens: None,
            cached_tokens: None,
        });
    }

    // Add user entries to produce the desired turn count.
    for i in 0..turn_count {
        state.push_entry(ChatEntry::user(format!("user msg {i}")));
    }

    state
}

/// Helper: set parent session on a session.
fn set_parent(
    sessions: &mut HashMap<SessionId, ChatSessionState>,
    child: &SessionId,
    parent: &SessionId,
) {
    if let Some(session) = sessions.get_mut(child) {
        session.set_parent_session(parent.clone());
    }
}

#[rstest::rstest]
fn single_session_is_own_root() {
    // Given a single session with no parent.
    let id = SessionId::new();
    let mut sessions = HashMap::new();
    sessions.insert(id.clone(), make_session(id.clone()));

    // When finding the root.
    let root = find_tree_root(&sessions, &HashMap::new(), &id);

    // Then it returns the same session.
    assert_eq!(root, id);
}

#[rstest::rstest]
fn child_finds_root() {
    // Given a parent with a child.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();
    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    let mut child = ChatSessionState::new();
    child.set_session_id(child_id.clone());
    child.set_parent_session(parent_id.clone());
    sessions.insert(child_id.clone(), child);

    // When finding root from the child.
    let root = find_tree_root(&sessions, &HashMap::new(), &child_id);

    // Then the root is the parent.
    assert_eq!(root, parent_id);
}

#[rstest::rstest]
fn deeply_nested_finds_root() {
    // Given grandparent → parent → child.
    let gp_id = SessionId::new();
    let parent_id = SessionId::new();
    let child_id = SessionId::new();
    let mut sessions = HashMap::new();
    sessions.insert(gp_id.clone(), make_session(gp_id.clone()));

    let mut parent = ChatSessionState::new();
    parent.set_session_id(parent_id.clone());
    parent.set_parent_session(gp_id.clone());
    sessions.insert(parent_id.clone(), parent);

    let mut child = ChatSessionState::new();
    child.set_session_id(child_id.clone());
    child.set_parent_session(parent_id);
    sessions.insert(child_id.clone(), child);

    // When finding root from the child.
    let root = find_tree_root(&sessions, &HashMap::new(), &child_id);

    // Then the root is the grandparent.
    assert_eq!(root, gp_id);
}

#[rstest::rstest]
fn orphan_session_treated_as_root() {
    // Given a session whose parent_session points to a non-existent ID.
    let orphan_id = SessionId::new();
    let ghost_parent = SessionId::new();
    let mut orphan = ChatSessionState::new();
    orphan.set_session_id(orphan_id.clone());
    orphan.set_parent_session(ghost_parent);

    let mut sessions = HashMap::new();
    sessions.insert(orphan_id.clone(), orphan);

    // When finding root.
    let root = find_tree_root(&sessions, &HashMap::new(), &orphan_id);

    // Then the orphan itself is the root (ghost parent not in map).
    assert_eq!(root, orphan_id);
}

#[rstest::rstest]
fn single_session_returns_own_stats() {
    // Given a single session with known stats.
    let id = SessionId::new();
    let mut sessions = HashMap::new();
    sessions.insert(
        id.clone(),
        make_session_with_stats(id.clone(), 100, 50, Some(0.01), 2),
    );

    // When aggregating tree stats.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &id, &HashMap::new());

    // Then it returns the session's own stats.
    assert_eq!(stats.session_count, 1);
    assert_eq!(stats.total_sent, 100);
    assert_eq!(stats.total_received, 50);
    assert!(stats.total_cost - 0.01 < 1e-10);
    assert_eq!(stats.total_turns, 2); // 2 user entries
}

#[rstest::rstest]
fn parent_with_children_sums_all() {
    // Given a parent with 2 children.
    let parent_id = SessionId::new();
    let child1_id = SessionId::new();
    let child2_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, Some(0.01), 2),
    );
    sessions.insert(
        child1_id.clone(),
        make_session_with_stats(child1_id.clone(), 200, 100, Some(0.02), 1),
    );
    sessions.insert(
        child2_id.clone(),
        make_session_with_stats(child2_id.clone(), 300, 150, Some(0.03), 3),
    );

    set_parent(&mut sessions, &child1_id, &parent_id);
    set_parent(&mut sessions, &child2_id, &parent_id);

    // When aggregating from parent.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then all sessions are summed.
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_sent, 600);
    assert_eq!(stats.total_received, 300);
    assert!(stats.total_cost - 0.06 < 1e-10);
    assert_eq!(stats.total_turns, 6); // 2 + 1 + 3
}

#[rstest::rstest]
fn child_sees_entire_tree() {
    // Given a parent with 2 children.
    let parent_id = SessionId::new();
    let child1_id = SessionId::new();
    let child2_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, None, 1),
    );
    sessions.insert(
        child1_id.clone(),
        make_session_with_stats(child1_id.clone(), 200, 100, None, 2),
    );
    sessions.insert(
        child2_id.clone(),
        make_session_with_stats(child2_id.clone(), 300, 150, None, 3),
    );

    set_parent(&mut sessions, &child1_id, &parent_id);
    set_parent(&mut sessions, &child2_id, &parent_id);

    // When aggregating from child1.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &child1_id, &HashMap::new());

    // Then the result includes parent + both children.
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_sent, 600);
    assert_eq!(stats.total_received, 300);
    assert_eq!(stats.total_turns, 6); // 1 + 2 + 3
}

#[rstest::rstest]
fn deeply_nested_tree_sums_all() {
    // Given grandparent → parent → child.
    let gp_id = SessionId::new();
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        gp_id.clone(),
        make_session_with_stats(gp_id.clone(), 10, 5, None, 1),
    );
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 20, 10, None, 1),
    );
    sessions.insert(
        child_id.clone(),
        make_session_with_stats(child_id.clone(), 30, 15, None, 1),
    );

    set_parent(&mut sessions, &parent_id, &gp_id);
    set_parent(&mut sessions, &child_id, &parent_id);

    // When aggregating from the child.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &child_id, &HashMap::new());

    // Then all 3 sessions are included.
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_sent, 60);
    assert_eq!(stats.total_received, 30);
    assert_eq!(stats.total_turns, 3);
}

#[rstest::rstest]
fn disconnected_sessions_excluded() {
    // Given a parent with a child and a disconnected session.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();
    let disconnected_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, None, 1),
    );
    sessions.insert(
        child_id.clone(),
        make_session_with_stats(child_id.clone(), 200, 100, None, 1),
    );
    sessions.insert(
        disconnected_id.clone(),
        make_session_with_stats(disconnected_id, 999, 999, None, 99),
    );

    set_parent(&mut sessions, &child_id, &parent_id);

    // When aggregating from parent.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then disconnected is excluded.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 300);
    assert_eq!(stats.total_received, 150);
    assert_eq!(stats.total_turns, 2);
}

#[rstest::rstest]
fn frozen_parent_is_found_as_root() {
    // Given a frozen node (archived parent) and a live child.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    // Only the child is live.
    let mut child = ChatSessionState::new();
    child.set_session_id(child_id.clone());
    child.set_parent_session(parent_id.clone());
    sessions.insert(child_id.clone(), child);

    // Parent is archived (frozen).
    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        parent_id.clone(),
        FrozenTreeNode {
            session_id: parent_id.clone(),
            parent_session_id: None,
            total_sent: 0,
            total_received: 0,
            total_cost: 0.0,
            total_turns: 0,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When finding root from the live child.
    let root = find_tree_root(&sessions, &frozen_nodes, &child_id);

    // Then the frozen parent is the root.
    assert_eq!(root, parent_id);
}

#[rstest::rstest]
fn frozen_child_included_in_aggregate() {
    // Given a live root with an archived (frozen) child.
    let root_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        root_id.clone(),
        make_session_with_stats(root_id.clone(), 100, 50, Some(0.01), 2),
    );

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        child_id.clone(),
        FrozenTreeNode {
            session_id: child_id,
            parent_session_id: Some(root_id.clone()),
            total_sent: 200,
            total_received: 100,
            total_cost: 0.02,
            total_turns: 3,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating from the root.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &root_id, &HashMap::new());

    // Then the frozen child's stats are included.
    assert_eq!(stats.session_count, 2); // 1 live + 1 frozen
    assert_eq!(stats.total_sent, 300); // 100 + 200
    assert_eq!(stats.total_received, 150); // 50 + 100
    assert!(stats.total_cost - 0.03 < 1e-10); // 0.01 + 0.02
    assert_eq!(stats.total_turns, 5); // 2 + 3
}

#[rstest::rstest]
fn child_of_frozen_included_in_aggregate() {
    // Given a frozen root with a live child.
    let root_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        child_id.clone(),
        make_session_with_stats(child_id.clone(), 100, 50, Some(0.01), 2),
    );
    // The live child's parent is the frozen root.
    if let Some(s) = sessions.get_mut(&child_id) {
        s.set_parent_session(root_id.clone());
    }

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        root_id.clone(),
        FrozenTreeNode {
            session_id: root_id,
            parent_session_id: None,
            total_sent: 200,
            total_received: 100,
            total_cost: 0.02,
            total_turns: 3,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating from the live child.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &child_id, &HashMap::new());

    // Then both the frozen root and live child are included.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 300);
    assert_eq!(stats.total_received, 150);
    assert!(stats.total_cost - 0.03 < 1e-10);
    assert_eq!(stats.total_turns, 5);
}

#[rstest::rstest]
fn deeply_nested_with_frozen_in_middle() {
    // Given: grandparent (live) -> parent (frozen) -> child (live).
    let gp_id = SessionId::new();
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        gp_id.clone(),
        make_session_with_stats(gp_id.clone(), 10, 5, None, 1),
    );
    sessions.insert(
        child_id.clone(),
        make_session_with_stats(child_id.clone(), 30, 15, None, 1),
    );
    // Child's parent is the frozen parent.
    if let Some(s) = sessions.get_mut(&child_id) {
        s.set_parent_session(parent_id.clone());
    }

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        parent_id.clone(),
        FrozenTreeNode {
            session_id: parent_id,
            parent_session_id: Some(gp_id),
            total_sent: 20,
            total_received: 10,
            total_cost: 0.0,
            total_turns: 1,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating from the grandchild.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &child_id, &HashMap::new());

    // Then all three are included.
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_sent, 60); // 10 + 20 + 30
    assert_eq!(stats.total_received, 30); // 5 + 10 + 15
    assert_eq!(stats.total_turns, 3);
}

#[rstest::rstest]
fn session_count_includes_frozen_nodes() {
    // Given a live root with 2 frozen children.
    let root_id = SessionId::new();
    let child1_id = SessionId::new();
    let child2_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(root_id.clone(), make_session(root_id.clone()));

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        child1_id.clone(),
        FrozenTreeNode {
            session_id: child1_id,
            parent_session_id: Some(root_id.clone()),
            total_sent: 100,
            total_received: 50,
            total_cost: 0.01,
            total_turns: 1,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );
    frozen_nodes.insert(
        child2_id.clone(),
        FrozenTreeNode {
            session_id: child2_id,
            parent_session_id: Some(root_id.clone()),
            total_sent: 200,
            total_received: 100,
            total_cost: 0.02,
            total_turns: 2,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &root_id, &HashMap::new());

    // Then session_count is 3 (1 live + 2 frozen).
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_sent, 300);
    assert_eq!(stats.total_received, 150);
    assert_eq!(stats.total_turns, 3);
}

#[rstest::rstest]
fn frozen_node_not_in_tree_is_excluded() {
    // Given a live session and a frozen node that belongs to a different tree.
    let live_id = SessionId::new();
    let frozen_id = SessionId::new();
    let frozen_root = SessionId::new(); // root of a disconnected tree

    let mut sessions = HashMap::new();
    sessions.insert(
        live_id.clone(),
        make_session_with_stats(live_id.clone(), 100, 50, None, 1),
    );

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        frozen_root.clone(),
        FrozenTreeNode {
            session_id: frozen_root.clone(),
            parent_session_id: None,
            total_sent: 999,
            total_received: 999,
            total_cost: 9.99,
            total_turns: 99,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );
    frozen_nodes.insert(
        frozen_id.clone(),
        FrozenTreeNode {
            session_id: frozen_id,
            parent_session_id: Some(frozen_root),
            total_sent: 888,
            total_received: 888,
            total_cost: 8.88,
            total_turns: 88,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating the live session's tree.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &live_id, &HashMap::new());

    // Then only the live session is included.
    assert_eq!(stats.session_count, 1);
    assert_eq!(stats.total_sent, 100);
    assert_eq!(stats.total_received, 50);
    assert_eq!(stats.total_turns, 1);
}

#[rstest::rstest]
fn all_frozen_tree_aggregates() {
    // Given a tree where ALL nodes are frozen (no live sessions).
    let root_id = SessionId::new();
    let child_id = SessionId::new();

    let sessions = HashMap::new(); // no live sessions

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        root_id.clone(),
        FrozenTreeNode {
            session_id: root_id.clone(),
            parent_session_id: None,
            total_sent: 100,
            total_received: 50,
            total_cost: 0.01,
            total_turns: 2,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );
    frozen_nodes.insert(
        child_id.clone(),
        FrozenTreeNode {
            session_id: child_id,
            parent_session_id: Some(root_id.clone()),
            total_sent: 200,
            total_received: 100,
            total_cost: 0.02,
            total_turns: 3,
            effective_sent: 0,
            measured_sent: 0,
            cached_total: 0,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating from the frozen root.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &root_id, &HashMap::new());

    // Then both frozen nodes are included.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 300);
    assert_eq!(stats.total_received, 150);
    assert!(stats.total_cost - 0.03 < 1e-10);
    assert_eq!(stats.total_turns, 5);
}

#[rstest::rstest]
fn empty_sessions_produce_zeros() {
    // Given sessions with no token records or history.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    let mut child = ChatSessionState::new();
    child.set_session_id(child_id.clone());
    child.set_parent_session(parent_id.clone());
    sessions.insert(child_id, child);

    // When aggregating.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then all values are zero.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 0);
    assert_eq!(stats.total_received, 0);
    assert!(stats.total_cost < f64::EPSILON);
    assert_eq!(stats.total_turns, 0);
}

#[rstest::rstest]
fn forked_session_excluded_from_tree_turn_count() {
    // Given a parent with 2 user entries and a forked child that inherited them.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, Some(0.01), 2),
    );

    // The forked child inherited 2 user entries from the parent.
    let mut child = make_session_with_stats(child_id.clone(), 0, 0, None, 2);
    child.set_parent_session(parent_id.clone());
    child.set_fork_ordinal(1); // inherited both entries (ordinal 0 and 1)
    sessions.insert(child_id, child);

    // When aggregating from the parent.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then parent contributes 2 turns, forked child contributes 0.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_turns, 2);
}

#[rstest::rstest]
fn fork_from_fork_turns_counted_correctly() {
    // Given root (2 user entries) -> fork A (fork_ordinal=1, adds 1 entry) -> fork B (fork_ordinal=2, no new entries).
    let root_id = SessionId::new();
    let fork_a_id = SessionId::new();
    let fork_b_id = SessionId::new();

    let mut sessions = HashMap::new();
    // Root has 2 user entries -> 2 turns.
    sessions.insert(
        root_id.clone(),
        make_session_with_stats(root_id.clone(), 100, 50, None, 2),
    );

    // Fork A inherited entries 0..=1, then added its own entry (index 2) -> 1 turn.
    let mut fork_a = make_session_with_stats(fork_a_id.clone(), 50, 25, None, 3);
    fork_a.set_parent_session(root_id.clone());
    fork_a.set_fork_ordinal(1); // skip inherited entries 0..=1
    sessions.insert(fork_a_id.clone(), fork_a);

    // Fork B inherited entries 0..=2 from Fork A -> 0 turns.
    let mut fork_b = make_session_with_stats(fork_b_id.clone(), 0, 0, None, 3);
    fork_b.set_parent_session(fork_a_id);
    fork_b.set_fork_ordinal(2); // skip inherited entries 0..=2
    sessions.insert(fork_b_id, fork_b);

    // When aggregating from root.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &root_id, &HashMap::new());

    // Then root=2 turns, fork_a=1 turn (entry 2), fork_b=0 turns.
    // No double-counting: each session counts only its own entries.
    assert_eq!(stats.session_count, 3);
    assert_eq!(stats.total_turns, 3); // 2 + 1 + 0
}

#[rstest::rstest]
fn tree_aggregate_sums_cached_total_across_live_and_frozen() {
    // Given a live root with cached hits and a frozen child with cached hits.
    let root_id = SessionId::new();
    let child_id = SessionId::new();

    let mut root = make_session_with_stats(root_id.clone(), 1000, 50, None, 1);
    root.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 10,
        cost: None,
        prompt_tokens: Some(1000),
        cached_tokens: Some(400),
    });

    let mut sessions = HashMap::new();
    sessions.insert(root_id.clone(), root);

    let mut frozen_nodes = HashMap::new();
    frozen_nodes.insert(
        child_id.clone(),
        FrozenTreeNode {
            session_id: child_id,
            parent_session_id: Some(root_id.clone()),
            total_sent: 1000,
            total_received: 50,
            total_cost: 0.0,
            total_turns: 1,
            effective_sent: 1000,
            measured_sent: 1000,
            cached_total: 600,
            working_intervals: Vec::new(),
        },
    );

    // When aggregating from the root.
    let stats = aggregate_tree_stats(&sessions, &frozen_nodes, &root_id, &HashMap::new());

    // Then cached_total sums live + frozen (400 + 600 = 1000).
    assert_eq!(stats.cached_total, 1000);
    // And measured_sent sums live + frozen (1000 + 1000 = 2000).
    assert_eq!(stats.measured_sent, 2000);
}

// ---------------------------------------------------------------------------
// Subagent rollup
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_aggregate_includes_subagent_usage() {
    // Given a parent with stats and a spawned subagent child with its own stats.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, Some(0.01), 2),
    );
    sessions.insert(
        child_id.clone(),
        make_session_with_stats(child_id.clone(), 400, 200, Some(0.04), 1),
    );
    set_parent(&mut sessions, &child_id, &parent_id);

    // When aggregating from the parent.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then the subagent's usage rolls into the tree totals.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 500);
    assert_eq!(stats.total_received, 250);
    assert!(stats.total_cost - 0.05 < 1e-10);
    assert_eq!(stats.total_turns, 3);
}

#[rstest::rstest]
fn spawned_child_crosses_tree_display_threshold() {
    // Given a session map holding an active parent and one spawned subagent child.
    let mut session_map = SessionMap::default();
    let parent_id = session_map.active_session_id().clone();
    let child = ChatSessionState::new_child(&parent_id, true);
    session_map.insert(child);

    // When aggregating tree stats from the active parent's context.
    let tree = aggregate_tree_stats(
        session_map.sessions(),
        session_map.frozen_nodes(),
        session_map.active_session_id(),
        &HashMap::new(),
    );

    // Then the count crosses the >1 display threshold of the status bar.
    assert!(
        tree.session_count > 1,
        "spawning a subagent must surface the tree stats element, got count {}",
        tree.session_count
    );
}

#[rstest::rstest]
fn empty_pre_dispatch_child_contributes_zeros_but_counts() {
    // Given a parent with stats and a freshly spawned child that has been
    // enqueued but has produced nothing yet (no entries, no tokens).
    let parent_id = SessionId::new();

    let mut sessions = HashMap::new();
    sessions.insert(
        parent_id.clone(),
        make_session_with_stats(parent_id.clone(), 100, 50, Some(0.01), 2),
    );
    let child = ChatSessionState::new_child(&parent_id, true);
    let child_id = child.session_id().clone();
    sessions.insert(child_id, child);

    // When aggregating from the parent.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then the totals are unchanged but the child counts as a session.
    assert_eq!(stats.session_count, 2);
    assert_eq!(stats.total_sent, 100);
    assert_eq!(stats.total_received, 50);
    assert!(stats.total_cost - 0.01 < 1e-10);
    assert_eq!(stats.total_turns, 2);
}

// ── Tree working time ─────────────────────────────────────────────────
//
// Working time is the UNION of members' intervals, not the sum. A parent
// blocked on a subagent is itself non-idle, so summing per-session totals
// would bill the shared second once per member.

/// A frozen node carrying only working intervals, for archive tests.
fn frozen_with_working(
    id: &SessionId,
    parent: Option<SessionId>,
    intervals: Vec<WorkingInterval>,
) -> FrozenTreeNode {
    FrozenTreeNode {
        session_id: id.clone(),
        parent_session_id: parent,
        total_sent: 0,
        total_received: 0,
        total_cost: 0.0,
        total_turns: 0,
        effective_sent: 0,
        measured_sent: 0,
        cached_total: 0,
        working_intervals: intervals,
    }
}

fn at(offset_secs: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_000_000_000 + offset_secs).expect("valid offset")
}

#[rstest::rstest]
fn tree_working_time_counts_a_shared_second_once() {
    // Given a parent working 0..10 and a child working 4..14 entirely inside it.
    let parent_id = SessionId::new();
    let child = ChatSessionState::new_child(&parent_id, true);
    let child_id = child.session_id().clone();

    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    sessions.insert(child_id.clone(), child);

    let mut live = HashMap::new();
    live.insert(
        parent_id.clone(),
        vec![WorkingInterval::closed(at(0), at(10))],
    );
    live.insert(
        child_id.clone(),
        vec![WorkingInterval::closed(at(4), at(14))],
    );

    // When aggregating the tree.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &live);

    // Then the total is the 14-second span, not 10 + 10.
    assert_eq!(stats.total_working, jiff::SignedDuration::from_secs(14));
}

#[rstest::rstest]
fn tree_working_time_sums_members_that_worked_apart() {
    // Given a parent working 0..10 and a child working 20..25, well clear.
    let parent_id = SessionId::new();
    let child = ChatSessionState::new_child(&parent_id, true);
    let child_id = child.session_id().clone();

    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    sessions.insert(child_id.clone(), child);

    let mut live = HashMap::new();
    live.insert(
        parent_id.clone(),
        vec![WorkingInterval::closed(at(0), at(10))],
    );
    live.insert(
        child_id.clone(),
        vec![WorkingInterval::closed(at(20), at(25))],
    );

    // When aggregating the tree.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &live);

    // Then the disjoint spans add up.
    assert_eq!(stats.total_working, jiff::SignedDuration::from_secs(15));
}

#[rstest::rstest]
fn archiving_a_member_does_not_change_the_trees_working_time() {
    // Given a parent that worked 10 seconds with a child that worked the
    // same 10 seconds, entirely inside it.
    let parent_id = SessionId::new();
    let child_id = SessionId::new();
    let shared = WorkingInterval::closed(at(0), at(10));

    let mut live = HashMap::new();
    live.insert(parent_id.clone(), vec![shared.clone()]);
    live.insert(child_id.clone(), vec![shared.clone()]);

    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    sessions.insert(child_id.clone(), make_session(child_id.clone()));

    // When aggregating with both members live.
    let before = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &live);

    // And then the child is archived, becoming a frozen node that carries the
    // intervals it contributed.
    sessions.remove(&child_id);
    let mut frozen = HashMap::new();
    frozen.insert(
        child_id.clone(),
        frozen_with_working(&child_id, Some(parent_id.clone()), vec![shared]),
    );

    // When aggregating again.
    let after = aggregate_tree_stats(&sessions, &frozen, &parent_id, &live);

    // Then the tree's working time is unchanged by the archive.
    assert_eq!(after.total_working, before.total_working);
    assert_eq!(after.total_working, jiff::SignedDuration::from_secs(10));
}

#[rstest::rstest]
fn a_frozen_member_alone_still_contributes_its_working_time() {
    // Given a tree whose only member is a frozen node.
    let root_id = SessionId::new();
    let mut frozen = HashMap::new();
    frozen.insert(
        root_id.clone(),
        frozen_with_working(&root_id, None, vec![WorkingInterval::closed(at(0), at(42))]),
    );

    // When aggregating.
    let stats = aggregate_tree_stats(&HashMap::new(), &frozen, &root_id, &HashMap::new());

    // Then the archived member's time is counted.
    assert_eq!(stats.total_working, jiff::SignedDuration::from_secs(42));
}

#[rstest::rstest]
fn a_tree_with_no_recorded_working_time_totals_zero() {
    // Given a tree whose members have never worked.
    let parent_id = SessionId::new();
    let child = ChatSessionState::new_child(&parent_id, true);
    let child_id = child.session_id().clone();
    let mut sessions = HashMap::new();
    sessions.insert(parent_id.clone(), make_session(parent_id.clone()));
    sessions.insert(child_id, child);

    // When aggregating.
    let stats = aggregate_tree_stats(&sessions, &HashMap::new(), &parent_id, &HashMap::new());

    // Then the total is zero.
    assert_eq!(stats.total_working, jiff::SignedDuration::ZERO);
}
