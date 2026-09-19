//! Snapshot actor — the single point that clones history into an `Arc`.
//!
//! Subscribes to [`HistoryAppended`] events. On each event, acquires a brief
//! read lock on shared state, clones the session history into `Arc<[ChatEntry]>`,
//! and publishes [`HistorySnapshotReady`]. All history workers subscribe to that
//! event instead of `HistoryAppended`, sharing a single allocation.

use std::sync::Arc;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::state::State;
use crate::feat::session::protocol::history_appended::HistoryAppended;
use crate::feat::session::protocol::history_snapshot_ready::HistorySnapshotReady;
use crate::protocol::ChatEntry;
use kameo::prelude::{Actor, ActorRef, Context, Message};

/// Actor that creates shared history snapshots for workers.
///
/// This is the **only** place that calls `session.history().to_vec()`.
/// All workers receive the resulting `Arc<[ChatEntry]>` via the
/// [`HistorySnapshotReady`] event.
pub struct HistorySnapshotActor {
    deps: ActorDeps,
    state: State,
}

/// Dependencies for spawning a [`HistorySnapshotActor`].
#[derive(Clone)]
pub struct HistorySnapshotActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// Shared application state (for reading session history).
    pub state: State,
}

impl Actor for HistorySnapshotActor {
    type Args = HistorySnapshotActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<HistoryAppended>())
            .await;

        Ok(Self {
            deps: args.deps,
            state: args.state,
        })
    }
}

impl Message<HistoryAppended> for HistorySnapshotActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HistoryAppended,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        tracing::debug!(
            session_id = %msg.session_id,
            "HistoryAppended received, creating snapshot"
        );

        // Brief read lock → clone history into Arc → drop lock.
        let history: Arc<[ChatEntry]> = {
            let state = self.state.read();
            let Some(session) = state.session.get(&msg.session_id) else {
                tracing::debug!(
                    session_id = %msg.session_id,
                    "session not found, skipping snapshot"
                );
                return;
            };
            Arc::from(session.history().to_vec())
        };

        tracing::debug!(
            session_id = %msg.session_id,
            entries = history.len(),
            "history snapshot created"
        );

        self.publish(HistorySnapshotReady {
            session_id: msg.session_id,
            history,
        })
        .await;
    }
}

impl BusPublish for HistorySnapshotActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use std::time::Duration;

    use crate::common::app_state::AppState;
    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::common::state::State;
    use crate::feat::session::protocol::history_appended::HistoryAppended;
    use crate::feat::session::protocol::history_snapshot_ready::HistorySnapshotReady;
    use crate::protocol::ChatEntry;
    use crate::protocol::SessionId;

    use super::{HistorySnapshotActor, HistorySnapshotActorDeps};

    fn test_state_with_session(entries: Vec<ChatEntry>) -> (State, SessionId) {
        let mut session = crate::feat::session::chat_session::ChatSessionState::new();
        for entry in entries {
            session.push_entry(entry);
        }
        let id = session.session_id().clone();
        let state = State::new(AppState::default());
        {
            let mut app = state.write_test_no_cap();
            app.session.insert(session);
        }
        (state, id)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn snapshot_contains_all_entries() {
        // Given a session with 5 entries.
        let entries: Vec<ChatEntry> = (0..5)
            .map(|i| ChatEntry::user(format!("msg {i}")))
            .collect();
        let (state, session_id) = test_state_with_session(entries);
        let harness = TestHarness::new().await;
        let _actor = harness
            .spawn_actor::<HistorySnapshotActor>(HistorySnapshotActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
            })
            .await;
        let recorder = harness.spawn_recorder::<HistorySnapshotReady>().await;

        // When publishing HistoryAppended.
        harness
            .publish(HistoryAppended {
                session_id: session_id.clone(),
            })
            .await;

        // Then a snapshot is published with all 5 entries.
        let snapshots = await_recorded(&recorder, 1, Duration::from_secs(2)).await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].session_id, session_id);
        assert_eq!(snapshots[0].history.len(), 5);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn nonexistent_session_produces_no_event() {
        // Given an actor with no sessions.
        let state = State::new(AppState::default());
        let harness = TestHarness::new().await;
        let _actor = harness
            .spawn_actor::<HistorySnapshotActor>(HistorySnapshotActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
            })
            .await;
        let recorder = harness.spawn_recorder::<HistorySnapshotReady>().await;

        // When publishing HistoryAppended for a nonexistent session.
        harness
            .publish(HistoryAppended {
                session_id: SessionId::new(),
            })
            .await;

        // Then no snapshot is published.
        let snapshots = await_recorded(&recorder, 1, Duration::from_millis(200)).await;
        assert!(snapshots.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_history_produces_empty_snapshot() {
        // Given a session with no entries.
        let (state, session_id) = test_state_with_session(vec![]);
        let harness = TestHarness::new().await;
        let _actor = harness
            .spawn_actor::<HistorySnapshotActor>(HistorySnapshotActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
            })
            .await;
        let recorder = harness.spawn_recorder::<HistorySnapshotReady>().await;

        // When publishing HistoryAppended.
        harness
            .publish(HistoryAppended {
                session_id: session_id.clone(),
            })
            .await;

        // Then a snapshot is published with 0 entries.
        let snapshots = await_recorded(&recorder, 1, Duration::from_secs(2)).await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].session_id, session_id);
        assert_eq!(snapshots[0].history.len(), 0);
    }
}
