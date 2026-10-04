//! Validated phase transition machine for a chat session.
//!
//! [`SessionPhaseMachine`] owns the current [`Phase`] and enforces that all
//! transitions are valid. Each transition method is named after the triggering
//! event and returns [`Result<TransitionOutcome, TransitionError>`].
//!
//! # Transition graph
//!
//! ```text
//! Idle ──on_dispatch_message()──► Sending ──on_first_token()──► Streaming
//!   ▲                               │                              │
//!   │                               │     on_stream_completed_*()  │
//!   │                               │◄─────────────────────────────┤
//!   │                               │                              │
//!   │         on_tool_batch_completed()      on_retry_rewind()      │
//!   │                               │◄─────────────────────────────┘
//!   │                               │
//!   │                               │   (if tool_loop_disabled)
//!   └───────────────────────────────┘
//!
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::phase::{IdlePhase, Phase, PhaseKind, SendingPhase, StreamingPhase};

/// Result of a successful phase transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionOutcome {
    /// The phase before the transition.
    pub old_phase: PhaseKind,
    /// The phase after the transition.
    pub new_phase: PhaseKind,
}

/// Result of a successful `cancel()` - includes the old streaming phase data
/// so the caller can force-exclude dangling tool calls and drain the queue.
#[derive(Debug)]
pub struct CancelOutcome {
    /// The transition outcome.
    pub outcome: TransitionOutcome,
    /// The old `StreamingPhase` data, consumed by the caller for cleanup.
    pub old_streaming: StreamingPhase,
}

/// Error returned when a transition is not valid from the current phase.
///
/// Callers should attach contextual information via `.attach()` to explain
/// why the transition was attempted.
#[derive(Debug, wherror::Error)]
#[error("invalid transition from {from:?}")]
pub struct TransitionError {
    /// The phase the machine was in when the invalid transition was attempted.
    pub from: PhaseKind,
}

/// Validated phase transition machine for a chat session.
///
/// Owns the current [`Phase`] and enforces that all transitions are valid.
/// Invalid transitions return [`TransitionError`] rather than panicking or
/// silently no-oping.
///
/// Construct with [`SessionPhaseMachine::new`] (starts in `Idle`).
/// Call transition methods to advance the phase. Use accessor methods
/// (`streaming_phase`, `sending_phase_mut`, etc.) to read or modify
/// per-phase data without transitioning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionPhaseMachine {
    /// The current phase.
    phase: Phase,
    /// When `true`, `on_tool_batch_completed` transitions to `Idle`
    /// instead of continuing the tool loop. Set by judge verdict tools
    /// (`task_complete`, `task_incomplete`) during `Streaming` or `Sending`.
    /// Machine-level flag that survives phase transitions.
    /// Self-clearing on read.
    tool_loop_disabled: bool,
    /// Maps stream tool-call index to history index for in-progress tool
    /// calls whose arguments are still arriving.
    ///
    /// Machine-level, for the same reason `tool_loop_disabled` is: a tool
    /// call's arguments stream in across the whole provider burst, which spans
    /// both busy phases. When this map lived on `SendingPhase` and
    /// `StreamingPhase` as two independent copies, every transition built a
    /// fresh empty one for the destination variant and silently discarded a
    /// registration that was still live — after which every later delta of
    /// that call was refused. Cleared at the same points the turn ends.
    streaming_tool_call_indices: HashMap<usize, usize>,
    /// Maps `tool_call_id` to history index for the in-flight tool batch's
    /// `Pending` result entries.
    ///
    /// Machine-level for the same reason: a tool result arrives in `Sending`
    /// for the ordinary case (the stream ended in `ToolUse` before the batch
    /// ran) and in `Streaming` only when a batch overlaps a live stream, so a
    /// phase-local copy lost whichever registrations did not share its phase.
    streaming_tool_result_indices: HashMap<String, usize>,
}

impl SessionPhaseMachine {
    /// Create a new machine in the `Idle` phase.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only access to the current phase.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The discriminant of the current phase.
    pub fn kind(&self) -> PhaseKind {
        self.phase.kind()
    }

    // ── Tool-loop suppression ────────────────────────────────────────

    /// Disable the tool loop for this session's current turn.
    ///
    /// After the current tool batch completes, `on_tool_batch_completed`
    /// will transition to `Idle` instead of continuing the tool loop.
    /// Machine-level flag that survives phase transitions.
    /// The flag is consumed (cleared) by [`take_tool_loop_disabled`](Self::take_tool_loop_disabled).
    pub fn set_tool_loop_disabled(&mut self) {
        self.tool_loop_disabled = true;
    }

    /// Take the tool-loop-disabled flag, clearing it.
    ///
    /// Returns `true` if the tool loop was disabled, and clears the flag.
    pub fn take_tool_loop_disabled(&mut self) -> bool {
        std::mem::take(&mut self.tool_loop_disabled)
    }

    /// Returns `true` if the tool loop is disabled, without clearing.
    pub fn is_tool_loop_disabled(&self) -> bool {
        self.tool_loop_disabled
    }

    /// `Streaming → Idle` or `Sending → Idle` - hard cancel, returns old streaming data.
    ///
    /// Accepts cancel from either `Streaming` or `Sending` phase. When cancelled
    /// from `Sending` (e.g., during tool execution), the returned `old_streaming`
    /// is `StreamingPhase::default()` since there is no streaming data to preserve.
    /// When cancelled from `Streaming`, the old streaming data is preserved in
    /// the result so the caller can force-exclude dangling tool calls and drain
    /// the queue to the input buffer.
    ///
    /// Clears the tool-call and tool-result maps: a cancelled turn is over, so
    /// no registration from it may outlive it.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming` or `Sending`.
    pub fn cancel(&mut self) -> Result<CancelOutcome, TransitionError> {
        let old = self.phase.kind();
        match old {
            PhaseKind::Streaming | PhaseKind::Sending => {}
            PhaseKind::Idle => return Err(TransitionError { from: old }),
        }
        self.streaming_tool_call_indices.clear();
        self.streaming_tool_result_indices.clear();
        let old_phase = std::mem::replace(&mut self.phase, Phase::Idle(IdlePhase));
        let old_streaming = match old_phase {
            Phase::Streaming(sp) => sp,
            _ => StreamingPhase::default(),
        };
        Ok(CancelOutcome {
            outcome: TransitionOutcome {
                old_phase: old,
                new_phase: PhaseKind::Idle,
            },
            old_streaming,
        })
    }

    /// Set soft cancel flag on the current `Streaming` phase.
    ///
    /// Does NOT transition - the flag is checked at the next stream-completion
    /// boundary (`on_stream_completed_tool_use` or `on_stream_completed_finished`).
    /// At that point, the transition goes to `Idle` instead of `Sending`.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    pub fn soft_cancel(&mut self) -> Result<(), TransitionError> {
        self.validate(PhaseKind::Streaming)?;
        if let Phase::Streaming(ref mut streaming) = self.phase {
            streaming.soft_cancel_requested = true;
        }
        Ok(())
    }

    // ── Phase data accessors ────────────────────────────────────────────

    /// Read-only access to `StreamingPhase` data, if currently streaming.
    pub fn streaming_phase(&self) -> Option<&StreamingPhase> {
        match &self.phase {
            Phase::Streaming(s) => Some(s),
            _ => None,
        }
    }

    /// Mutable access to `StreamingPhase` data, if currently streaming.
    pub fn streaming_phase_mut(&mut self) -> Option<&mut StreamingPhase> {
        match &mut self.phase {
            Phase::Streaming(s) => Some(s),
            _ => None,
        }
    }

    // ── Streaming state accessors ─────────────────────────────────────────

    /// The streaming assistant entry index, if streaming.
    pub fn streaming_entry_index(&self) -> Option<usize> {
        let sp = self.streaming_phase()?;
        sp.streaming_entry_index
    }

    /// Set the streaming assistant entry index. No-op if not streaming.
    pub fn set_streaming_entry_index(&mut self, index: usize) {
        if let Some(sp) = self.streaming_phase_mut() {
            sp.streaming_entry_index = Some(index);
        }
    }

    /// The streaming thinking entry index, if streaming.
    pub fn streaming_thinking_entry_index(&self) -> Option<usize> {
        let sp = self.streaming_phase()?;
        sp.streaming_thinking_entry_index
    }

    /// Set the streaming thinking entry index. No-op if not streaming.
    pub fn set_streaming_thinking_entry_index(&mut self, index: usize) {
        if let Some(sp) = self.streaming_phase_mut() {
            sp.streaming_thinking_entry_index = Some(index);
        }
    }

    /// Read-only access to the tool-call tracking map.
    ///
    /// Phase-agnostic by design: a tool call's arguments stream in across the
    /// whole provider burst, which spans both busy phases, so there is no phase
    /// for this map to be "the" map of. Same read as
    /// [`Self::active_tool_call_indices`].
    pub fn streaming_tool_call_indices(&self) -> &HashMap<usize, usize> {
        &self.streaming_tool_call_indices
    }

    /// Mutable access to the tool-call tracking map.
    ///
    /// Same write as [`Self::active_tool_call_indices_mut`]. Always available:
    /// the map outlives every phase, so there is nothing to gate on. Callers
    /// that must refuse a turn with no work in flight should test
    /// [`Self::kind`] instead.
    pub fn streaming_tool_call_indices_mut(&mut self) -> &mut HashMap<usize, usize> {
        &mut self.streaming_tool_call_indices
    }

    /// Read-only access to the tool-call tracking map.
    ///
    /// Tool-call arguments stream in during both busy phases: a dispatch leaves
    /// the session `Sending`, and the `Sending → Streaming` edge is driven by a
    /// prose token that can arrive *between* two argument deltas of the same
    /// call. Gating this map on one phase dropped the registration of any call
    /// whose arguments outlived the edge, and every later delta was refused.
    ///
    /// Returns empty when no turn is in flight, because the map is cleared when
    /// the turn ends.
    pub fn active_tool_call_indices(&self) -> &HashMap<usize, usize> {
        &self.streaming_tool_call_indices
    }

    /// Mutable access to the tool-call tracking map. Always available.
    pub fn active_tool_call_indices_mut(&mut self) -> &mut HashMap<usize, usize> {
        &mut self.streaming_tool_call_indices
    }

    /// Read-only access to the tool-result tracking map.
    ///
    /// Same read as [`Self::active_tool_result_indices`]. Returns empty when no
    /// turn is in flight.
    pub fn streaming_tool_result_indices(&self) -> &HashMap<String, usize> {
        &self.streaming_tool_result_indices
    }

    /// Mutable access to the tool-result tracking map. Always available.
    pub fn streaming_tool_result_indices_mut(&mut self) -> &mut HashMap<String, usize> {
        &mut self.streaming_tool_result_indices
    }

    /// Read-only access to the tool-result tracking map.
    ///
    /// A tool result arrives in `Sending` for the ordinary case (the stream
    /// ended in `ToolUse` before the batch ran) and in `Streaming` only when a
    /// batch overlaps a live stream, so a phase-local copy lost whichever
    /// registrations did not share its phase.
    ///
    /// Returns empty when no turn is in flight.
    pub fn active_tool_result_indices(&self) -> &HashMap<String, usize> {
        &self.streaming_tool_result_indices
    }

    /// Mutable access to the tool-result tracking map. Always available.
    pub fn active_tool_result_indices_mut(&mut self) -> &mut HashMap<String, usize> {
        &mut self.streaming_tool_result_indices
    }

    /// Shift all streaming indices >= `inserted_at` by +1.
    ///
    /// Called after `insert_entry_at` to keep indices valid. No-op in `Idle`.
    ///
    /// Tool-call and tool-result indices are shifted wherever they live: a tool
    /// call's arguments stream in during `Sending` as often as during
    /// `Streaming`, so shifting only one of them would let an index drift onto
    /// the wrong history entry.
    pub fn shift_streaming_indices_for_insert_at(&mut self, inserted_at: usize) {
        // Two separate borrows: one mutable borrow per map, never overlapping.
        for value in self.streaming_tool_call_indices.values_mut() {
            if *value >= inserted_at {
                *value += 1;
            }
        }
        for value in self.streaming_tool_result_indices.values_mut() {
            if *value >= inserted_at {
                *value += 1;
            }
        }
        let Some(sp) = self.streaming_phase_mut() else {
            return;
        };
        if let Some(ref mut i) = sp.streaming_entry_index
            && *i >= inserted_at
        {
            *i += 1;
        }
        if let Some(ref mut i) = sp.streaming_thinking_entry_index
            && *i >= inserted_at
        {
            *i += 1;
        }
    }

    /// Shift every live streaming index above `removed_at` down by one.
    ///
    /// The mirror of [`Self::shift_streaming_indices_for_insert_at`], called
    /// after a history entry is removed. Without it an index outlives the slot
    /// it named and the next delta is applied to an unrelated entry — silently,
    /// because the index is still in range.
    ///
    /// An index landing exactly on `removed_at` is the entry that was removed;
    /// it is dropped rather than shifted, so no index ever names a position one
    /// past the end. No-op in `Idle`.
    pub fn shift_streaming_indices_after_remove_at(&mut self, removed_at: usize) {
        for value in self.streaming_tool_call_indices.values_mut() {
            *value = shift_removed_index(*value, removed_at);
        }
        for value in self.streaming_tool_result_indices.values_mut() {
            *value = shift_removed_index(*value, removed_at);
        }
        let Some(sp) = self.streaming_phase_mut() else {
            return;
        };
        sp.streaming_entry_index = sp
            .streaming_entry_index
            .map(|i| shift_removed_index(i, removed_at));
        sp.streaming_thinking_entry_index = sp
            .streaming_thinking_entry_index
            .map(|i| shift_removed_index(i, removed_at));
    }

    /// Clear all streaming indices without leaving the current busy phase.
    ///
    /// Zeros the assistant entry, thinking entry, tool-call, and tool-result
    /// index tracking. The stall-retry path calls this after taking the
    /// partial entries out of context, so the retried stream's first token
    /// creates fresh entries. The tool-call and tool-result maps are cleared
    /// even in `Idle`, where the entry indices have nothing to clear.
    pub fn clear_streaming_indices(&mut self) {
        if let Some(sp) = self.streaming_phase_mut() {
            sp.streaming_entry_index = None;
            sp.streaming_thinking_entry_index = None;
        }
        self.streaming_tool_call_indices.clear();
        self.streaming_tool_result_indices.clear();
    }

    /// Whether the machine is tracking a tool call at the given history index.
    pub fn is_tool_call_at_history_index(&self, history_index: usize) -> bool {
        self.streaming_tool_call_indices()
            .values()
            .any(|&v| v == history_index)
    }

    /// Read-only access to `SendingPhase` data, if currently sending.
    pub fn sending_phase(&self) -> Option<&SendingPhase> {
        match &self.phase {
            Phase::Sending(s) => Some(s),
            _ => None,
        }
    }

    /// Mutable access to `SendingPhase` data, if currently sending.
    pub fn sending_phase_mut(&mut self) -> Option<&mut SendingPhase> {
        match &mut self.phase {
            Phase::Sending(s) => Some(s),
            _ => None,
        }
    }

    // ── Internal helpers ────────────────────────────────────────────────

    /// Drop every tool-call and tool-result registration.
    ///
    /// Called by the transitions that end a turn, now that the maps no longer
    /// live on a phase struct whose drop did this implicitly.
    pub(crate) fn clear_tool_tracking(&mut self) {
        self.streaming_tool_call_indices.clear();
        self.streaming_tool_result_indices.clear();
    }

    /// Validate the current phase, then swap to `next`.
    ///
    /// Returns [`TransitionOutcome`] recording the before/after phases.
    pub(crate) fn transition(
        &mut self,
        expected: PhaseKind,
        next: Phase,
    ) -> Result<TransitionOutcome, TransitionError> {
        let old = self.validate(expected)?;
        let new_phase = next.kind();
        self.phase = next;
        Ok(TransitionOutcome {
            old_phase: old,
            new_phase,
        })
    }

    /// Validate that the current phase matches the expected kind.
    pub(crate) fn validate(&self, expected: PhaseKind) -> Result<PhaseKind, TransitionError> {
        let actual = self.phase.kind();
        if actual == expected {
            Ok(actual)
        } else {
            Err(TransitionError { from: actual })
        }
    }

    /// End the turn from `expected`: drop every registration and go `Idle`.
    ///
    /// The turn is over, so the tool-call and tool-result maps are cleared
    /// before the swap. Previously this happened implicitly, as a side effect
    /// of dropping the outgoing phase struct.
    pub(crate) fn end_turn_to_idle(
        &mut self,
        expected: PhaseKind,
    ) -> Result<TransitionOutcome, TransitionError> {
        self.validate(expected)?;
        self.clear_tool_tracking();
        let old = self.phase.kind();
        self.phase = Phase::Idle(IdlePhase);
        Ok(TransitionOutcome {
            old_phase: old,
            new_phase: PhaseKind::Idle,
        })
    }
}

/// Where a stored history index points after the entry at `removed_at` is
/// deleted.
///
/// An index *at* the removal site named the entry that just went away, so it
/// becomes `usize::MAX` — a sentinel no entry occupies, and out of range for
/// every `history.get(..)` that reads it. An index above it shifts down one.
/// An index below it is untouched.
///
/// A removed live index leaves a stale `usize::MAX` in the map rather than a
/// missing key. That is deliberate: `streaming_tool_call_ids` filters through
/// `history.get(..)`, so a dangling key is already skipped by every read, and
/// leaving the key preserves the call's identity for the next delta rather
/// than making the map look as if the call had never begun.
fn shift_removed_index(value: usize, removed_at: usize) -> usize {
    match value.cmp(&removed_at) {
        std::cmp::Ordering::Less => value,
        std::cmp::Ordering::Equal => usize::MAX,
        std::cmp::Ordering::Greater => value - 1,
    }
}
