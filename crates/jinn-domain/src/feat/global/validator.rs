//! Global intent validators - quit, toggle which-key, and interrupt.

use crate::common::app_state::AppState;
use crate::feat::session::phase_machine::PhaseKind;
use wherror::Error;

/// Validates the Quit intent.
///
/// Quit can always proceed - it has no preconditions.
pub fn validate_quit(_state: &AppState) {}

/// Validates the ToggleWhichkey intent.
///
/// Toggling the which-key popup can always proceed.
pub fn validate_toggle_whichkey(_state: &AppState) {}

/// Errors from validating an Interrupt intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum InterruptError {
    /// Nothing to interrupt - buffer is empty and session is idle.
    NothingToInterrupt,
}

/// Validates the Interrupt intent.
///
/// Returns an error if the buffer is empty and the session is idle
/// (not streaming, not sending, not assembling).
///
/// # Errors
///
/// Returns an error if the buffer is empty and the session is idle.
pub fn validate_interrupt(state: &AppState) -> Result<(), InterruptError> {
    if state
        .active_session()
        .with_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || true)
        && matches!(state.active_session().phase(), PhaseKind::Idle)
    {
        return Err(InterruptError::NothingToInterrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn interrupt_succeeds_with_non_empty_buffer() {
        // Given a state with text in the input buffer and idle session.
        let mut state = AppState::default();
        state.update_active_input(|i| i.insert_grapheme_at_cursor('h'));

        // When validating interrupt.
        let result = validate_interrupt(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn interrupt_succeeds_with_active_stream() {
        // Given a state with empty buffer but an active stream.
        let mut state = AppState::default();
        state.active_session_mut().begin_streaming();

        // When validating interrupt.
        let result = validate_interrupt(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn interrupt_fails_with_empty_buffer_and_idle_session() {
        // Given a state with empty buffer and idle session.
        let state = AppState::default();

        // When validating interrupt.
        let result = validate_interrupt(&state);

        // Then it returns NothingToInterrupt error.
        assert!(matches!(result, Err(InterruptError::NothingToInterrupt)));
    }
}
