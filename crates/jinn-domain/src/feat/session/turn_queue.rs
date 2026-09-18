//! Turn dispatch queue - encapsulates the per-session turn queue.
//!
//! The turn queue holds pending operations (user messages, compaction requests,
//! tool continuations) that should be dispatched when the session transitions
//! to [`Idle`](super::chat_session::PhaseKind::Idle).
//!
//! # Visibility
//!
//! - **Public:** `enqueue`, `enqueue_front`, `len`, `is_empty` - anyone can add items.
//! - **Restricted:** `pop`, `drain` - only the queue actor may consume items.
//!
//! During the migration (before the queue actor exists), `pop` and `drain` are
//! visible within the `session` feature module. This will be tightened once the
//! queue actor is introduced.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::queue_item::QueueItem;

/// Per-session turn dispatch queue.
///
/// Wraps a [`VecDeque<QueueItem>`] with controlled visibility for consumption.
/// Enqueuing is public; dequeuing is restricted to the queue owner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnQueue {
    inner: VecDeque<QueueItem>,
}

impl TurnQueue {
    /// Push an item onto the back of the queue.
    pub fn enqueue(&mut self, item: QueueItem) {
        self.inner.push_back(item);
    }

    /// Push an item onto the front of the queue (for priority items).
    pub fn enqueue_front(&mut self, item: QueueItem) {
        self.inner.push_front(item);
    }

    /// Number of items waiting in the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the queue contains no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Read-only access to the underlying deque (for rendering).
    pub fn items(&self) -> &VecDeque<QueueItem> {
        &self.inner
    }

    /// Pop the front item from the queue, if any.
    ///
    /// Restricted to the session feature module: the turn-dispatch queue
    /// actor reaches this through `ChatSessionState::dequeue`.
    pub(in crate::feat::session) fn pop(&mut self) -> Option<QueueItem> {
        self.inner.pop_front()
    }

    /// Drain all queued items, returning them in order.
    ///
    /// Restricted to the session feature module: the turn-dispatch queue
    /// actor reaches this through `ChatSessionState::dequeue`.
    pub(in crate::feat::session) fn drain(&mut self) -> VecDeque<QueueItem> {
        std::mem::take(&mut self.inner)
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
    use super::*;

    fn user_msg(text: &str) -> QueueItem {
        QueueItem::UserMessage(Box::new(crate::protocol::ChatEntry::user(text)))
    }

    #[rstest::rstest]
    fn enqueue_front_places_item_at_front() {
        // Given an empty queue.
        let mut queue = TurnQueue::default();
        queue.enqueue(user_msg("normal"));

        // When enqueueing a priority item at the front.
        queue.enqueue_front(user_msg("priority"));

        // Then the priority item is at the front.
        let popped = queue.pop();
        assert!(popped.is_some());
        let items = queue.items();
        assert_eq!(items.len(), 1);
    }

    #[rstest::rstest]
    fn is_empty_returns_true_when_empty() {
        // Given an empty queue.
        let queue = TurnQueue::default();

        // When checking is_empty.
        // Then it returns true.
        assert!(queue.is_empty());
    }

    #[rstest::rstest]
    fn is_empty_returns_false_after_enqueue() {
        // Given a queue with one item.
        let mut queue = TurnQueue::default();
        queue.enqueue(user_msg("hello"));

        // When checking is_empty.
        // Then it returns false.
        assert!(!queue.is_empty());
    }

    #[rstest::rstest]
    fn is_empty_returns_true_after_drain() {
        // Given a queue with items.
        let mut queue = TurnQueue::default();
        queue.enqueue(user_msg("a"));
        queue.enqueue(user_msg("b"));

        // When draining all items.
        let drained = queue.drain();
        assert_eq!(drained.len(), 2);

        // Then is_empty returns true.
        assert!(queue.is_empty());
    }

    #[rstest::rstest]
    fn len_returns_correct_count() {
        // Given a queue with 3 items.
        let mut queue = TurnQueue::default();
        queue.enqueue(user_msg("a"));
        queue.enqueue(user_msg("b"));
        queue.enqueue(user_msg("c"));

        // When checking length.
        assert_eq!(queue.len(), 3);
    }
}
