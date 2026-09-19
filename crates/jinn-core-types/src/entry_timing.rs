//! Timing data for chat entries.
//!
//! Every [`ChatEntry`](crate::ChatEntry) carries an [`EntryTiming`] that records when it was
//! created, and — for entries that go through a streaming lifecycle — how long the LLM took to
//! dispatch, produce its first token, and finish.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Timing data for a chat entry.
///
/// - `Instant`: the entry was created and finalized in a single step (e.g. a user message).
/// - `Streamed`: the entry went through a multi-step lifecycle (LLM dispatch → first token → finish).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryTiming {
    /// Entry was created and finalized in one step.
    Instant {
        /// When the entry was created.
        at: Timestamp,
    },
    /// Entry went through a streaming lifecycle.
    Streamed {
        /// When the LLM request was dispatched.
        dispatched_at: Timestamp,
        /// When the first token/event arrived that created this entry.
        first_token_at: Option<Timestamp>,
        /// When the entry was finalized (stream completed, tool finished).
        finished_at: Option<Timestamp>,
    },
}

impl EntryTiming {
    /// Create an `Instant` timing at the current moment.
    #[must_use]
    pub fn instant_now() -> Self {
        Self::Instant {
            at: Timestamp::now(),
        }
    }

    /// Create a `Streamed` timing with only the dispatch timestamp.
    ///
    /// `first_token_at` and `finished_at` start as `None` and are populated later.
    #[must_use]
    pub fn streamed(dispatched_at: Timestamp) -> Self {
        Self::Streamed {
            dispatched_at,
            first_token_at: None,
            finished_at: None,
        }
    }

    /// Record the first-token arrival time.
    ///
    /// No-op for `Instant` variants (their time is already known).
    pub fn set_first_token(&mut self) {
        if let Self::Streamed { first_token_at, .. } = self {
            *first_token_at = Some(Timestamp::now());
        }
    }

    /// Record the finish time.
    ///
    /// No-op for `Instant` variants (their time is already known).
    pub fn finish(&mut self) {
        if let Self::Streamed { finished_at, .. } = self {
            *finished_at = Some(Timestamp::now());
        }
    }

    /// Return the recorded finish time, if any.
    ///
    /// `None` for `Instant` or when the stream has not yet finished.
    #[must_use]
    pub fn finished_at(&self) -> Option<Timestamp> {
        match self {
            Self::Streamed { finished_at, .. } => *finished_at,
            _ => None,
        }
    }

    /// Return the primary timestamp for this entry.
    ///
    /// For `Instant`, this is `at`. For `Streamed`, this is `dispatched_at`.
    #[must_use]
    pub fn at(&self) -> Timestamp {
        match self {
            Self::Instant { at } => *at,
            Self::Streamed { dispatched_at, .. } => *dispatched_at,
        }
    }

    /// Return the time-to-first-token duration, if available.
    ///
    /// For `Streamed` entries with a recorded `first_token_at`, this is the
    /// elapsed time between dispatch and the first token arriving.
    /// Returns `None` for `Instant` entries or when `first_token_at` has not
    /// been recorded yet.
    #[must_use]
    pub fn ttft(&self) -> Option<jiff::SignedDuration> {
        match self {
            Self::Instant { .. }
            | Self::Streamed {
                first_token_at: None,
                ..
            } => None,
            Self::Streamed {
                dispatched_at,
                first_token_at: Some(ft),
                ..
            } => {
                let span = ft.since(*dispatched_at).ok()?;
                let millis = span.total(jiff::Unit::Millisecond).ok()?;
                Some(jiff::SignedDuration::from_millis(millis.round() as i64))
            }
        }
    }

    /// Return the active stream duration, if available.
    ///
    /// For `Streamed` entries with both `first_token_at` and `finished_at`
    /// recorded, this is the elapsed time between the first token and
    /// completion — i.e. how long this entry actively streamed content,
    /// excluding the pre-first-token wait (TTFT).
    ///
    /// This definition works for both burst and streaming providers: a burst
    /// response (everything arriving at once) yields a near-zero duration, while
    /// a streaming response yields the real generation span.
    ///
    /// Returns `None` for `Instant` entries, when `first_token_at` or
    /// `finished_at` have not been recorded, or if timestamps are not
    /// monotonic (should not happen).
    #[must_use]
    pub fn total_duration(&self) -> Option<jiff::SignedDuration> {
        match self {
            Self::Instant { .. }
            | Self::Streamed {
                first_token_at: None,
                ..
            }
            | Self::Streamed {
                finished_at: None, ..
            } => None,
            Self::Streamed {
                first_token_at: Some(first),
                finished_at: Some(fin),
                ..
            } => {
                let span = fin.since(*first).ok()?;
                let millis = span.total(jiff::Unit::Millisecond).ok()?;
                Some(jiff::SignedDuration::from_millis(millis.round() as i64))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::match_wildcard_for_single_variants,
        clippy::panic,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn instant_entry_has_timing_at_creation() {
        // Given the current time.
        let before = Timestamp::now();

        // When creating an instant timing.
        let timing = EntryTiming::instant_now();

        // Then the timing is Instant with a valid at timestamp.
        let after = Timestamp::now();
        assert!(matches!(timing, EntryTiming::Instant { .. }));
        assert!(timing.at() >= before);
        assert!(timing.at() <= after);
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_begins_with_dispatched_at_only() {
        // Given a dispatched-at timestamp.
        let dispatched = Timestamp::now();

        // When creating a streamed timing.
        let timing = EntryTiming::streamed(dispatched);

        // Then dispatched_at is set and first_token_at/finished_at are None.
        assert_eq!(timing.at(), dispatched);
        match &timing {
            EntryTiming::Streamed {
                dispatched_at,
                first_token_at,
                finished_at,
            } => {
                assert_eq!(*dispatched_at, dispatched);
                assert!(first_token_at.is_none());
                assert!(finished_at.is_none());
            }
            _ => panic!("expected Streamed variant"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_gets_first_token_at_on_creation() {
        // Given a streamed timing with no first_token_at.
        let dispatched = Timestamp::now();
        let mut timing = EntryTiming::streamed(dispatched);

        // When recording the first token.
        let before = Timestamp::now();
        timing.set_first_token();
        let after = Timestamp::now();

        // Then first_token_at is Some.
        match &timing {
            EntryTiming::Streamed { first_token_at, .. } => {
                let ft = first_token_at.expect("first_token_at should be set");
                assert!(ft >= before);
                assert!(ft <= after);
            }
            _ => panic!("expected Streamed variant"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_gets_finished_at_on_stream_complete() {
        // Given a streamed timing with first_token set.
        let dispatched = Timestamp::now();
        let mut timing = EntryTiming::streamed(dispatched);
        timing.set_first_token();

        // When finishing.
        let before = Timestamp::now();
        timing.finish();
        let after = Timestamp::now();

        // Then finished_at is Some.
        match &timing {
            EntryTiming::Streamed { finished_at, .. } => {
                let fin = finished_at.expect("finished_at should be set");
                assert!(fin >= before);
                assert!(fin <= after);
            }
            _ => panic!("expected Streamed variant"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn instant_timing_serializes_to_json() {
        // Given an instant timing.
        let at = Timestamp::now();
        let timing = EntryTiming::Instant { at };

        // When serializing and deserializing.
        let json = serde_json::to_string(&timing).expect("serialize");
        let roundtripped: EntryTiming = serde_json::from_str(&json).expect("deserialize");

        // Then the value is preserved.
        assert_eq!(timing, roundtripped);
    }

    #[rstest::rstest]
    #[test]
    fn streamed_timing_serializes_to_json() {
        // Given a streamed timing with partial timestamps.
        let dispatched = Timestamp::now();
        let mut timing = EntryTiming::streamed(dispatched);
        timing.set_first_token();
        // finished_at stays None.

        // When serializing and deserializing.
        let json = serde_json::to_string(&timing).expect("serialize");
        let roundtripped: EntryTiming = serde_json::from_str(&json).expect("deserialize");

        // Then the value is preserved.
        assert_eq!(timing, roundtripped);

        // And finished_at is still None.
        match &roundtripped {
            EntryTiming::Streamed { finished_at, .. } => {
                assert!(finished_at.is_none());
            }
            _ => panic!("expected Streamed variant"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn legacy_plain_timestamp_parses_as_instant_via_fallback() {
        // Given a raw ISO timestamp string (pre-v17 data in the database).
        let legacy_ts = "2024-01-15T10:30:00Z";

        // When serde_json fails to parse it (it's not JSON), the fallback
        // path parses it as a Timestamp and wraps in Instant.
        let timing: EntryTiming = serde_json::from_str(legacy_ts).unwrap_or_else(|_| {
            legacy_ts.parse::<jiff::Timestamp>().map_or_else(
                |_| EntryTiming::instant_now(),
                |at| EntryTiming::Instant { at },
            )
        });

        // Then the result is Instant with the expected timestamp.
        match &timing {
            EntryTiming::Instant { at } => {
                let ts = at.to_string();
                assert!(
                    ts.starts_with("2024-01-15T10:30:00"),
                    "expected legacy timestamp, got {ts}"
                );
            }
            other => panic!("expected Instant, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn ttft_returns_none_for_instant() {
        // Given an instant timing.
        let timing = EntryTiming::instant_now();

        // When querying ttft.
        // Then it returns None.
        assert!(timing.ttft().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ttft_returns_none_when_first_token_not_recorded() {
        // Given a streamed timing with no first_token_at.
        let timing = EntryTiming::streamed(Timestamp::now());

        // When querying ttft.
        // Then it returns None.
        assert!(timing.ttft().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ttft_returns_duration_when_first_token_recorded() {
        // Given a streamed timing with a 2-second gap between dispatch and first token.
        let dispatched = Timestamp::now();
        let first_token = dispatched
            .checked_add(jiff::SignedDuration::from_secs(2))
            .expect("2s later");
        let timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: Some(first_token),
            finished_at: None,
        };

        // When querying ttft.
        let ttft = timing.ttft().expect("ttft should be present");

        // Then the duration is 2 seconds.
        assert_eq!(ttft.as_secs(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn total_duration_returns_none_for_instant() {
        // Given an instant timing.
        let timing = EntryTiming::instant_now();

        // When querying total_duration.
        // Then it returns None.
        assert!(timing.total_duration().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn total_duration_returns_none_when_finished_not_recorded() {
        // Given a streamed timing with no finished_at.
        let mut timing = EntryTiming::streamed(Timestamp::now());
        timing.set_first_token();

        // When querying total_duration.
        // Then it returns None.
        assert!(timing.total_duration().is_none());
    }
    #[rstest::rstest]
    #[test]
    fn total_duration_returns_none_when_first_token_not_recorded() {
        // Given a streamed timing with finished_at set but no first_token_at.
        let dispatched = Timestamp::now();
        let finished = dispatched
            .checked_add(jiff::SignedDuration::from_secs(5))
            .expect("5s later");
        let timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: None,
            finished_at: Some(finished),
        };

        // When querying total_duration.
        // Then it returns None (active span requires both timestamps).
        assert!(timing.total_duration().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn total_duration_returns_duration_when_finished_recorded() {
        // Given a streamed timing with first_token at 2s and finished at 15s
        let dispatched = Timestamp::now();
        let finished = dispatched
            .checked_add(jiff::SignedDuration::from_secs(15))
            .expect("15s later");
        let timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: Some(
                dispatched
                    .checked_add(jiff::SignedDuration::from_secs(2))
                    .expect("2s later"),
            ),
            finished_at: Some(finished),
        };

        // When querying total_duration.
        let dur = timing.total_duration().expect("duration should be present");

        // Then the active stream duration is 15 - 2 = 13 seconds (excludes TTFT).
        assert_eq!(dur.as_secs(), 13);
    }

    #[rstest::rstest]
    #[test]
    fn ttft_preserves_subsecond_precision() {
        // Given a streamed timing with a 450ms gap between dispatch and first token.
        let dispatched = "2024-01-15T10:30:00Z".parse::<Timestamp>().expect("valid");
        let first_token = dispatched
            .checked_add(jiff::SignedDuration::from_millis(450))
            .expect("450ms later");
        let timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: Some(first_token),
            finished_at: None,
        };

        // When querying ttft.
        let ttft = timing.ttft().expect("ttft should be present");

        // Then the duration retains the sub-second part (450ms, not truncated to 0).
        assert_eq!(ttft.as_secs(), 0);
        assert_eq!(ttft.subsec_millis(), 450);
    }

    #[rstest::rstest]
    #[test]
    fn total_duration_preserves_subsecond_precision() {
        // Given a streamed timing with first_token at 200ms and finished at 2.45s.
        let dispatched = "2024-01-15T10:30:00Z".parse::<Timestamp>().expect("valid");
        let first_token = dispatched
            .checked_add(jiff::SignedDuration::from_millis(200))
            .expect("200ms later");
        let finished = dispatched
            .checked_add(jiff::SignedDuration::from_millis(2_450))
            .expect("2.45s later");
        let timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: Some(first_token),
            finished_at: Some(finished),
        };

        // When querying total_duration.
        let dur = timing.total_duration().expect("duration should be present");

        // Then the active stream duration is 2.45s - 0.2s = 2.25s (excludes TTFT).
        assert_eq!(dur.as_secs(), 2);
        assert_eq!(dur.subsec_millis(), 250);
    }
}
