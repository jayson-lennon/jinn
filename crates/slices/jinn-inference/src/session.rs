//! Per-session state machine for the LLM actor.
//!
//! Each active LLM conversation is tracked by a [`SessionData`] instance
//! that records the current state and stream data.

use jiff::Timestamp;

/// Per-session state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    /// No active streaming.
    Idle,
    /// Streaming tokens from the LLM.
    Streaming,
}

/// Per-session data tracked by the actor.
pub(crate) struct SessionData {
    /// Current state in the streaming lifecycle.
    state: SessionState,
    /// The concrete model ID handling the current/last request.
    /// Propagated from `SendToLlmProvider` to `StreamCompleted`.
    model_used: Option<String>,
    /// When the LLM request was dispatched. Stored here so cancellation
    /// events can carry the original dispatch time.
    dispatched_at: Option<Timestamp>,
}

impl SessionData {
    /// Creates a new [`SessionData`] in Idle state.
    pub(crate) fn new() -> Self {
        Self {
            state: SessionState::Idle,
            model_used: None,
            dispatched_at: None,
        }
    }

    /// Returns the current streaming lifecycle state.
    #[cfg(test)]
    pub(crate) fn state(&self) -> &SessionState {
        &self.state
    }

    /// Transitions to [`SessionState::Streaming`].
    pub(crate) fn begin_streaming(&mut self, dispatched_at: Timestamp) {
        self.state = SessionState::Streaming;
        self.dispatched_at = Some(dispatched_at);
    }

    /// Returns the dispatch timestamp, if the session has been dispatched.
    pub(crate) fn dispatched_at(&self) -> Option<Timestamp> {
        self.dispatched_at
    }

    /// Sets the concrete model ID for the current stream.
    pub(crate) fn set_model_used(&mut self, model: Option<String>) {
        self.model_used = model;
    }
}
