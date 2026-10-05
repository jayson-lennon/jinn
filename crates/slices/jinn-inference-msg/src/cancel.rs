//! Turn cancellation — the one command that ends a turn.
//!
//! [`CancelTurn`] replaced a pair of commands that each performed a different
//! subset of "stop the stream, settle the phase, report the end, drop the
//! queued dispatch". Eleven sites performed those steps separately, and a
//! *partial* cancel left a session wedged in a busy phase with a watchdog
//! still armed.
//!
//! It is a schema broadcast, so both actors that must act on a cancel act on
//! the same message: the inference actor stops the stream, and the session
//! actor reports the turn's end. Nothing else is duplicated.

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// Why a turn is being cancelled.
///
/// Decides one thing: whether the dispatch queued behind this cancel dies
/// with it. Nothing else. Work that is local to whoever asked for the cancel —
/// draining queued messages into the draft input, cancelling busy background
/// operations — stays at that caller, because only the caller knows whether a
/// person asked for it, and the synchronous Escape path cannot wait for a bus
/// round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelCause {
    /// The turn ends; whatever dispatches next runs normally.
    ///
    /// A person's Escape, a subagent teardown, an attendant rerun, a tool-call
    /// or stall watchdog giving up. Each has its own local work at its call
    /// site, and none of them should swallow the next thing the user does.
    #[default]
    Turn,
    /// The turn ends and the dispatch queued behind it is dropped too.
    ///
    /// The stream-rule watchdog's trip, and nothing else. The resume is
    /// already in flight when the watchdog counts the interrupt, and it
    /// arrives as a user-originated send — the only origin that lifts the
    /// interrupt's cancel tombstone. Without dropping it the resume always
    /// survives, the count resets on every trip, and the model loops forever
    /// with a cancel every four interrupts.
    TurnAndQueuedDispatch,
}

impl CancelCause {
    /// Whether the dispatch queued behind this cancel is dropped with it.
    pub fn latches_queued_dispatch(self) -> bool {
        matches!(self, Self::TurnAndQueuedDispatch)
    }
}

/// End a session's turn.
///
/// One command, one settle path. The inference actor tears the provider stream
/// down — arms the cancel tombstone, drops pending tool batches, aborts the
/// task — and publishes nothing. The session actor publishes exactly one
/// `StreamCompleted(Canceled)` if the session is not already idle, and the
/// existing completion handler settles the phase.
///
/// Two actors publishing a completion for one cancel is what produced the
/// double-settle, so exactly one of them does.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "End a session's turn, optionally dropping its queued dispatch.")]
pub struct CancelTurn {
    /// The session whose turn should be ended.
    pub session_id: SessionId,
    /// Whether the dispatch queued behind this cancel dies with it.
    pub cause: CancelCause,
}

impl jinn_slices::BusMessage for CancelTurn {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn a_turn_cancel_leaves_the_queued_dispatch_alone() {
        // Given the cause of an ordinary turn cancel.
        let cause = CancelCause::Turn;

        // When reading whether it latches the queued dispatch.
        let latched = cause.latches_queued_dispatch();

        // Then it does not.
        assert!(!latched);
    }

    #[rstest::rstest]
    #[test]
    fn a_watchdog_cancel_latches_the_queued_dispatch() {
        // Given the cause of a stream-rule watchdog's cancel.
        let cause = CancelCause::TurnAndQueuedDispatch;

        // When reading whether it latches the queued dispatch.
        let latched = cause.latches_queued_dispatch();

        // Then it does.
        assert!(latched);
    }

    #[rstest::rstest]
    #[test]
    fn schemas_are_declared_for_the_cancel_command() {
        // Given the cancel command.
        // When reading its schema id.
        // Then it has a schema (compile-time proof of the impl).
        let _ = <CancelTurn as trouper::schema::Schema>::schema_id();
    }
}
