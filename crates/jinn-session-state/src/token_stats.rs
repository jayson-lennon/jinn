//! Recursive token aggregation for a session's descendants.
//!
//! Shared ledger values live in `jinn-token-count-msg`. This module aggregates
//! them across a session map keyed by [`SessionId`].

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_token_count_msg::{AggregatedTokenStats, TokenStats};

use crate::ChatSessionState;

/// Aggregates token statistics for a session and all descendants.
pub fn aggregate_session_stats<S>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    session_id: &SessionId,
) -> AggregatedTokenStats
where
    S: std::hash::BuildHasher,
{
    let own_session = sessions.get(session_id);
    let own = own_session.map_or_default(|session| TokenStats::from_ledger(session.token_ledger()));
    let children = aggregate_children(sessions, session_id);
    let own_cost = own_session.map_or(0.0, |session| {
        TokenStats::total_cost(session.token_ledger())
    });
    let children_cost = aggregate_children_cost(sessions, session_id);

    AggregatedTokenStats {
        own,
        children,
        own_cost,
        children_cost,
    }
}

fn aggregate_children<S>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    parent_id: &SessionId,
) -> TokenStats
where
    S: std::hash::BuildHasher,
{
    let mut total = TokenStats::default();
    for (id, session) in sessions {
        if session.parent_session().as_ref() == Some(parent_id) {
            let child_own = TokenStats::from_ledger(session.token_ledger());
            let child_descendants = aggregate_children(sessions, id);
            total.total_sent += child_own.total_sent + child_descendants.total_sent;
            total.total_received += child_own.total_received + child_descendants.total_received;
            total.request_count += child_own.request_count + child_descendants.request_count;
            total.effective_sent += child_own.effective_sent + child_descendants.effective_sent;
            total.measured_sent += child_own.measured_sent + child_descendants.measured_sent;
            total.cached_total += child_own.cached_total + child_descendants.cached_total;
        }
    }
    total
}

fn aggregate_children_cost<S>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    parent_id: &SessionId,
) -> f64
where
    S: std::hash::BuildHasher,
{
    let mut total = 0.0;
    for (id, session) in sessions {
        if session.parent_session().as_ref() == Some(parent_id) {
            let own = TokenStats::total_cost(session.token_ledger());
            let descendants = aggregate_children_cost(sessions, id);
            total += own + descendants;
        }
    }
    total
}
