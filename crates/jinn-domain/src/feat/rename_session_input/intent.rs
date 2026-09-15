//! Rename session input intent handlers - enter, confirm, leave, and text editing.

use crate::common::app_state::{AppState, FocusScope, RenameSessionInputState};
use crate::feat::session_lifecycle::protocol::command::PersistSession;
use crate::feat::ui::sidebar::sessions::sorted_open_sessions;
use crate::protocol::IntentResult;

/// Opens the rename session input popup.
///
/// Pushes `FocusScope::RenameSessionInput` and seeds the input with the
/// currently selected session's title (or empty if "Untitled Session").
/// No-op if no session is selected in the sidebar.
pub fn handle_rename_session_enter(state: &mut AppState) -> IntentResult {
    let Some(index) = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
    else {
        return IntentResult::empty();
    };

    let sessions = sorted_open_sessions(state);
    let Some(entry) = sessions.get(index) else {
        return IntentResult::empty();
    };

    let title = state
        .session
        .get(&entry.id)
        .map(|s| s.title().unwrap_or("Untitled Session").to_owned())
        .unwrap_or_default();
    let cursor_pos = title.len();

    state.frontend.rename_session_input = RenameSessionInputState {
        text: crate::common::line_input::LineInput {
            input: title,
            cursor_pos,
        },
    };
    state.frontend.scope_push(FocusScope::RenameSessionInput);
    IntentResult::empty()
}

/// Confirms the rename session input.
///
/// Validates the input (non-empty, different from current title),
/// updates the session title in memory, pops the scope, and clears the input state.
pub fn handle_rename_session_confirm(state: &mut AppState) -> IntentResult {
    let rename_input = &state.frontend.rename_session_input;
    let text = rename_input.text.input.trim().to_owned();

    // Validate: non-empty.
    if text.is_empty() {
        return IntentResult::empty();
    }

    // Resolve the selected session.
    let Some(index) = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
    else {
        return IntentResult::empty();
    };
    let sessions = sorted_open_sessions(state);
    let Some(entry) = sessions.get(index) else {
        return IntentResult::empty();
    };
    let session_id = entry.id.clone();

    // Update the session title and mark as interacted.
    state.session_mut(&session_id).set_title(text);
    state.session_mut(&session_id).touch();
    state.session_mut(&session_id).mark_interacted();

    // Pop scope and clear state.
    state.frontend.scope_pop();
    state.frontend.rename_session_input = RenameSessionInputState::default();

    IntentResult::new_message(PersistSession { session_id })
}

/// Cancels the rename session input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_rename_session_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_pop();
    state.frontend.rename_session_input = RenameSessionInputState::default();
    IntentResult::empty()
}

/// Inserts a character at the cursor position.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    state.frontend.rename_session_input.text.insert_char(ch);
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    state.frontend.rename_session_input.text.delete();
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    state.frontend.rename_session_input.text.delete_forward();
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    state.frontend.rename_session_input.text.cursor_left();
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    state.frontend.rename_session_input.text.cursor_right();
    IntentResult::empty()
}

/// Handles `PasteText` - bulk inserts pasted text at the cursor.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    state.frontend.rename_session_input.text.paste(text);
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
    use crate::common::app_state::{AppState, FocusScope, RenameSessionInputState};
    use crate::feat::session::chat_session::ChatSessionState;
    use crate::protocol::ChatEntry;

    use super::*;

    fn state_with_sessions(count: usize) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        for i in 1..count {
            let session = ChatSessionState::new();
            let _id = session.session_id().clone();
            state.session.insert({
                let mut s = ChatSessionState::new();
                s.push_entry(ChatEntry::user(format!("message for session {i}")));
                s
            });
        }
        state
    }

    #[rstest::rstest]
    fn enter_pushes_rename_session_input_scope() {
        // Given a state with a selected session.
        let mut state = state_with_sessions(2);
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));

        // When handling SidebarRenameSession.
        let result = handle_rename_session_enter(&mut state);

        // Then RenameSessionInput is the current scope.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::RenameSessionInput
        ));
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn enter_seeds_input_with_current_title() {
        // Given a session with a title.
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .session_mut(&session_id)
            .set_title("My Session".to_owned());
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));

        // When handling SidebarRenameSession.
        let _result = handle_rename_session_enter(&mut state);

        // Then the input is seeded with the session title.
        assert_eq!(state.frontend.rename_session_input.text.input, "My Session");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 10);
    }

    #[rstest::rstest]
    fn enter_noop_when_no_selection() {
        // Given a state with no session selected.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::SidebarSessions);

        // When handling SidebarRenameSession.
        let result = handle_rename_session_enter(&mut state);

        // Then scope is unchanged.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::SidebarSessions
        ));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_updates_session_title() {
        // Given state in RenameSessionInput scope with input "New Title".
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "New Title".to_owned(),
                cursor_pos: 9,
            },
        };

        // When handling RenameSessionConfirm.
        let result = handle_rename_session_confirm(&mut state);

        // Then the session title is updated.
        assert_eq!(state.session_mut(&session_id).title(), Some("New Title"));
        // And scope is popped back.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::SidebarSessions
        ));
        // And input state is cleared.
        assert!(state.frontend.rename_session_input.text.input.is_empty());
        // And a PersistSession command is emitted.
        assert_eq!(
            result.messages.len(),
            1,
            "expected PersistSession message for the renamed session"
        );
    }

    #[rstest::rstest]
    fn confirm_rejects_empty_input() {
        // Given state with empty input.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: String::new(),
                cursor_pos: 0,
            },
        };

        // When handling RenameSessionConfirm.
        let result = handle_rename_session_confirm(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
        // And scope is NOT popped (user stays in popup).
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::RenameSessionInput
        ));
    }

    #[rstest::rstest]
    fn confirm_marks_interacted_and_persists_non_interacted_session() {
        // Given a fresh session that has never been interacted with.
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        assert!(
            !state
                .session
                .get(&session_id)
                .expect("session exists")
                .has_interacted(),
            "fresh session should not be interacted"
        );
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Fresh Title".to_owned(),
                cursor_pos: 11,
            },
        };

        // When handling RenameSessionConfirm.
        let result = handle_rename_session_confirm(&mut state);

        // Then the session is marked as interacted.
        assert!(
            state
                .session
                .get(&session_id)
                .expect("session exists")
                .has_interacted(),
            "session should be marked as interacted after rename"
        );
        // And a PersistSession command is emitted.
        assert_eq!(
            result.messages.len(),
            1,
            "expected PersistSession message for non-interacted session after rename"
        );
    }

    #[rstest::rstest]
    fn leave_discards_changes() {
        // Given state in RenameSessionInput scope.
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .session_mut(&session_id)
            .set_title("Original".to_owned());
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Changed".to_owned(),
                cursor_pos: 7,
            },
        };

        // When handling RenameSessionLeave.
        let result = handle_rename_session_leave(&mut state);

        // Then scope is popped back.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::SidebarSessions
        ));
        // And input state is cleared.
        assert!(state.frontend.rename_session_input.text.input.is_empty());
        // And session title is unchanged.
        assert_eq!(state.session_mut(&session_id).title(), Some("Original"));
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn insert_char_adds_character() {
        // Given state with input "Hello".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 5,
            },
        };

        // When inserting '!'.
        let _result = handle_insert_char(&mut state, '!');

        // Then the input is "Hello!".
        assert_eq!(state.frontend.rename_session_input.text.input, "Hello!");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 6);
    }

    #[rstest::rstest]
    fn delete_removes_grapheme_before_cursor() {
        // Given state with input "Hello" and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 5,
            },
        };

        // When deleting.
        let _result = handle_delete(&mut state);

        // Then input is "Hell" and cursor moved back.
        assert_eq!(state.frontend.rename_session_input.text.input, "Hell");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 4);
    }

    #[rstest::rstest]
    fn delete_forward_removes_grapheme_after_cursor() {
        // Given state with input "Hello" and cursor at position 1.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 1,
            },
        };

        // When forward deleting.
        let _result = handle_delete_forward(&mut state);

        // Then input is "Hllo" and cursor stays at 1.
        assert_eq!(state.frontend.rename_session_input.text.input, "Hllo");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn cursor_left_moves_back() {
        // Given state with input "Hi" and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hi".to_owned(),
                cursor_pos: 2,
            },
        };

        // When moving cursor left.
        let _result = handle_cursor_left(&mut state);

        // Then cursor moved to 1.
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn cursor_right_moves_forward() {
        // Given state with input "Hi" and cursor at start.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hi".to_owned(),
                cursor_pos: 0,
            },
        };

        // When moving cursor right.
        let _result = handle_cursor_right(&mut state);

        // Then cursor moved to 1.
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn handle_delete_noop_at_position_zero() {
        // Given state with cursor at position 0 (boundary: > vs >=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 0,
            },
        };

        // When deleting at position 0.
        let _result = handle_delete(&mut state);

        // Then nothing is deleted and cursor stays at 0.
        assert_eq!(state.frontend.rename_session_input.text.input, "Hello");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn handle_delete_forward_noop_at_end() {
        // Given state with cursor at end (boundary: < vs <=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 5,
            },
        };

        // When forward deleting at end.
        let _result = handle_delete_forward(&mut state);

        // Then nothing is deleted.
        assert_eq!(state.frontend.rename_session_input.text.input, "Hello");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 5);
    }

    #[rstest::rstest]
    fn handle_cursor_left_noop_at_position_zero() {
        // Given state with cursor at position 0 (boundary: > vs >=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 0,
            },
        };

        // When moving cursor left at position 0.
        let _result = handle_cursor_left(&mut state);

        // Then cursor stays at 0.
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn handle_cursor_right_noop_at_end() {
        // Given state with cursor at end (boundary: < vs <=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 5,
            },
        };

        // When moving cursor right at end.
        let _result = handle_cursor_right(&mut state);

        // Then cursor stays at end.
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 5);
    }

    #[rstest::rstest]
    fn handle_paste_inserts_text_and_advances_cursor() {
        // Given state with cursor in the middle.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 2,
            },
        };

        // When pasting "XY".
        let _result = handle_paste(&mut state, "XY");

        // Then text is inserted and cursor advances by text.len().
        assert_eq!(state.frontend.rename_session_input.text.input, "HeXYllo");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 4);
    }

    #[rstest::rstest]
    fn handle_paste_noop_when_empty() {
        // Given state.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "Hello".to_owned(),
                cursor_pos: 2,
            },
        };

        // When pasting empty text.
        let _result = handle_paste(&mut state, "");

        // Then nothing changes.
        assert_eq!(state.frontend.rename_session_input.text.input, "Hello");
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 2);
    }
}
