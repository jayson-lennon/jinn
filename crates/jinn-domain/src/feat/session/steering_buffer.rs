//! Per-session steering buffer for mid-turn message injection.
//!
//! Steering messages are submitted while the LLM is mid-turn (not Idle) and
//! buffered for injection at the next prompt assembly boundary. On drain, all
//! accumulated fragments are concatenated into a single [`ChatEntry`] of kind
//! `User` and inserted at the tail of working history.
//!
//! Lifecycle:
//! - Buffer is in-memory only; not persisted across session restart.
//! - Multiple submits accumulate FIFO; drain produces one entry.
//! - Separator between fragments is `\n\n`.

use crate::protocol::ChatEntry;

/// Accumulator for pending steering fragments.
///
/// Held inside [`crate::feat::session::chat_session::SessionCoreEphemeral`]
/// so it is dropped on session close and on application restart.
#[derive(Debug, Default, Clone)]
pub struct SteeringBuffer {
    fragments: Vec<String>,
}

impl SteeringBuffer {
    /// Construct an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a new fragment to the buffer.
    ///
    /// Fragments are stored in submission order and concatenated on drain.
    pub fn push_fragment<T: Into<String>>(&mut self, fragment: T) {
        self.fragments.push(fragment.into());
    }

    /// Number of fragments currently buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    /// Whether no fragments are currently buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// Concatenate all buffered fragments into a single `User` entry and clear
    /// the buffer.
    ///
    /// Returns `None` when the buffer is empty. The produced entry has equal
    /// display and expanded text; prompt-token (`#name`) expansion happens later
    /// in [`ChatSessionState::push_entry`], just like every other user entry.
    ///
    /// [`ChatSessionState::push_entry`]: crate::feat::session::chat_session::ChatSessionState::push_entry
    pub fn drain_into_entry(&mut self) -> Option<ChatEntry> {
        if self.fragments.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.fragments).join("\n\n");
        Some(ChatEntry::user_expanded(text.clone(), text))
    }

    /// Drain all buffered fragments, returning them in submission order.
    ///
    /// Unlike [`Self::drain_into_entry`], this returns the raw fragments without
    /// joining, so callers can apply their own separator. Clears the buffer.
    pub fn drain_fragments(&mut self) -> Vec<String> {
        std::mem::take(&mut self.fragments)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::{ChatEntryKind, ContextOverride};

    #[rstest::rstest]
    #[test]
    fn new_buffer_is_empty() {
        let buf = SteeringBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn push_increments_len() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        assert_eq!(buf.len(), 1);
        assert!(!buf.is_empty());
        buf.push_fragment("b");
        assert_eq!(buf.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn drain_empty_returns_none() {
        let mut buf = SteeringBuffer::new();
        assert!(buf.drain_into_entry().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn drain_single_fragment_produces_user_entry() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("hello");

        let entry = buf.drain_into_entry().expect("entry");
        match entry.kind {
            ChatEntryKind::User {
                display, expanded, ..
            } => {
                assert_eq!(display, "hello");
                assert_eq!(expanded, "hello");
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn drain_multiple_fragments_joined_by_blank_line_in_order() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        buf.push_fragment("b");
        buf.push_fragment("c");

        let entry = buf.drain_into_entry().expect("entry");
        match entry.kind {
            ChatEntryKind::User {
                display, expanded, ..
            } => {
                assert_eq!(display, "a\n\nb\n\nc");
                assert_eq!(expanded, "a\n\nb\n\nc");
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn drain_clears_buffer() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        buf.push_fragment("b");

        let _ = buf.drain_into_entry();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn drain_after_drain_returns_none() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        let _ = buf.drain_into_entry();
        assert!(buf.drain_into_entry().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn push_after_drain_starts_fresh() {
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("first");
        let _ = buf.drain_into_entry();
        buf.push_fragment("second");
        buf.push_fragment("third");

        let entry = buf.drain_into_entry().expect("entry");
        match entry.kind {
            ChatEntryKind::User { expanded, .. } => assert_eq!(expanded, "second\n\nthird"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn drained_entry_has_no_pin_and_normal_context_fields() {
        // Given a buffer with one fragment.
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("stay focused");

        // When draining.
        let entry = buf.drain_into_entry().expect("entry");

        // Then the entry has no pin (compaction-eligible like any normal User).
        assert!(
            entry.pin_position.is_none(),
            "drained steering entry must not be pinned"
        );
        // And the kind is User (not System / ToolResult / etc).
        assert!(
            matches!(entry.kind, ChatEntryKind::User { .. }),
            "drained steering entry must be a normal User entry"
        );
        // And context_override is the default (not pinned, not excluded).
        assert_eq!(
            entry.context_override,
            ContextOverride::Default,
            "drained steering entry must use default context override"
        );
    }

    #[rstest::rstest]
    #[test]
    fn drain_fragments_empty_returns_empty_vec() {
        // Given an empty buffer.
        let mut buf = SteeringBuffer::new();

        // When draining fragments.
        let fragments = buf.drain_fragments();

        // Then an empty vec is returned.
        assert!(fragments.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn drain_fragments_returns_fragments_in_order() {
        // Given a buffer with three fragments.
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        buf.push_fragment("b");
        buf.push_fragment("c");

        // When draining fragments.
        let fragments = buf.drain_fragments();

        // Then fragments are returned in submission order.
        assert_eq!(fragments, vec!["a", "b", "c"]);
    }

    #[rstest::rstest]
    #[test]
    fn drain_fragments_clears_buffer() {
        // Given a buffer with two fragments.
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("a");
        buf.push_fragment("b");

        // When draining fragments.
        let _ = buf.drain_fragments();

        // Then the buffer is empty.
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn drain_fragments_after_drain_into_entry_starts_fresh() {
        // Given a buffer drained via drain_into_entry then refilled.
        let mut buf = SteeringBuffer::new();
        buf.push_fragment("first");
        let _ = buf.drain_into_entry();
        buf.push_fragment("second");
        buf.push_fragment("third");

        // When draining fragments.
        let fragments = buf.drain_fragments();

        // Then only the newly pushed fragments are returned.
        assert_eq!(fragments, vec!["second", "third"]);
    }
}
