//! Sliding window input intent handlers — enter, confirm, leave, and text editing.

use unicode_segmentation::UnicodeSegmentation;

use crate::common::app_state::{AppState, FocusScope, SlidingWindowInputState};
use crate::feat::context::protocol::command::SwitchPromptStrategy;
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::protocol::{Command, IntentResult};

/// Minimum window size — values below this are rejected.
const MIN_WINDOW_SIZE: usize = 1;

/// Opens the sliding window input popup.
///
/// Pushes `FocusScope::SlidingWindowInput` and seeds the input with the
/// current session's sliding window size value.
pub fn handle_sliding_window_enter(state: &mut AppState) -> IntentResult {
    let current_size = state.active_session().profile().sliding_window_size;
    let input = current_size.to_string();
    let cursor_pos = input.len();

    state.frontend.sliding_window_input = SlidingWindowInputState {
        input,
        cursor_pos,
        error_message: None,
    };
    state
        .frontend
        .scope_stack
        .push(FocusScope::SlidingWindowInput);
    IntentResult::empty()
}

/// Confirms the sliding window input.
///
/// Validates the input (non-empty, valid usize, >= MIN_WINDOW_SIZE),
/// updates the active session's profile, emits `UpdatePreferences` to persist
/// the new default, pops the scope, and clears the input state.
pub fn handle_sliding_window_confirm(state: &mut AppState) -> IntentResult {
    let input_state = &state.frontend.sliding_window_input;
    let text = input_state.input.trim().to_owned();

    // Validate: non-empty.
    if text.is_empty() {
        return IntentResult::empty();
    }

    // Validate: valid usize.
    let Ok(size) = text.parse::<usize>() else {
        return IntentResult::empty();
    };

    // Validate: minimum size.
    if size < MIN_WINDOW_SIZE {
        return IntentResult::empty();
    }

    // Update session profile.
    state.active_session_mut().profile_mut().sliding_window_size = size;

    // Read current strategy and session ID before clearing state.
    let session_id = state.session.active_session_id().clone();
    let strategy_id = state.active_session().active_strategy().clone();

    // Pop scope and clear state.
    state.frontend.scope_stack.pop();
    state.frontend.sliding_window_input = SlidingWindowInputState::default();

    // Rebuild the strategy with the new size and persist to preferences.
    IntentResult::with_commands(vec![
        Command::SwitchPromptStrategy(SwitchPromptStrategy {
            session_id,
            strategy_id,
        }),
        Command::UpdatePreferences(UpdatePreferences {
            updates: vec![PreferenceUpdate::SetSlidingWindowSize(size)],
        }),
    ])
}

/// Cancels the sliding window input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_sliding_window_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_stack.pop();
    state.frontend.sliding_window_input = SlidingWindowInputState::default();
    IntentResult::empty()
}

/// Inserts a digit character at the cursor position.
///
/// Non-digit characters are silently ignored.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;
    if !ch.is_ascii_digit() {
        return IntentResult::empty();
    }
    let input = &mut state.frontend.sliding_window_input;
    input.input.insert(input.cursor_pos, ch);
    input.cursor_pos += ch.len_utf8();
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;
    if input.cursor_pos > 0 {
        let prev = input.input[..input.cursor_pos]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i);
        if let Some(prev_idx) = prev {
            input.input.drain(prev_idx..input.cursor_pos);
            input.cursor_pos = prev_idx;
        }
    }
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;
    if input.cursor_pos < input.input.len() {
        let next_end = input.input[input.cursor_pos..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(input.input.len(), |(i, _)| input.cursor_pos + i);
        input.input.drain(input.cursor_pos..next_end);
    }
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;
    if input.cursor_pos > 0 {
        let prev = input.input[..input.cursor_pos]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i);
        if let Some(prev_idx) = prev {
            input.cursor_pos = prev_idx;
        }
    }
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;
    if input.cursor_pos < input.input.len() {
        let next = input.input[input.cursor_pos..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| input.cursor_pos + i);
        match next {
            Some(next_idx) => input.cursor_pos = next_idx,
            None => input.cursor_pos = input.input.len(),
        }
    }
    IntentResult::empty()
}

/// Handles `PasteText` — bulk inserts pasted text if all digits, otherwise rejects.
///
/// Rejects paste if the text contains any non-digit characters, setting
/// `error_message`. Accepts all-digit text and inserts at cursor.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    let input = &mut state.frontend.sliding_window_input;
    input.error_message = None;

    if text.is_empty() {
        return IntentResult::empty();
    }

    if !text.chars().all(|c| c.is_ascii_digit()) {
        input.error_message = Some("Paste rejected: digits only".to_owned());
        return IntentResult::empty();
    }

    input.input.insert_str(input.cursor_pos, text);
    input.cursor_pos += text.len();
    IntentResult::empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use crate::common::app_state::{AppState, FocusScope, SlidingWindowInputState};
    use crate::feat::preferences_actor::protocol::command::PreferenceUpdate;
    use crate::protocol::Command;

    use super::*;

    #[rstest::rstest]
    fn enter_pushes_sliding_window_input_scope() {
        // Given default app state.
        let mut state = AppState::default();
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));

        // When handling SlidingWindowInputEnter.
        let result = handle_sliding_window_enter(&mut state);

        // Then SlidingWindowInput is the current scope.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::SlidingWindowInput
        ));
        // And no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn enter_seeds_input_with_current_window_size() {
        // Given a session with sliding_window_size 10.
        let mut state = AppState::default();
        state.active_session_mut().profile_mut().sliding_window_size = 10;

        // When handling SlidingWindowInputEnter.
        let _result = handle_sliding_window_enter(&mut state);

        // Then the input is seeded with "10".
        assert_eq!(state.frontend.sliding_window_input.input, "10");
        // And cursor is at the end.
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn confirm_updates_session_profile() {
        // Given state in SlidingWindowInput scope with input "20".
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::SlidingWindowInput);
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "20".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When handling SlidingWindowInputConfirm.
        let result = handle_sliding_window_confirm(&mut state);

        // Then the session profile is updated.
        assert_eq!(state.active_session().profile().sliding_window_size, 20);
        // And scope is popped back to Normal.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));
        // And input state is cleared.
        assert!(state.frontend.sliding_window_input.input.is_empty());
        // And two commands are emitted: SwitchPromptStrategy and UpdatePreferences.
        assert_eq!(result.commands.len(), 2);
        assert!(matches!(
            &result.commands[0],
            Command::SwitchPromptStrategy(_)
        ));
        assert!(matches!(
            &result.commands[1],
            Command::UpdatePreferences(cmd) if cmd.updates.len() == 1
                && matches!(&cmd.updates[0], PreferenceUpdate::SetSlidingWindowSize(20))
        ));
    }

    #[rstest::rstest]
    fn confirm_rejects_empty_input() {
        // Given state with empty input.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::SlidingWindowInput);
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: String::new(),
            cursor_pos: 0,
            error_message: None,
        };

        // When handling SlidingWindowInputConfirm.
        let result = handle_sliding_window_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
        // And scope is NOT popped (user stays in popup).
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::SlidingWindowInput
        ));
    }

    #[rstest::rstest]
    fn confirm_rejects_value_below_minimum() {
        // Given state with input "0" (below MIN_WINDOW_SIZE=1).
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::SlidingWindowInput);
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "0".to_owned(),
            cursor_pos: 1,
            error_message: None,
        };

        // When handling SlidingWindowInputConfirm.
        let result = handle_sliding_window_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
        // And scope is NOT popped.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::SlidingWindowInput
        ));
    }

    #[rstest::rstest]
    fn leave_discards_changes() {
        // Given state in SlidingWindowInput scope.
        let mut state = AppState::default();
        let original_size = state.active_session().profile().sliding_window_size;
        state
            .frontend
            .scope_stack
            .push(FocusScope::SlidingWindowInput);
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "99".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When handling SlidingWindowInputLeave.
        let result = handle_sliding_window_leave(&mut state);

        // Then scope is popped back to Normal.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));
        // And input state is cleared.
        assert!(state.frontend.sliding_window_input.input.is_empty());
        // And session window size is unchanged.
        assert_eq!(
            state.active_session().profile().sliding_window_size,
            original_size
        );
        // And no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn insert_char_accepts_digit() {
        // Given state in SlidingWindowInput scope.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "1".to_owned(),
            cursor_pos: 1,
            error_message: None,
        };

        // When inserting '0'.
        let _result = handle_insert_char(&mut state, '0');

        // Then the input is "10".
        assert_eq!(state.frontend.sliding_window_input.input, "10");
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn insert_char_rejects_non_digit() {
        // Given state in SlidingWindowInput scope.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "1".to_owned(),
            cursor_pos: 1,
            error_message: None,
        };

        // When inserting 'a' (non-digit).
        let _result = handle_insert_char(&mut state, 'a');

        // Then the input is unchanged.
        assert_eq!(state.frontend.sliding_window_input.input, "1");
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn delete_removes_grapheme_before_cursor() {
        // Given state with input "10" and cursor at end.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "10".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When deleting.
        let _result = handle_delete(&mut state);

        // Then input is "1" and cursor moved back.
        assert_eq!(state.frontend.sliding_window_input.input, "1");
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn delete_forward_removes_grapheme_after_cursor() {
        // Given state with input "10" and cursor at position 0.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "10".to_owned(),
            cursor_pos: 0,
            error_message: None,
        };

        // When forward deleting.
        let _result = handle_delete_forward(&mut state);

        // Then input is "0" and cursor stays at 0.
        assert_eq!(state.frontend.sliding_window_input.input, "0");
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn cursor_left_moves_back() {
        // Given state with input "10" and cursor at end.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "10".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When moving cursor left.
        let _result = handle_cursor_left(&mut state);

        // Then cursor moved to 1.
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn cursor_right_moves_forward() {
        // Given state with input "10" and cursor at start.
        let mut state = AppState::default();
        state.frontend.sliding_window_input = SlidingWindowInputState {
            input: "10".to_owned(),
            cursor_pos: 0,
            error_message: None,
        };

        // When moving cursor right.
        let _result = handle_cursor_right(&mut state);

        // Then cursor moved to 1.
        assert_eq!(state.frontend.sliding_window_input.cursor_pos, 1);
    }
}
