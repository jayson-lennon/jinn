//! Chat input intent validators.
//!
//! Validators for message submission and autocomplete confirmation.

use crate::common::app_state::AppState;
use wherror::Error;

/// Errors from validating a SubmitMessage intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum SubmitMessageError {
    /// The input buffer is empty.
    EmptyBuffer,
}

/// Validates the SubmitMessage intent.
///
/// Returns an error if autocomplete is active or the input buffer is empty.
///
/// # Errors
///
/// Returns an error if autocomplete is active or the input buffer is empty.
pub fn validate_submit_message(state: &AppState) -> Result<(), SubmitMessageError> {
    if state
        .active_session()
        .with_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || true)
    {
        return Err(SubmitMessageError::EmptyBuffer);
    }
    Ok(())
}

/// Errors from validating an AutocompleteConfirm intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum AutocompleteConfirmError {
    /// No autocomplete session is active.
    NotActive,
}

/// Validates the AutocompleteConfirm intent.
///
/// Returns an error if no autocomplete session is active.
///
/// # Errors
///
/// Returns an error if no autocomplete session is active.
pub fn validate_autocomplete_confirm(state: &AppState) -> Result<(), AutocompleteConfirmError> {
    if state
        .active_session()
        .with_input(|i| i.autocomplete().is_none(), || true)
    {
        return Err(AutocompleteConfirmError::NotActive);
    }
    Ok(())
}

/// Validates the NormalEscape intent.
///
/// Escape in Normal mode can always proceed.
pub fn validate_normal_escape(_state: &AppState) {}

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
    fn submit_message_succeeds_with_non_empty_buffer() {
        // Given a state with text in the input buffer.
        let mut state = AppState::default();
        state.update_active_input(|i| i.insert_grapheme_at_cursor('h'));

        // When validating submit message.
        let result = validate_submit_message(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn submit_message_fails_with_empty_buffer() {
        // Given a state with an empty input buffer.
        let state = AppState::default();

        // When validating submit message.
        let result = validate_submit_message(&state);

        // Then it returns EmptyBuffer error.
        assert!(matches!(result, Err(SubmitMessageError::EmptyBuffer)));
    }

    #[rstest::rstest]
    fn autocomplete_confirm_succeeds_when_active() {
        // Given a state with autocomplete active.
        let mut state = AppState::default();
        state.update_active_input(|i| {
            i.activate_autocomplete(
                0,
                crate::feat::chat_input::AutocompleteTrigger::Hash,
                vec![],
            );
        });

        // When validating autocomplete confirm.
        let result = validate_autocomplete_confirm(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn autocomplete_confirm_fails_when_not_active() {
        // Given a state with no autocomplete.
        let state = AppState::default();

        // When validating autocomplete confirm.
        let result = validate_autocomplete_confirm(&state);

        // Then it returns NotActive error.
        assert!(matches!(result, Err(AutocompleteConfirmError::NotActive)));
    }
}
