//! Token budget input intent handlers — enter, confirm, leave, and text editing.

use unicode_segmentation::UnicodeSegmentation;

use crate::common::app_state::{AppState, FocusScope, TokenBudgetInputState};
use crate::feat::context::protocol::command::SwitchPromptStrategy;
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::protocol::{Command, IntentResult};

/// Minimum token budget — values below this are rejected.
const MIN_TOKEN_BUDGET: usize = 1_000;

/// Opens the token budget input popup.
///
/// Pushes `FocusScope::TokenBudgetInput` and seeds the input with the
/// current session's token budget value.
pub fn handle_token_budget_enter(state: &mut AppState) -> IntentResult {
    let current_budget = state.active_session().profile().token_budget;
    let input = current_budget.to_string();
    let cursor_pos = input.len();

    state.frontend.token_budget_input = TokenBudgetInputState {
        input,
        cursor_pos,
        error_message: None,
    };
    state
        .frontend
        .scope_stack
        .push(FocusScope::TokenBudgetInput);
    IntentResult::empty()
}

/// Confirms the token budget input.
///
/// Validates the input (non-empty, valid usize, >= MIN_TOKEN_BUDGET),
/// updates the active session's profile, emits `UpdatePreferences` to persist
/// the new default, pops the scope, and clears the input state.
pub fn handle_token_budget_confirm(state: &mut AppState) -> IntentResult {
    let budget_input = &state.frontend.token_budget_input;
    let text = budget_input.input.trim().to_owned();

    // Validate: non-empty.
    if text.is_empty() {
        return IntentResult::empty();
    }

    // Validate: valid usize.
    let Ok(budget) = text.parse::<usize>() else {
        return IntentResult::empty();
    };

    // Validate: minimum budget.
    if budget < MIN_TOKEN_BUDGET {
        return IntentResult::empty();
    }

    // Update session profile.
    state.active_session_mut().profile_mut().token_budget = budget;

    // Read current strategy and session ID before clearing state.
    let session_id = state.session.active_session_id().clone();
    let strategy_id = state.active_session().active_strategy().clone();

    // Pop scope and clear state.
    state.frontend.scope_stack.pop();
    state.frontend.token_budget_input = TokenBudgetInputState::default();

    // Rebuild the strategy with the new budget and persist to preferences.
    IntentResult::with_commands(vec![
        Command::SwitchPromptStrategy(SwitchPromptStrategy {
            session_id,
            strategy_id,
        }),
        Command::UpdatePreferences(UpdatePreferences {
            updates: vec![PreferenceUpdate::SetTokenBudget(budget)],
        }),
    ])
}

/// Cancels the token budget input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_token_budget_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_stack.pop();
    state.frontend.token_budget_input = TokenBudgetInputState::default();
    IntentResult::empty()
}

/// Inserts a digit character at the cursor position.
///
/// Non-digit characters are silently ignored.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    let input = &mut state.frontend.token_budget_input;
    input.error_message = None;
    if !ch.is_ascii_digit() {
        return IntentResult::empty();
    }
    let input = &mut state.frontend.token_budget_input;
    input.input.insert(input.cursor_pos, ch);
    input.cursor_pos += ch.len_utf8();
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    let input = &mut state.frontend.token_budget_input;
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
    let input = &mut state.frontend.token_budget_input;
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
    let input = &mut state.frontend.token_budget_input;
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
    let input = &mut state.frontend.token_budget_input;
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
    let input = &mut state.frontend.token_budget_input;
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
    use crate::common::app_state::{AppState, FocusScope, TokenBudgetInputState};
    use crate::feat::preferences_actor::protocol::command::PreferenceUpdate;
    use crate::protocol::Command;

    use super::*;

    #[rstest::rstest]
    fn enter_pushes_token_budget_input_scope() {
        // Given default app state.
        let mut state = AppState::default();
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));

        // When handling TokenBudgetInputEnter.
        let result = handle_token_budget_enter(&mut state);

        // Then TokenBudgetInput is the current scope.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::TokenBudgetInput
        ));
        // And no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn enter_seeds_input_with_current_budget() {
        // Given a session with token budget 200000.
        let mut state = AppState::default();
        state.active_session_mut().profile_mut().token_budget = 200_000;

        // When handling TokenBudgetInputEnter.
        let _result = handle_token_budget_enter(&mut state);

        // Then the input is seeded with "200000".
        assert_eq!(state.frontend.token_budget_input.input, "200000");
        // And cursor is at the end.
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 6);
    }

    #[rstest::rstest]
    fn confirm_updates_session_profile() {
        // Given state in TokenBudgetInput scope with input "999999".
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "999999".to_owned(),
            cursor_pos: 6,
            error_message: None,
        };

        // When handling TokenBudgetInputConfirm.
        let result = handle_token_budget_confirm(&mut state);

        // Then the session profile is updated.
        assert_eq!(state.active_session().profile().token_budget, 999_999);
        // And scope is popped back to Normal.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));
        // And input state is cleared.
        assert!(state.frontend.token_budget_input.input.is_empty());
        // And two commands are emitted: SwitchPromptStrategy and UpdatePreferences.
        assert_eq!(result.commands.len(), 2);
        assert!(matches!(
            &result.commands[0],
            Command::SwitchPromptStrategy(_)
        ));
        assert!(matches!(
            &result.commands[1],
            Command::UpdatePreferences(cmd) if cmd.updates.len() == 1
                && matches!(&cmd.updates[0], PreferenceUpdate::SetTokenBudget(999_999))
        ));
    }

    #[rstest::rstest]
    fn confirm_rejects_empty_input() {
        // Given state with empty input.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: String::new(),
            cursor_pos: 0,
            error_message: None,
        };

        // When handling TokenBudgetInputConfirm.
        let result = handle_token_budget_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
        // And scope is NOT popped (user stays in popup).
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::TokenBudgetInput
        ));
    }

    #[rstest::rstest]
    fn confirm_rejects_budget_below_minimum() {
        // Given state with input "500" (below MIN_TOKEN_BUDGET=1000).
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "500".to_owned(),
            cursor_pos: 3,
            error_message: None,
        };

        // When handling TokenBudgetInputConfirm.
        let result = handle_token_budget_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
        // And scope is NOT popped.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::TokenBudgetInput
        ));
    }

    #[rstest::rstest]
    fn leave_discards_changes() {
        // Given state in TokenBudgetInput scope.
        let mut state = AppState::default();
        let original_budget = state.active_session().profile().token_budget;
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "500000".to_owned(),
            cursor_pos: 6,
            error_message: None,
        };

        // When handling TokenBudgetInputLeave.
        let result = handle_token_budget_leave(&mut state);

        // Then scope is popped back to Normal.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Normal
        ));
        // And input state is cleared.
        assert!(state.frontend.token_budget_input.input.is_empty());
        // And session budget is unchanged.
        assert_eq!(
            state.active_session().profile().token_budget,
            original_budget
        );
        // And no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn insert_char_accepts_digit() {
        // Given state in TokenBudgetInput scope.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "15".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When inserting '0'.
        let _result = handle_insert_char(&mut state, '0');

        // Then the input is "150".
        assert_eq!(state.frontend.token_budget_input.input, "150");
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 3);
    }

    #[rstest::rstest]
    fn insert_char_rejects_non_digit() {
        // Given state in TokenBudgetInput scope.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "15".to_owned(),
            cursor_pos: 2,
            error_message: None,
        };

        // When inserting 'a' (non-digit).
        let _result = handle_insert_char(&mut state, 'a');

        // Then the input is unchanged.
        assert_eq!(state.frontend.token_budget_input.input, "15");
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn delete_removes_grapheme_before_cursor() {
        // Given state with input "150" and cursor at end.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "150".to_owned(),
            cursor_pos: 3,
            error_message: None,
        };

        // When deleting.
        let _result = handle_delete(&mut state);

        // Then input is "15" and cursor moved back.
        assert_eq!(state.frontend.token_budget_input.input, "15");
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn delete_forward_removes_grapheme_after_cursor() {
        // Given state with input "150" and cursor at position 1.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "150".to_owned(),
            cursor_pos: 1,
            error_message: None,
        };

        // When forward deleting.
        let _result = handle_delete_forward(&mut state);

        // Then input is "10" and cursor stays at 1.
        assert_eq!(state.frontend.token_budget_input.input, "10");
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn cursor_left_moves_back() {
        // Given state with input "150" and cursor at end.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "150".to_owned(),
            cursor_pos: 3,
            error_message: None,
        };

        // When moving cursor left.
        let _result = handle_cursor_left(&mut state);

        // Then cursor moved to 2.
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn cursor_right_moves_forward() {
        // Given state with input "150" and cursor at start.
        let mut state = AppState::default();
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "150".to_owned(),
            cursor_pos: 0,
            error_message: None,
        };

        // When moving cursor right.
        let _result = handle_cursor_right(&mut state);

        // Then cursor moved to 1.
        assert_eq!(state.frontend.token_budget_input.cursor_pos, 1);
    }
}
