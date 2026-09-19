//! Tests for the prune actor: snapshot → strategy evaluation → mutation
//! publish, plus the in-flight skip and empty-worker short-circuit.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;

use super::PruneActor;
use async_trait::async_trait;
use jinn_core_types::HistoryMutation;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride, SessionId};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusAudit;
use jinn_domain::common::state::State;
use jinn_session_history_msg::{HistoryAppended, SubmitHistoryMutations};

use crate::worker::HistoryWorker;

// ── Test workers ─────────────────────────────────────────────────────

/// A test worker that marks all User entries beyond the first 3 as excluded.
struct TruncateOldUserEntries;

#[async_trait]
impl HistoryWorker for TruncateOldUserEntries {
    fn name(&self) -> &'static str {
        "test-truncate-old-user"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let user_entries: Vec<_> = history
            .iter()
            .filter(|e| matches!(e.kind, ChatEntryKind::User { .. }))
            .collect();

        if user_entries.len() <= 3 {
            return vec![];
        }

        // Mark all but last 3 user entries as excluded.
        let to_exclude = user_entries.len() - 3;
        user_entries[..to_exclude]
            .iter()
            .map(|e| HistoryMutation::SetContextOverride {
                entry_id: e.id.clone(),
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Internal {
                    label: "test".into(),
                },
            })
            .collect()
    }
}

/// A test worker that always produces an empty result.
struct NoOpWorker;

#[async_trait]
impl HistoryWorker for NoOpWorker {
    fn name(&self) -> &'static str {
        "test-noop"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        _history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        vec![]
    }
}

// ── Test helpers ─────────────────────────────────────────────────────

async fn create_actor(workers: Vec<Box<dyn HistoryWorker>>) -> (PruneActor, State, BusAudit) {
    let (bus, audit) = jinn_domain::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus).await;
    let state = State::new(AppState::default_with_scope_focus());
    (
        PruneActor {
            state: state.clone(),
            services,
            workers,
            in_flight: std::collections::HashSet::new(),
        },
        state,
        audit,
    )
}

fn appended_event(session_id: &SessionId) -> HistoryAppended {
    HistoryAppended {
        session_id: session_id.clone(),
    }
}

// ── Strategy-contract tests (ported from the kernel pipeline suite) ──

#[rstest::rstest]
#[test]
fn worker_produces_mutations_for_long_history() {
    let entries: Vec<ChatEntry> = (0..5)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();
    let worker = TruncateOldUserEntries;
    let mutations = {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(entries)).await })
    };
    // 5 entries - 3 kept = 2 excluded.
    assert_eq!(mutations.len(), 2);
    for m in &mutations {
        if let HistoryMutation::SetContextOverride { value, .. } = m {
            assert!(matches!(value, ContextOverride::ForcedExclude));
        } else {
            panic!("expected SetContextOverride mutation");
        }
    }
}

#[rstest::rstest]
#[test]
fn worker_produces_no_mutations_for_short_history() {
    let entries: Vec<ChatEntry> = (0..3)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();
    let worker = TruncateOldUserEntries;
    let mutations = {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(entries)).await })
    };
    assert!(mutations.is_empty());
}

// ── PruneActor behavior ──────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn prune_actor_publishes_submit_mutations_for_long_history() {
    // Given a prune actor with the truncate strategy and a session with 5 user entries.
    let (mut actor, state, audit) = create_actor(vec![Box::new(TruncateOldUserEntries)]).await;
    let sid = SessionId::new();
    {
        let mut guard = state.write_test_no_cap();
        let session = guard.session_mut_or_create(&sid);
        for i in 0..5 {
            session
                .edit_history()
                .append(ChatEntry::user(format!("msg {i}")));
        }
    }

    // When handling HistoryAppended for that session.
    actor.handle_history_appended(&appended_event(&sid)).await;

    // Then SubmitHistoryMutations is published with 2 excludes.
    let submits: Vec<SubmitHistoryMutations> = audit.of_type::<SubmitHistoryMutations>();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits.first().expect("one submit").mutations.len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn prune_actor_publishes_nothing_for_short_history() {
    // Given a prune actor and a session with 2 user entries.
    let (mut actor, state, audit) = create_actor(vec![Box::new(TruncateOldUserEntries)]).await;
    let sid = SessionId::new();
    {
        let mut guard = state.write_test_no_cap();
        let session = guard.session_mut_or_create(&sid);
        for i in 0..2 {
            session
                .edit_history()
                .append(ChatEntry::user(format!("msg {i}")));
        }
    }

    // When handling HistoryAppended for that session.
    actor.handle_history_appended(&appended_event(&sid)).await;

    // Then no SubmitHistoryMutations is published.
    let submits: Vec<SubmitHistoryMutations> = audit.of_type::<SubmitHistoryMutations>();
    assert!(submits.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn noop_strategy_never_produces_mutations() {
    // Given a prune actor whose only strategy always returns empty.
    let (mut actor, _state, audit) = create_actor(vec![Box::new(NoOpWorker)]).await;
    let sid = SessionId::new();

    // When handling HistoryAppended.
    actor.handle_history_appended(&appended_event(&sid)).await;

    // Then no SubmitHistoryMutations is published.
    let submits: Vec<SubmitHistoryMutations> = audit.of_type::<SubmitHistoryMutations>();
    assert!(submits.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn prune_actor_with_no_strategies_short_circuits() {
    // Given a prune actor with zero strategies (all disabled in config).
    let (mut actor, _state, audit) = create_actor(Vec::new()).await;
    let sid = SessionId::new();

    // When handling HistoryAppended.
    actor.handle_history_appended(&appended_event(&sid)).await;

    // Then no SubmitHistoryMutations is published.
    let submits: Vec<SubmitHistoryMutations> = audit.of_type::<SubmitHistoryMutations>();
    assert!(submits.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn prune_actor_skips_evaluation_for_unknown_session() {
    // Given a prune actor and no session in state.
    let (mut actor, _state, audit) = create_actor(vec![Box::new(TruncateOldUserEntries)]).await;
    let sid = SessionId::new();

    // When handling HistoryAppended for the unknown session.
    actor.handle_history_appended(&appended_event(&sid)).await;

    // Then no SubmitHistoryMutations is published (snapshot found no session).
    let submits: Vec<SubmitHistoryMutations> = audit.of_type::<SubmitHistoryMutations>();
    assert!(submits.is_empty());
}
