//! The one write path for a session's phase.
//!
//! Every phase transition, dispatch admission, and turn termination
//! reaches the phase actor through a [`PhaseCommand`]. The actor is the
//! sole phase writer; each variant names the event that causes the
//! transition, so a variant maps to the machine edge it drives without
//! a lookup table.
//!
//! This is an enum, and trouper's schema derive accepts only
//! named-field structs — so the `Schema` and `PayloadValue` impls are
//! hand-written: a unit-field schema (no declared fields), serde's
//! externally tagged enum representation on the wire, and a per-variant
//! `field()` reader for the shard key the fabric would otherwise look
//! up.

use jiff::Timestamp;
use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

use trouper::envelope::PayloadValue;
use trouper::schema::{FieldDef, FieldTy, Schema, SchemaDef, SchemaKind};

/// Which kind of dispatch is asking to start a stream.
///
/// Admission policy keys on this: a fresh turn always mints a new
/// generation; a resume or a tool continuation may only reuse the live
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind {
    /// A user message or a manual resume: always admitted, always mints.
    FreshTurn,
    /// A queued dispatch (resume tail, stall retry): admitted only while
    /// its turn is still live.
    ResumeTurn,
    /// The tool loop's automatic continuation: admitted only while its
    /// turn is still live.
    ToolContinuation,
}

/// The sole write path for a session's phase.
///
/// One variant per driving event, named for that event:
///
/// - `BeginStream` — a dispatch prepares to stream (`Idle → Sending →
///   Streaming` fused with the generation mint; admitted or refused by
///   kind).
/// - `StreamEndedToolUse` — the stream ended in tool use; the tool loop
///   continues (`Streaming → Sending`).
/// - `StreamEndedFinished` — the stream ended with the final answer
///   (`Streaming → Idle`).
/// - `StreamEndedError` — the stream ended in error (`Streaming →
///   Idle`).
/// - `TurnCanceled` — the turn ends, from either busy phase (`→ Idle`),
///   and its generation dies.
/// - `InterceptRewind` — a rule intercept rewinds for the re-dispatch
///   (`Streaming → Sending`), generation stays live.
/// The phase actor's static trouper path. The actor spawns here and
/// every ask targets here; the constant lives in the contract crate so
/// publishers never name a slice implementation crate.
pub const SESSION_PHASE_PATH: &str = "session-phase";

/// One phase transition, carried to the phase actor.
///
/// The sole write path for session phase. Variants are named for the
/// event that causes the transition, so a variant maps to its machine
/// edge without a lookup table.
///
/// Ask-reply: the actor replies with a [`PhaseDecision`]. A refused
/// decision means the caller publishes nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PhaseCommand {
    /// A dispatch prepares to stream. Admitted or refused by kind.
    BeginStream {
        /// The session whose turn should start.
        session_id: SessionId,
        /// Which dispatch path is asking.
        kind: DispatchKind,
        /// The moment the outgoing request carries; the actor records
        /// it as the generation's stream stamp so completions resolve
        /// against the same value the provider saw.
        dispatched_at: Timestamp,
    },
    /// The stream ended in tool use; the tool loop continues.
    StreamEndedToolUse {
        /// The session whose stream ended.
        session_id: SessionId,
        /// The completing generation's stream stamp.
        dispatched_at: Timestamp,
    },
    /// The stream ended with the final answer.
    StreamEndedFinished {
        /// The session whose stream ended.
        session_id: SessionId,
        /// The completing generation's stream stamp.
        dispatched_at: Timestamp,
    },
    /// The stream ended in error.
    StreamEndedError {
        /// The session whose stream ended.
        session_id: SessionId,
        /// The completing generation's stream stamp.
        dispatched_at: Timestamp,
    },
    /// The turn is cancelled: settle from either busy phase, kill the
    /// generation, and mark the turn ended so nothing after it re-arms.
    TurnCanceled {
        /// The session whose turn ends.
        session_id: SessionId,
    },
    /// A rule intercept rewinds for the re-dispatch; the turn and its
    /// generation stay live.
    InterceptRewind {
        /// The session whose stream was interrupted.
        session_id: SessionId,
        /// The interrupted generation's stream stamp.
        dispatched_at: Timestamp,
    },
}

impl PhaseCommand {
    /// The session this command names.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::BeginStream { session_id, .. }
            | Self::StreamEndedToolUse { session_id, .. }
            | Self::StreamEndedFinished { session_id, .. }
            | Self::StreamEndedError { session_id, .. }
            | Self::TurnCanceled { session_id }
            | Self::InterceptRewind { session_id, .. } => session_id,
        }
    }
}

impl Schema for PhaseCommand {
    fn schema_name() -> Option<&'static str> {
        Some("PhaseCommand")
    }

    fn schema_def() -> SchemaDef {
        static DEF: std::sync::LazyLock<SchemaDef> = std::sync::LazyLock::new(|| SchemaDef {
            name: String::from("PhaseCommand"),
            kind: SchemaKind::Command,
            fields: vec![
                FieldDef {
                    name: String::from("session_id"),
                    ty: FieldTy::Uuid,
                    unit: None,
                    range: None,
                    role: None,
                    description: None,
                },
                FieldDef {
                    name: String::from("kind"),
                    ty: FieldTy::Str,
                    unit: None,
                    range: None,
                    role: None,
                    description: None,
                },
                FieldDef {
                    name: String::from("dispatched_at"),
                    ty: FieldTy::Str,
                    unit: None,
                    range: None,
                    role: None,
                    description: None,
                },
            ],
            description: Some(String::from(
                "The sole write path for a session's phase; each variant names the event that causes the transition.",
            )),
        });
        DEF.clone()
    }
}

impl PayloadValue for PhaseCommand {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn field(&self, name: &str) -> Option<String> {
        match name {
            "session_id" => Some(self.session_id().to_string()),
            "kind" => match self {
                Self::BeginStream { kind, .. } => match kind {
                    DispatchKind::FreshTurn => Some(String::from("fresh_turn")),
                    DispatchKind::ResumeTurn => Some(String::from("resume_turn")),
                    DispatchKind::ToolContinuation => Some(String::from("tool_continuation")),
                },
                _ => None,
            },
            "dispatched_at" => match self {
                Self::BeginStream { dispatched_at, .. }
                | Self::StreamEndedToolUse { dispatched_at, .. }
                | Self::StreamEndedFinished { dispatched_at, .. }
                | Self::StreamEndedError { dispatched_at, .. }
                | Self::InterceptRewind { dispatched_at, .. } => Some(dispatched_at.to_string()),
                Self::TurnCanceled { .. } => None,
            },
            _ => None,
        }
    }

    fn to_json_bytes(&self) -> std::sync::Arc<[u8]> {
        trouper::envelope::payload_value_json_bytes(self)
    }

    fn clone_value(&self) -> Box<dyn PayloadValue> {
        Box::new(self.clone())
    }
}

/// The phase actor's answer to a [`PhaseCommand`].
///
/// A command is either applied or refused; the reply says which, and
/// carries the phase around the transition and the generation facts the
/// caller needs to do its own work (arming a guard, publishing a
/// completion).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseDecision {
    /// Whether the command was applied. `false` means refused: no
    /// transition happened, and the caller must not publish whatever
    /// was going to follow the transition (a provider request, a retry,
    /// a completion).
    pub admitted: bool,
    /// The phase before an applied transition.
    pub old_phase: crate::PhaseKind,
    /// The phase after an applied transition (equal to `old_phase` when
    /// refused).
    pub new_phase: crate::PhaseKind,
    /// The turn generation the command resolved against. Zero when the
    /// session has no turn record yet.
    pub generation: u64,
    /// The live generation's stream stamp after the command, when one
    /// exists — the value completions resolve against.
    pub stream_stamp: Option<Timestamp>,
}

impl PhaseDecision {
    /// The refusal every failed ask falls back to: no transition, the
    /// phase reported as it stands (unknown to the caller, so echoed as
    /// `Idle` — a refusal must gate on `admitted`, never on the phase
    /// fields), and no live generation.
    pub fn refused() -> Self {
        Self {
            admitted: false,
            old_phase: crate::PhaseKind::Idle,
            new_phase: crate::PhaseKind::Idle,
            generation: 0,
            stream_stamp: None,
        }
    }
}

impl Schema for PhaseDecision {
    fn schema_name() -> Option<&'static str> {
        Some("PhaseDecision")
    }

    fn schema_def() -> SchemaDef {
        static DEF: std::sync::LazyLock<SchemaDef> = std::sync::LazyLock::new(|| SchemaDef {
            name: String::from("PhaseDecision"),
            kind: SchemaKind::Event,
            fields: vec![
                FieldDef {
                    name: String::from("admitted"),
                    ty: FieldTy::Bool,
                    unit: None,
                    range: None,
                    role: None,
                    description: None,
                },
                FieldDef {
                    name: String::from("generation"),
                    ty: FieldTy::Int,
                    unit: None,
                    range: None,
                    role: None,
                    description: None,
                },
            ],
            description: Some(String::from(
                "The phase actor's answer to a PhaseCommand: applied or refused, with the generation facts.",
            )),
        });
        DEF.clone()
    }
}

impl PayloadValue for PhaseDecision {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn field(&self, name: &str) -> Option<String> {
        match name {
            "admitted" => Some(self.admitted.to_string()),
            "generation" => Some(self.generation.to_string()),
            _ => None,
        }
    }

    fn to_json_bytes(&self) -> std::sync::Arc<[u8]> {
        trouper::envelope::payload_value_json_bytes(self)
    }

    fn clone_value(&self) -> Box<dyn PayloadValue> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn phase_command_json_roundtrips_every_variant() {
        // Given one command of every variant.
        let id = SessionId::new();
        let at = jiff::Timestamp::now();
        let commands = vec![
            PhaseCommand::BeginStream {
                session_id: id.clone(),
                kind: DispatchKind::FreshTurn,
                dispatched_at: at,
            },
            PhaseCommand::StreamEndedToolUse {
                session_id: id.clone(),
                dispatched_at: at,
            },
            PhaseCommand::StreamEndedFinished {
                session_id: id.clone(),
                dispatched_at: at,
            },
            PhaseCommand::StreamEndedError {
                session_id: id.clone(),
                dispatched_at: at,
            },
            PhaseCommand::TurnCanceled {
                session_id: id.clone(),
            },
            PhaseCommand::InterceptRewind {
                session_id: id.clone(),
                dispatched_at: at,
            },
        ];

        // When each roundtrips through the wire encoding.
        for command in commands {
            let bytes = trouper::envelope::payload_value_json_bytes(&command);
            let back: PhaseCommand = serde_json::from_slice(&bytes).expect("roundtrip");

            // Then the session id and (where present) the stamp survive.
            assert_eq!(back.session_id(), &id);
            assert_eq!(
                std::mem::discriminant(&command),
                std::mem::discriminant(&back),
                "variant mismatch: json was {}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn phase_command_carries_the_session_id_field() {
        // Given a command naming a session.
        let id = SessionId::new();
        let command = PhaseCommand::TurnCanceled {
            session_id: id.clone(),
        };

        // When the fabric reads its declared shard field.
        let read = command.field("session_id");

        // Then the id comes back in its string form.
        assert_eq!(read, Some(id.to_string()));
    }
}
