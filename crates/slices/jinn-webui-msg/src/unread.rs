//! The unread-session mark cell — which sessions have work the user
//! has not viewed.
//!
//! The payload is the marked-session id set; the slot key names where
//! it lives in the shared registry. The webui fan-out actor is the
//! sole writer (set on a settle-to-idle of a non-active,
//! non-subagent-origin session; clear when that session becomes the
//! active one); the sessions-list render paths resolve read handles
//! and compute the per-row `unread` flag. The mark is process-local:
//! a fresh boot starts from an empty set.

use std::collections::HashSet;

use jinn_core_types::SessionId;
use jinn_slices::SlotKey;

/// The slot key the webui unread-mark cell lives under.
#[must_use]
pub fn webui_unread_slot() -> SlotKey {
    SlotKey::builtin("webui", "unread")
}

/// The sessions wearing the unread mark, as one set.
///
/// The set methods return whether they *transitioned* the set — the
/// fact the fan-out refreshes every page on, and only on. A set that
/// already held the id, or a clear of an unmarked id, is a no-op the
/// callers stay quiet about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnreadSessionMarks {
    /// The marked sessions' ids.
    pub marks: HashSet<SessionId>,
}

impl UnreadSessionMarks {
    /// Whether the session currently wears the mark.
    #[must_use]
    pub fn is_marked(&self, session_id: &SessionId) -> bool {
        self.marks.contains(session_id)
    }

    /// Marks the session unread.
    ///
    /// Returns `true` when the mark is new — the transition a set
    /// refresh is worth. Marking an already-marked session is `false`.
    pub fn mark(&mut self, session_id: SessionId) -> bool {
        self.marks.insert(session_id)
    }

    /// Clears the session's mark — the user has now viewed it.
    ///
    /// Returns `true` when a mark was actually removed — the
    /// transition a set refresh is worth. Clearing an unmarked
    /// session is `false`.
    pub fn clear(&mut self, session_id: &SessionId) -> bool {
        self.marks.remove(session_id)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::UnreadSessionMarks;
    use jinn_core_types::SessionId;

    #[rstest::rstest]
    #[test]
    fn a_fresh_set_marks_nothing() {
        // Given a fresh mark set.
        let marks = UnreadSessionMarks::default();

        // Then no session is marked.
        assert!(!marks.is_marked(&SessionId::new()));
    }

    #[rstest::rstest]
    #[test]
    fn marking_an_unmarked_session_is_a_transition() {
        // Given a fresh mark set and a session id.
        let mut marks = UnreadSessionMarks::default();
        let id = SessionId::new();

        // When marking the session.
        let transitioned = marks.mark(id.clone());

        // Then the mark landed and reports itself as news.
        assert!(transitioned);
        assert!(marks.is_marked(&id));
    }

    #[rstest::rstest]
    #[test]
    fn marking_an_already_marked_session_is_not_news() {
        // Given a set already holding the session's mark.
        let mut marks = UnreadSessionMarks::default();
        let id = SessionId::new();
        marks.mark(id.clone());

        // When marking the same session again.
        let transitioned = marks.mark(id.clone());

        // Then nothing changed — a second settle of the same work is
        // not a new fact.
        assert!(!transitioned);
    }

    #[rstest::rstest]
    #[test]
    fn clearing_a_mark_is_a_transition_and_the_next_clear_is_not() {
        // Given a set holding one mark.
        let mut marks = UnreadSessionMarks::default();
        let id = SessionId::new();
        marks.mark(id.clone());

        // When clearing it, then clearing it again.
        let first = marks.clear(&id);
        let second = marks.clear(&id);

        // Then only the first clear removed anything.
        assert!(first);
        assert!(!second);
        assert!(!marks.is_marked(&id));
    }
}
