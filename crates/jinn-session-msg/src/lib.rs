//! Session crossing contracts.
//!
//! The EXPORT surface of the session family: the discriminant and
//! lifecycle events that cross the core bridge (trouper
//! topic). Kernel publishers (session actors) and slice consumers
//! (e.g. the discord bridge) both depend on this crate — the types
//! have exactly one home.
//!
//! The crate publishes shared session vocabulary: phase values and the
//! validated phase machine, plus lifecycle events consumed across slices.
//! Persistence commands remain owned by their implementation boundary.

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

pub mod phase_command;
pub mod phase_machine;
pub mod session_origin;
mod session_seed;

pub use phase_command::{DispatchKind, PhaseCommand, PhaseDecision};

/// The wall-clock moment carried by [`SessionPhaseChanged`] and
/// [`WorkStateChanged`], re-exported so a consumer publishing either event
/// does not need its own `jiff` dependency just to stamp it.
pub use jiff::Timestamp as PhaseEventAt;

pub use session_origin::SessionOrigin;
pub use session_seed::SessionSeed;

// ── phase discriminant ──────────────────────────────────────────────

/// The discriminant of a session's phase — used for event emission and
/// logging where the per-phase data is not needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseKind {
    /// Session is idle — no LLM request in flight.
    Idle,
    /// A message has been dispatched to the LLM but no tokens have
    /// arrived yet.
    Sending,
    /// LLM tokens are actively streaming into the session.
    Streaming,
}

impl std::str::FromStr for PhaseKind {
    type Err = PhaseKindParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "idle" => Ok(Self::Idle),
            "sending" => Ok(Self::Sending),
            "streaming" => Ok(Self::Streaming),
            _ => Err(PhaseKindParseError(s.to_owned())),
        }
    }
}

impl PhaseKind {
    /// Whether a session in this phase is working.
    ///
    /// The single definition of "working" in the codebase: a session is busy
    /// whenever its phase is not [`PhaseKind::Idle`]. The separate `is_busy`
    /// counter that used to disagree with this one (four writers, phase-adjacent
    /// readers) is gone — every liveness predicate reads the phase, through this
    /// method or a `matches!` on it. There are deliberately no carve-outs for a
    /// parent orchestrating subagents, an attendant composing a report, or a
    /// turn running its tool loop — those *are* the time the user is waiting.
    #[must_use]
    pub const fn is_working(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Error returned when a string does not match any [`PhaseKind`] variant.
#[derive(Debug, wherror::Error)]
#[error("unknown phase kind: {0}")]
pub struct PhaseKindParseError(String);

// ── session events (jinn bus → crossing topics) ─────────────────────

/// Session phase transitioned to a new state.
///
/// Emitted by the session actor whenever the session phase transitions
/// (e.g., Idle → Sending, Sending → Streaming, Streaming → Idle).
///
/// The QueueActor subscribes to this event to react to `Idle`
/// transitions and pop the turn dispatch queue.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's phase transitioned.")]
pub struct SessionPhaseChanged {
    /// The session whose phase changed.
    pub session_id: SessionId,
    /// The phase before the transition.
    pub old_phase: PhaseKind,
    /// The new phase after the transition.
    pub new_phase: PhaseKind,
    /// When the transition was observed.
    ///
    /// Carried on the event rather than left to a consumer's clock so every
    /// subscriber measures the same boundary: a recorder that timestamps on
    /// receipt bills the time its own mailbox took, which differs per
    /// subscriber.
    pub at: jiff::Timestamp,
}

/// A session started or stopped working.
///
/// Published alongside every [`SessionPhaseChanged`] by the writers that
/// mutate a phase. The working-time monitor folds these into wall-clock
/// intervals; it never reads a phase, so a session whose phase it never
/// observes still costs it nothing.
///
/// Distinct from [`SessionPhaseChanged`] because the phase event is a
/// no-op on equal phases while this one states the fact directly, and because
/// the monitor needs the moment rather than the phases around it.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session started or stopped working.")]
pub struct WorkStateChanged {
    /// The session whose working state changed.
    pub session_id: SessionId,
    /// Whether the session is now working.
    pub working: bool,
    /// When the change was observed.
    pub at: jiff::Timestamp,
}

/// A session's turn has ended, with the outcome read from its history.
///
/// Emitted by the session actor after the terminal entries are applied, so a
/// subscriber sees history that already discriminates the outcome. Unlike
/// [`SessionPhaseChanged`], which fires on every transition and is a no-op on
/// equal phases, this fires exactly once per dispatched turn — a tool-loop
/// continuation publishes nothing.
///
/// The outcome is derived from the session's last history entry, not from the
/// transport-level completion reason: every consumer agrees on the policy by
/// construction instead of re-deriving it.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's turn completed, with its outcome.")]
pub struct TurnCompleted {
    /// The session whose turn ended.
    pub session_id: SessionId,
    /// How the turn ended.
    pub outcome: TurnOutcome,
}

/// How a completed turn ended, as read from the session's history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// The turn ran to completion without an error entry.
    #[default]
    Succeeded,
    /// The last history entry is an error.
    Error,
    /// The turn was cancelled by the user.
    Canceled,
    /// A stream rule interrupted the turn and it resumed with the rule's
    /// body.
    ///
    /// **This is not a terminal outcome, despite arriving on
    /// [`TurnCompleted`].** The turn did not end — it was rewound and
    /// re-dispatched, and a fresh generation will follow. It is reported here
    /// rather than as its own event so a consumer can tell an intercepted
    /// generation from a cancelled turn with one subscription, but a consumer
    /// that *reacts to a turn ending* must exclude it. Use
    /// [`TurnOutcome::is_terminal`] rather than matching variants by hand; the
    /// bug this prevents is a watchdog disarming itself on an intercept and a
    /// turn counter being cleared mid-turn, so the condition it is watching for
    /// never accumulates.
    RuleIntercepted,
}

impl TurnOutcome {
    /// Whether this outcome ends the turn, and so must disarm anything
    /// monitoring it.
    ///
    /// The single place this policy lives. Every watchdog that tracks a turn
    /// asks this rather than re-deriving "which outcomes are terminal", because
    /// the two derivations are what let a watchdog disarm on an intercept.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::RuleIntercepted)
    }
}

/// Session archived in persistent storage.
/// Emitted by the session-store actor after marking a session as archived in
/// SQLite. Emitted before the session-closed event so consumers can distinguish
/// archived closes from empty-session closes.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session was archived in persistent storage.")]
pub struct SessionArchived {
    /// The session that was archived.
    pub session_id: SessionId,
}

/// Mark a session as having been interacted with by the user.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Mark a session as interacted by the user.")]
pub struct MarkSessionInteracted {
    /// The session the user interacted with.
    pub session_id: SessionId,
}

/// Emitted after a session records its first user interaction.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session recorded its first user interaction.")]
pub struct UserInteracted {
    /// The session that was interacted with.
    pub session_id: SessionId,
}

/// Re-dispatch a turn whose in-flight provider stream stalled.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Re-dispatch a turn whose stream stalled.")]
pub struct RetryStalledSession {
    /// The session whose turn has stalled.
    pub session_id: SessionId,
    /// The one-based restart attempt within the current stall lineage.
    pub attempt: u32,
    /// The restart budget enforced by the watchdog.
    pub max_restarts: u32,
}

/// Emitted after a session's close workflow has completed.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session close workflow completed.")]
pub struct SessionClosed {
    /// The session that was closed.
    pub session_id: SessionId,
}

/// Emitted immediately after a session is removed from the live session map.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session was removed from the live session map.")]
pub struct SessionRemoved {
    /// The session removed from the live map.
    pub session_id: SessionId,
    /// The removed session's persisted direct parent, captured before deletion.
    pub removed_parent: Option<SessionId>,
    /// Whether the removed session was the active one, captured before deletion.
    ///
    /// Carried rather than reconstructed: once the session is gone the active id
    /// already names whatever `remove_and_replace` moved it to, so a consumer
    /// cannot tell whether the user was reading this session when it left. That
    /// distinction decides whether the next session becomes active, and getting
    /// it wrong pulls the user out of a conversation they are still reading.
    pub was_active: bool,
}

/// Emitted when archiving a session did not complete, leaving it live.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's archive did not complete and the session remains live.")]
pub struct SessionArchiveFailed {
    /// The session whose archive did not complete.
    pub session_id: SessionId,
    /// Why the archive did not complete.
    pub error: String,
}

// ── wire contracts ──────────────────────────────────────────────────

impl jinn_slices::BusMessage for PhaseKind {}
impl jinn_slices::BusMessage for MarkSessionInteracted {}
impl jinn_slices::BusMessage for RetryStalledSession {}
impl jinn_slices::BusMessage for SessionClosed {}
impl jinn_slices::BusMessage for SessionRemoved {}
impl jinn_slices::BusMessage for SessionPhaseChanged {}
impl jinn_slices::BusMessage for WorkStateChanged {}
impl jinn_slices::BusMessage for TurnCompleted {}
impl jinn_slices::BusMessage for SessionArchived {}
impl jinn_slices::BusMessage for SessionArchiveFailed {}
impl jinn_slices::BusMessage for UserInteracted {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::MarkSessionInteracted;
    use super::PhaseKind;
    use super::RetryStalledSession;
    use super::SessionArchiveFailed;
    use super::SessionArchived;
    use super::SessionClosed;
    use super::SessionPhaseChanged;
    use super::SessionRemoved;
    use super::UserInteracted;
    use super::WorkStateChanged;
    use jiff::Timestamp;
    use jinn_core_types::SessionId;
    use std::str::FromStr;

    #[rstest::rstest]
    #[case("idle", PhaseKind::Idle)]
    #[case("SENDING", PhaseKind::Sending)]
    #[case("streaming", PhaseKind::Streaming)]
    fn phase_kind_parses_case_insensitively(#[case] raw: &str, #[case] expected: PhaseKind) {
        // Given a phase-kind string in any case.
        // When parsing.
        let parsed = PhaseKind::from_str(raw);
        // Then the variant matches.
        assert_eq!(parsed.expect("parses"), expected);
    }

    #[rstest::rstest]
    #[test]
    fn phase_kind_rejects_unknown_strings() {
        // Given a string that is no phase.
        // When parsing.
        let parsed = PhaseKind::from_str("paused");
        // Then parsing fails.
        assert!(parsed.is_err());
    }

    #[rstest::rstest]
    #[case(PhaseKind::Idle, false)]
    #[case(PhaseKind::Sending, true)]
    #[case(PhaseKind::Streaming, true)]
    fn only_idle_is_not_working(#[case] phase: PhaseKind, #[case] expected: bool) {
        // Given any phase.
        // When asking whether a session in it is working.
        let working = phase.is_working();
        // Then working is exactly "the phase is not idle".
        assert_eq!(working, expected);
    }

    #[rstest::rstest]
    #[test]
    fn phase_change_roundtrips_its_moment_through_json() {
        // Given a phase change stamped with a moment.
        let at = Timestamp::now();
        let event = SessionPhaseChanged {
            session_id: SessionId::new(),
            old_phase: PhaseKind::Idle,
            new_phase: PhaseKind::Sending,
            at,
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&event).unwrap();
        let round: SessionPhaseChanged = serde_json::from_str(&json).unwrap();

        // Then the moment survives, so a subscriber measures the boundary the
        // publisher observed rather than its own clock.
        assert_eq!(round.at, at);
    }

    #[rstest::rstest]
    #[test]
    fn work_state_change_roundtrips_through_json() {
        // Given a work-state change.
        let at = Timestamp::now();
        let event = WorkStateChanged {
            session_id: SessionId::new(),
            working: true,
            at,
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&event).unwrap();
        let round: WorkStateChanged = serde_json::from_str(&json).unwrap();

        // Then every field survives the wire.
        assert_eq!(round.session_id, event.session_id);
        assert!(round.working);
        assert_eq!(round.at, at);
    }

    #[rstest::rstest]
    #[test]
    fn session_events_roundtrip_through_json() {
        // Given the session events this crate owns.
        let id = jinn_core_types::SessionId::new();
        let events = (
            SessionPhaseChanged {
                session_id: id.clone(),
                old_phase: PhaseKind::Streaming,
                new_phase: PhaseKind::Idle,
                at: jiff::Timestamp::now(),
            },
            SessionArchived {
                session_id: id.clone(),
            },
            SessionArchiveFailed {
                session_id: id.clone(),
                error: "write failed".to_owned(),
            },
        );

        // When serializing and deserializing the tuple.
        let json = serde_json::to_string(&events).unwrap();
        let round: (SessionPhaseChanged, SessionArchived, SessionArchiveFailed) =
            serde_json::from_str(&json).unwrap();

        // Then every event survives with its fields intact.
        assert_eq!(round.0.new_phase, PhaseKind::Idle);
        assert_eq!(round.1.session_id, round.0.session_id);
        assert_eq!(round.2.session_id, round.0.session_id);
        assert_eq!(round.2.error, "write failed");
    }

    #[rstest::rstest]
    #[test]
    fn promoted_session_contracts_roundtrip_through_json() {
        // Given one of each promoted session command and event.
        let id = jinn_core_types::SessionId::new();
        let removed_parent = SessionId::new();
        let contracts = (
            MarkSessionInteracted {
                session_id: id.clone(),
            },
            UserInteracted {
                session_id: id.clone(),
            },
            RetryStalledSession {
                session_id: id.clone(),
                attempt: 2,
                max_restarts: 5,
            },
            SessionClosed {
                session_id: id.clone(),
            },
            SessionRemoved {
                session_id: id.clone(),
                removed_parent: Some(removed_parent.clone()),
                was_active: true,
            },
        );

        // When serializing and deserializing the wire tuple.
        let json = serde_json::to_string(&contracts).unwrap();
        let restored = serde_json::from_str::<(
            MarkSessionInteracted,
            UserInteracted,
            RetryStalledSession,
            SessionClosed,
            SessionRemoved,
        )>(&json)
        .unwrap();

        // Then every moved payload survives unchanged.
        assert_eq!(restored.0.session_id, id);
        assert_eq!(restored.1.session_id, id);
        assert_eq!(restored.2.session_id, id);
        assert_eq!(restored.2.attempt, 2);
        assert_eq!(restored.2.max_restarts, 5);
        assert_eq!(restored.3.session_id, id);
        assert_eq!(restored.4.session_id, id);
        assert_eq!(restored.4.removed_parent, Some(removed_parent));
        assert!(restored.4.was_active);
    }
}
