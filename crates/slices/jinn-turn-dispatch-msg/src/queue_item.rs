//! Session turn dispatch queue items.
//!
//! The queue drives all turn transitions through a single dispatch point.
//! Each item type maps to a specific action when processed by the queue
//! processor in the session actor.

use jinn_core_types::ChatEntry;
use serde::{Deserialize, Serialize};

/// Items in the session turn dispatch queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QueueItem {
    /// A user-submitted message to send to the LLM.
    ///
    /// Boxed because `ChatEntry` is large (~208 B) and the queue stores
    /// many items — boxing shrinks each slot to a pointer.
    UserMessage(Box<ChatEntry>),
    /// Continue after a tool batch - re-assemble prompt with updated history.
    ToolContinuation,
}
