//! History snapshot ready event - emitted by the snapshot actor after cloning
//! history into a shared `Arc<[ChatEntry]>`.
//!
//! Workers subscribe to this instead of `HistoryAppended` to share a single
//! clone across all workers via cheap `Arc` reference counting.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::protocol::ChatEntry;
use crate::protocol::SessionId;

/// Emitted by `HistorySnapshotActor` after cloning history into a shared `Arc`.
///
/// Carries the session's entire history as an `Arc<[ChatEntry]>`, allowing all
/// workers to share the same allocation. The snapshot is taken once per
/// `HistoryAppended` event under a brief read lock, then distributed to all
/// workers through the event bus.
///
/// # Serialization
///
/// This event is purely in-process — it is never persisted or sent over a wire.
/// Manual `Serialize`/`Deserialize` impls serialize only the `session_id` and
/// deserialize with an empty history slice.
#[derive(Debug, Clone)]
pub struct HistorySnapshotReady {
    /// The session whose history was snapshotted.
    pub session_id: SessionId,
    /// Shared history snapshot. Cloning this is O(1) (reference count increment).
    pub history: Arc<[ChatEntry]>,
}

impl Serialize for HistorySnapshotReady {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize only session_id; history is not serializable and is
        // purely in-process. This event is never persisted.
        #[derive(Serialize)]
        struct Slim {
            session_id: SessionId,
        }
        Slim {
            session_id: self.session_id.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HistorySnapshotReady {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deserialize into an empty history — the event is never received
        // via deserialization in practice.
        #[derive(Deserialize)]
        struct Slim {
            session_id: SessionId,
        }
        let slim = Slim::deserialize(deserializer)?;
        Ok(Self {
            session_id: slim.session_id,
            history: Arc::new([]),
        })
    }
}

impl crate::common::bus::BusMessage for HistorySnapshotReady {}
