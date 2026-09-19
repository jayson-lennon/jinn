//! The `HistoryWorker` trait - pluggable heuristics for history mutation.
//!
//! Each worker inspects a snapshot of the session history and optionally
//! produces a batch of mutations. Workers run outside any lock.
use std::sync::Arc;

use crate::protocol::ChatEntry;
use crate::protocol::HistoryMutation;
use crate::protocol::SessionId;
/// A pluggable history mutation heuristic.
///
/// Each worker inspects a snapshot of the session history and optionally
/// produces a batch of mutations. Workers are run outside any lock,
/// so heuristic evaluation (including LLM calls) never blocks writes.
#[async_trait::async_trait]
pub trait HistoryWorker: Send + Sync + 'static {
    /// Human-readable name for logging and diagnostics.
    fn name(&self) -> &str;

    /// Inspect the history snapshot and optionally produce mutations.
    ///
    /// Called outside any lock. The `history` parameter is a shared snapshot
    /// (via `Arc<[ChatEntry]>`) cloned once by the snapshot actor. The
    /// `session_id` identifies which session triggered the evaluation.
    async fn evaluate(
        &self,
        session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation>;
}
