//! Phase types for the session phase machine.
//!
//! [`Phase`] is a struct-per-variant enum where each variant carries only the
//! state that is *meaningful solely while that phase is live* — the indices of
//! the entries currently receiving stream tokens, and the soft-cancel flag.
//! Transitioning away from a variant drops that data automatically.
//!
//! State that must outlive a transition does **not** live here. Tool-call and
//! tool-result index tracking lives on [`SessionPhaseMachine`], beside
//! `tool_loop_disabled`, because a tool call's arguments stream in across the
//! provider burst rather than within one phase: a `Sending → Streaming` edge
//! driven by a prose token used to discard the registration of a call that was
//! already streaming, and every later delta of that call was refused.
//!
//! [`PhaseKind`] is the discriminant used for event emission and logging
//! where the per-phase data is not needed. It lives in `jinn-session-msg`
//! (the crossing-contract crate) and is re-exported here.

use serde::{Deserialize, Serialize};

pub use crate::PhaseKind;

/// No per-phase data needed for Idle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdlePhase;

/// Per-phase data for the Sending phase.
///
/// Carries nothing of its own: the in-flight tool batch's result tracking and
/// any tool calls whose arguments are streaming in both belong to the machine,
/// not to this phase. A tool result does not arrive while the model is
/// streaming — the stream ends in `ToolUse`, the phase becomes `Sending`, and
/// *then* the tools run — and tool-call arguments stream in across the whole
/// provider burst, which begins in this phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendingPhase;

/// Carries the state that is only meaningful while the LLM is actively
/// streaming tokens.
///
/// All fields are cleared when transitioning away from `Streaming`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamingPhase {
    /// Index into history for the entry currently receiving stream tokens.
    pub streaming_entry_index: Option<usize>,
    /// Index into history for the entry currently receiving thinking tokens.
    pub streaming_thinking_entry_index: Option<usize>,
}

/// The current session phase with per-phase state.
///
/// Each variant carries only the state that belongs to that phase alone;
/// anything a turn needs across a transition lives on
/// [`SessionPhaseMachine`](super::machine::SessionPhaseMachine). Transitioning
/// away from a variant drops its own data — no manual cleanup of streaming
/// indices or flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Phase {
    /// Session is idle - no LLM request in flight.
    Idle(IdlePhase),
    /// A message has been dispatched to the LLM but no tokens have arrived yet.
    Sending(SendingPhase),
    /// LLM tokens are actively streaming into the session.
    Streaming(StreamingPhase),
}

impl Phase {
    /// Returns the discriminant without the per-phase data.
    pub fn kind(&self) -> PhaseKind {
        match self {
            Self::Idle(_) => PhaseKind::Idle,
            Self::Sending(_) => PhaseKind::Sending,
            Self::Streaming(_) => PhaseKind::Streaming,
        }
    }

    /// Mutable access to inner `SendingPhase`, if this is `Sending`.
    pub fn as_sending_mut(&mut self) -> Option<&mut SendingPhase> {
        match self {
            Self::Sending(s) => Some(s),
            _ => None,
        }
    }
}

impl Default for Phase {
    fn default() -> Self {
        Self::Idle(IdlePhase)
    }
}
