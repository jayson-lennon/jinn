//! Pruner accumulation threshold input intent handlers — enter, confirm, leave, and text editing.
//!
//! Like the rename-session popup, but numeric-only: only digit characters are
//! accepted. On confirm the parsed value is pushed to the `PreferencesActor`
//! via `UpdatePreferences` so it is persisted to `jinn.toml` and broadcast.

use crate::common::app_state::{AppState, FocusScope, PrunerAccumulationInputState};
use crate::common::line_input::LineInput;
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::protocol::IntentResult;

/// Opens the pruner accumulation threshold input popup.
///
/// Pushes `FocusScope::PrunerAccumulationInput` and seeds the input with the
/// current threshold from user preferences.
pub fn handle_enter(state: &mut AppState) -> IntentResult {
    let threshold = state
        .frontend
        .preferences
        .auto_prune
        .accumulation_threshold_tokens;
    let text = threshold.to_string();
    let cursor_pos = text.len();

    state.frontend.pruner_accumulation_input = PrunerAccumulationInputState {
        text: LineInput {
            input: text,
            cursor_pos,
        },
    };
    state
        .frontend
        .scope_push(FocusScope::PrunerAccumulationInput);
    IntentResult::empty()
}

/// Confirms the pruner accumulation threshold input.
///
/// Validates the input (non-empty, numeric), pops the scope, clears the input
/// state, and emits `UpdatePreferences` with the new threshold so the
/// `PreferencesActor` persists it and broadcasts `PreferencesUpdated`.
pub fn handle_confirm(state: &mut AppState) -> IntentResult {
    let raw = state
        .frontend
        .pruner_accumulation_input
        .text
        .input
        .trim()
        .to_owned();

    // Validate: non-empty, numeric.
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return IntentResult::empty();
    }

    // Parse is infallible given the digit-only check above, but guard anyway.
    let Ok(threshold) = raw.parse::<u32>() else {
        return IntentResult::empty();
    };

    // Pop scope and clear state.
    state.frontend.scope_pop();
    state.frontend.pruner_accumulation_input = PrunerAccumulationInputState::default();

    IntentResult::new_message(UpdatePreferences {
        updates: vec![PreferenceUpdate::SetAccumulationThreshold(threshold)],
    })
}

/// Cancels the pruner accumulation threshold input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_pop();
    state.frontend.pruner_accumulation_input = PrunerAccumulationInputState::default();
    IntentResult::empty()
}

/// Inserts a digit character at the cursor position.
///
/// Non-digits are rejected (the popup is numeric-only).
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    if ch.is_ascii_digit() {
        state
            .frontend
            .pruner_accumulation_input
            .text
            .insert_char(ch);
    }
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    state.frontend.pruner_accumulation_input.text.delete();
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .pruner_accumulation_input
        .text
        .delete_forward();
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    state.frontend.pruner_accumulation_input.text.cursor_left();
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    state.frontend.pruner_accumulation_input.text.cursor_right();
    IntentResult::empty()
}

/// Handles `PasteText` — bulk inserts pasted text at the cursor.
///
/// Only digit text is accepted; any non-digit text is rejected wholesale so
/// the popup never holds a non-numeric value.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        state.frontend.pruner_accumulation_input.text.paste(text);
    }
    IntentResult::empty()
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
    use crate::common::app_state::{AppState, FocusScope};
    use crate::feat::preferences_actor::protocol::command::UpdatePreferences;

    use super::*;

    /// State seeded with a threshold so `handle_enter` has a value to display.
    fn state_with_threshold(threshold: u32) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .auto_prune
            .accumulation_threshold_tokens = threshold;
        state
    }

    #[rstest::rstest]
    fn enter_pushes_pruner_accumulation_input_scope() {
        // Given default state.
        let mut state = state_with_threshold(10_000);

        // When handling OpenPrunerAccumulationInput.
        let result = handle_enter(&mut state);

        // Then PrunerAccumulationInput is the current scope.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::PrunerAccumulationInput
        ));
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn enter_seeds_input_with_current_threshold() {
        // Given state with a threshold of 7500.
        let mut state = state_with_threshold(7500);

        // When handling OpenPrunerAccumulationInput.
        let _result = handle_enter(&mut state);

        // Then the input is seeded with the threshold.
        assert_eq!(state.frontend.pruner_accumulation_input.text.input, "7500");
        // And the cursor is at the end.
        assert_eq!(state.frontend.pruner_accumulation_input.text.cursor_pos, 4);
    }

    #[rstest::rstest]
    fn confirm_emits_update_preferences_with_parsed_value() {
        // Given state in PrunerAccumulationInput scope with input "25000".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);
        state.frontend.pruner_accumulation_input.text.input = "25000".to_owned();

        // When handling PrunerAccumulationConfirm.
        let result = handle_confirm(&mut state);

        // Then exactly one UpdatePreferences message is emitted.
        assert_eq!(
            result.message_names,
            vec![std::any::type_name::<UpdatePreferences>()]
        );
        // And the scope is popped back.
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));
        // And input state is cleared.
        assert!(
            state
                .frontend
                .pruner_accumulation_input
                .text
                .input
                .is_empty()
        );
    }

    #[rstest::rstest]
    fn confirm_rejects_empty_input() {
        // Given state with empty input.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);

        // When handling PrunerAccumulationConfirm.
        let result = handle_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
        // And scope is NOT popped (user stays in popup).
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::PrunerAccumulationInput
        ));
    }

    #[rstest::rstest]
    fn confirm_rejects_non_numeric_input() {
        // Given state with non-numeric input "abc".
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);
        state.frontend.pruner_accumulation_input.text.input = "abc".to_owned();

        // When handling PrunerAccumulationConfirm.
        let result = handle_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
        // And scope is NOT popped.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::PrunerAccumulationInput
        ));
    }

    #[rstest::rstest]
    fn insert_char_accepts_digit() {
        // Given state with input "1".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pruner_accumulation_input.text.input = "1".to_owned();
        state.frontend.pruner_accumulation_input.text.cursor_pos = 1;

        // When inserting a digit '2'.
        let _result = handle_insert_char(&mut state, '2');

        // Then the input is "12".
        assert_eq!(state.frontend.pruner_accumulation_input.text.input, "12");
    }

    #[rstest::rstest]
    fn insert_char_rejects_non_digit() {
        // Given state with input "1".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pruner_accumulation_input.text.input = "1".to_owned();
        state.frontend.pruner_accumulation_input.text.cursor_pos = 1;

        // When inserting a non-digit 'a'.
        let _result = handle_insert_char(&mut state, 'a');

        // Then the input is unchanged.
        assert_eq!(state.frontend.pruner_accumulation_input.text.input, "1");
    }

    #[rstest::rstest]
    fn paste_rejects_non_digit_text() {
        // Given state with input "1".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pruner_accumulation_input.text.input = "1".to_owned();
        state.frontend.pruner_accumulation_input.text.cursor_pos = 1;

        // When pasting "abc".
        let _result = handle_paste(&mut state, "abc");

        // Then the input is unchanged.
        assert_eq!(state.frontend.pruner_accumulation_input.text.input, "1");
    }

    #[rstest::rstest]
    fn leave_discards_changes_and_pops_scope() {
        // Given state in PrunerAccumulationInput scope with input.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);
        state.frontend.pruner_accumulation_input.text.input = "999".to_owned();

        // When handling PrunerAccumulationLeave.
        let result = handle_leave(&mut state);

        // Then scope is popped back.
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));
        // And input state is cleared.
        assert!(
            state
                .frontend
                .pruner_accumulation_input
                .text
                .input
                .is_empty()
        );
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn delete_removes_digit_before_cursor() {
        // Given state with input "123" and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pruner_accumulation_input.text.input = "123".to_owned();
        state.frontend.pruner_accumulation_input.text.cursor_pos = 3;

        // When deleting.
        let _result = handle_delete(&mut state);

        // Then input is "12" and cursor moved back.
        assert_eq!(state.frontend.pruner_accumulation_input.text.input, "12");
        assert_eq!(state.frontend.pruner_accumulation_input.text.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn confirm_ignores_leading_and_trailing_whitespace() {
        // Given state with whitespace-padded numeric input.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);
        state.frontend.pruner_accumulation_input.text.input = "  25000  ".to_owned();

        // When handling PrunerAccumulationConfirm.
        let result = handle_confirm(&mut state);

        // Then exactly one UpdatePreferences message is emitted.
        assert_eq!(
            result.message_names,
            vec![std::any::type_name::<UpdatePreferences>()]
        );
        // And scope is popped back.
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));
    }
}
