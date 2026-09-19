//! Integration tests for the history worker pipeline.
//!
//! Tests cover the full lifecycle: worker evaluates history snapshot → produces
//! mutations → actor submits them via message bus.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::Arc;

use crate::common::bus::test_harness::TestHarness;
use crate::feat::history_worker::actor::{HistoryWorkerActor, HistoryWorkerActorDeps};
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::protocol::{ChatEntry, ChatEntryKind, ContextOverride};
use crate::protocol::HistoryMutation;
use crate::feat::session::protocol::history_snapshot_ready::HistorySnapshotReady;
use crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations;
use crate::protocol::{ChangeSource, SessionId};
use kameo::prelude::Spawn;
// ── Test workers ───────────────────────────────────────────────────

/// A test worker that marks all User entries beyond the first 3 as excluded.
#[derive(Clone)]
struct TruncateOldUserEntries;

#[async_trait::async_trait]
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
#[derive(Clone)]
struct NoOpWorker;

#[async_trait::async_trait]
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

// ── Test helpers ───────────────────��───────────────────────────────

fn snapshot_event(session_id: SessionId, entries: Vec<ChatEntry>) -> HistorySnapshotReady {
    HistorySnapshotReady {
        session_id,
        history: Arc::from(entries),
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn worker_produces_mutations_for_long_history() {
    let entries: Vec<ChatEntry> = (0..5)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();
    let worker = TruncateOldUserEntries;
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let mutations =
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(entries)).await }); // 5 entries - 3 kept = 2 excluded
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
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let mutations =
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(entries)).await });
    assert!(mutations.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn actor_publishes_submit_mutations_for_long_history() {
    // Given a history worker actor and a recorder for SubmitHistoryMutations.
    let harness = TestHarness::new().await;
    let _actor = HistoryWorkerActor::spawn(HistoryWorkerActorDeps {
        deps: harness.actor_deps().await,
        worker: TruncateOldUserEntries,
    });

    let recorder = harness.spawn_recorder::<SubmitHistoryMutations>().await;

    let entries: Vec<ChatEntry> = (0..5)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();
    let session_id = SessionId::new();

    // When publishing a HistorySnapshotReady with 5 user entries.
    harness
        .publish(snapshot_event(session_id.clone(), entries))
        .await;

    // Then the worker publishes SubmitHistoryMutations.
    let messages = crate::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        std::time::Duration::from_secs(2),
    )
    .await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].session_id, session_id);
    assert_eq!(messages[0].mutations.len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn actor_publishes_nothing_for_short_history() {
    // Given a history worker actor and a recorder.
    let harness = TestHarness::new().await;
    let _actor = HistoryWorkerActor::spawn(HistoryWorkerActorDeps {
        deps: harness.actor_deps().await,
        worker: TruncateOldUserEntries,
    });

    let recorder = harness.spawn_recorder::<SubmitHistoryMutations>().await;

    let entries: Vec<ChatEntry> = (0..2)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();

    // When publishing a HistorySnapshotReady with 2 entries.
    harness
        .publish(snapshot_event(SessionId::new(), entries))
        .await;

    // Then no mutations are published.
    let messages = crate::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        std::time::Duration::from_secs(2),
    )
    .await;
    assert!(messages.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn noop_worker_never_produces_mutations() {
    // Given a noop worker actor and a recorder.
    let harness = TestHarness::new().await;
    let _actor = HistoryWorkerActor::spawn(HistoryWorkerActorDeps {
        deps: harness.actor_deps().await,
        worker: NoOpWorker,
    });

    let recorder = harness.spawn_recorder::<SubmitHistoryMutations>().await;

    let entries: Vec<ChatEntry> = (0..10)
        .map(|i| ChatEntry::user(format!("msg {i}")))
        .collect();

    // When publishing a HistorySnapshotReady with 10 entries.
    harness
        .publish(snapshot_event(SessionId::new(), entries))
        .await;

    // Then no mutations are published.
    let messages = crate::common::bus::test_harness::await_recorded(
        &recorder,
        1,
        std::time::Duration::from_secs(2),
    )
    .await;
    assert!(messages.is_empty());
}
