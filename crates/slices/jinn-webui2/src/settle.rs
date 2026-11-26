//! When each session last landed on Idle — the fact behind the page's
//! unread mark.
//!
//! A turn is not-idle to idle, and only a landed Idle asks for the
//! user's attention, so this tracker records the moment of each
//! `SessionPhaseChanged` whose new phase is Idle. The time rides on every
//! session row the outbound actor ships; the page compares it against
//! its own last-seen time and decides the mark itself, so each browser
//! keeps its own.
//!
//! In memory only, for the process lifetime: a restarted server knows
//! no settle times, so every row carries none until a turn next ends.
//! Nothing here is persisted — an unviewed mark is a within-run signal.
//! Subagent sessions are never recorded, so their rows never mark.

use std::collections::HashMap;
use std::sync::Arc;

use jiff::Timestamp;
use jinn_core_types::SessionId;
use parking_lot::Mutex;

/// The last settle-to-Idle time of each session that has settled.
#[derive(Debug, Clone, Default)]
pub struct SettleTracker {
    last_settled: Arc<Mutex<HashMap<SessionId, Timestamp>>>,
}

impl SettleTracker {
    /// The last settle-to-Idle time recorded for the session.
    ///
    /// `None` when it has not settled since the server started, or when
    /// it is a subagent's.
    #[must_use]
    pub fn last_settled(&self, session_id: &SessionId) -> Option<Timestamp> {
        self.last_settled.lock().get(session_id).copied()
    }

    /// Records that the session settled to Idle at `at`.
    pub fn record(&self, session_id: &SessionId, at: Timestamp) {
        self.last_settled.lock().insert(session_id.clone(), at);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::SettleTracker;
    use jiff::Timestamp;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).expect("a valid timestamp")
    }

    #[rstest::rstest]
    #[test]
    fn an_unsettled_session_has_no_settle_time() {
        // Given a tracker that has recorded nothing.
        let tracker = SettleTracker::default();

        // When asking for a session's settle time.
        let found = tracker.last_settled(&jinn_core_types::SessionId::new());

        // Then there is none.
        assert!(found.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_recorded_settle_is_returned() {
        // Given a tracker that recorded a settle.
        let tracker = SettleTracker::default();
        let id = jinn_core_types::SessionId::new();
        tracker.record(&id, at(10));

        // When asking for that session's settle time.
        let found = tracker.last_settled(&id);

        // Then it is the recorded time.
        assert_eq!(found, Some(at(10)));
    }

    #[rstest::rstest]
    #[test]
    fn a_later_settle_replaces_the_earlier_time() {
        // Given a tracker that recorded a settle at 10.
        let tracker = SettleTracker::default();
        let id = jinn_core_types::SessionId::new();
        tracker.record(&id, at(10));

        // When the session settles again at 20.
        tracker.record(&id, at(20));

        // Then the time is the later one.
        assert_eq!(tracker.last_settled(&id), Some(at(20)));
    }

    #[rstest::rstest]
    #[test]
    fn a_clone_shares_the_recorded_settles() {
        // Given a tracker and a clone of it.
        let tracker = SettleTracker::default();
        let clone = tracker.clone();
        let id = jinn_core_types::SessionId::new();

        // When the original records a settle.
        tracker.record(&id, at(10));

        // Then the clone reads it.
        assert_eq!(clone.last_settled(&id), Some(at(10)));
    }
}
