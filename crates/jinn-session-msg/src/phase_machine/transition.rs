use super::machine::{SessionPhaseMachine, TransitionError, TransitionOutcome};
use super::phase::{IdlePhase, Phase, PhaseKind, SendingPhase, StreamingPhase};

/// Transition methods for [`SessionPhaseMachine`].
///
/// Each method is named after the event that triggers the transition.
/// The machine validates the current phase and returns [`TransitionError`]
/// if the transition is not valid from the current state.
///
/// `cancel()` and `soft_cancel()` are on the machine itself because they
/// need direct access to private phase data.
pub trait PhaseTransitions {
    /// `Idle → Sending` - a message has been dispatched to the LLM.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Idle`.
    fn on_dispatch_message(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Sending → Streaming` - the first token has arrived from the LLM.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Sending`.
    fn on_first_token(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Streaming → Sending` - stream ended with tool use (continue tool loop).
    ///
    /// If `soft_cancel_requested` was set on the `StreamingPhase`, transitions
    /// to `Idle` instead of continuing the tool loop.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    fn on_stream_completed_tool_use(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Streaming → Idle` - stream ended normally (no tool use).
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    fn on_stream_completed_finished(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Streaming → Idle` - stream ended with an error.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    fn on_stream_completed_error(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Streaming → Idle` - stream was canceled by the user.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    fn on_stream_completed_canceled(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Streaming → Sending` - a stalled generation is being retried.
    ///
    /// Not a terminal transition: the turn continues, and the retried
    /// dispatch re-enters `Streaming` through the normal
    /// `Sending → Streaming` path. Dropping `StreamingPhase` discards the
    /// stalled generation's entry indices and the tool tracking maps are
    /// cleared, so the retry starts from a clean slate — exactly what a fresh
    /// dispatch expects.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Streaming`.
    fn on_retry_rewind(&mut self) -> Result<TransitionOutcome, TransitionError>;

    /// `Sending → Streaming` or `Sending → Idle`.
    ///
    /// If `tool_loop_disabled` is set on the `SendingPhase`, transitions to
    /// `Idle` (tool loop stops). Otherwise transitions to `Streaming`
    /// (tool loop continues with the next LLM request).
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if not in `Sending`.
    fn on_tool_batch_completed(&mut self) -> Result<TransitionOutcome, TransitionError>;
}

impl PhaseTransitions for SessionPhaseMachine {
    fn on_dispatch_message(&mut self) -> Result<TransitionOutcome, TransitionError> {
        self.transition(PhaseKind::Idle, Phase::Sending(SendingPhase))
    }

    fn on_first_token(&mut self) -> Result<TransitionOutcome, TransitionError> {
        self.transition(
            PhaseKind::Sending,
            Phase::Streaming(StreamingPhase::default()),
        )
    }

    fn on_stream_completed_tool_use(&mut self) -> Result<TransitionOutcome, TransitionError> {
        let soft_cancel = self
            .streaming_phase()
            .is_some_and(|sp| sp.soft_cancel_requested);

        let next = if soft_cancel {
            Phase::Idle(IdlePhase)
        } else {
            Phase::Sending(SendingPhase)
        };
        let outcome = self.transition(PhaseKind::Streaming, next)?;
        // The burst is over: every tool call in it has already been finalized
        // by `ToolCallReceived`, so its registration is spent. Clearing here is
        // what `StreamingPhase`'s drop used to do.
        self.clear_tool_tracking();
        Ok(outcome)
    }

    fn on_stream_completed_finished(&mut self) -> Result<TransitionOutcome, TransitionError> {
        self.end_turn_to_idle(PhaseKind::Streaming)
    }

    fn on_stream_completed_error(&mut self) -> Result<TransitionOutcome, TransitionError> {
        self.end_turn_to_idle(PhaseKind::Streaming)
    }

    fn on_stream_completed_canceled(&mut self) -> Result<TransitionOutcome, TransitionError> {
        self.end_turn_to_idle(PhaseKind::Streaming)
    }

    fn on_retry_rewind(&mut self) -> Result<TransitionOutcome, TransitionError> {
        let outcome = self.transition(PhaseKind::Streaming, Phase::Sending(SendingPhase))?;
        // The stalled generation owns every live registration; the retried
        // dispatch starts from a clean slate.
        self.clear_tool_tracking();
        Ok(outcome)
    }

    fn on_tool_batch_completed(&mut self) -> Result<TransitionOutcome, TransitionError> {
        let disabled = self.take_tool_loop_disabled();

        if disabled {
            return self.end_turn_to_idle(PhaseKind::Sending);
        }
        // Continuing the tool loop into a fresh burst: registrations from the
        // batch that just ran stay live, because the next dispatch can still
        // address them. Only a turn *end* clears.
        self.transition(
            PhaseKind::Sending,
            Phase::Streaming(StreamingPhase::default()),
        )
    }
}
