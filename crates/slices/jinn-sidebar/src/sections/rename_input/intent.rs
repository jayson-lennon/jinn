//! Rename session input intent handlers - enter, confirm, leave, and text editing.

use jinn_kernel::common::app_state::AppState;
use jinn_sidebar_msg::sidebar_sections::RenameSessionInputState;

use jinn_kernel::protocol::IntentResult;
use jinn_session_store_msg::PersistSession;
use jinn_slices::ScopeSignal;
use jinn_slices::SliceScopeId;

/// The rename popup's dynamic scope (input-capturing).
#[must_use]
pub fn rename_scope() -> SliceScopeId {
    SliceScopeId::new("sidebar", "rename")
}

/// Opens the rename session input popup.
///
/// Seeds the input with the highlighted session's title (or empty if
/// "Untitled Session") and requests the rename scope. The highlighted
/// session resolves through the focused section's own cursor — sessions,
/// bots, or library — so the popup renames what the user is looking at.
/// No-op if no session is highlighted or it is not in memory (an archived
/// library member has no live title to edit until it is loaded).
pub fn handle_rename_session_enter(state: &mut AppState) -> IntentResult {
    let Some(session_id) = crate::sections::sessions::bot_actions::selected_section_session(state)
    else {
        return IntentResult::empty();
    };
    if !state.session.contains(&session_id) {
        return IntentResult::empty();
    }

    let title = state
        .session
        .get(&session_id)
        .map_or_default(|s| s.title().unwrap_or("Untitled Session").to_owned());
    let cursor_pos = title.len();

    state.frontend.update_sections(|s| {
        s.rename_input = RenameSessionInputState {
            text: jinn_slices::LineInput {
                input: title,
                cursor_pos,
            },
        };
    });
    IntentResult::empty().with_scope_signal(ScopeSignal::Push(rename_scope()))
}

/// Confirms the rename session input.
///
/// Validates the input, updates the session title in memory, clears the input
/// state, and requests a conditional pop of the rename scope.
pub fn handle_rename_session_confirm(state: &mut AppState) -> IntentResult {
    let text = state
        .frontend
        .with_sections(|s| s.rename_input.text.input.clone(), String::new)
        .trim()
        .to_owned();

    // Validate: non-empty.
    if text.is_empty() {
        return IntentResult::empty();
    }

    // Resolve the highlighted session — the same section-relative cursor the
    // enter handler resolved, so confirming renames the row the popup opened
    // from even if another section's cursor moved meanwhile.
    let Some(session_id) = crate::sections::sessions::bot_actions::selected_section_session(state)
    else {
        return IntentResult::empty();
    };
    if !state.session.contains(&session_id) {
        return IntentResult::empty();
    }

    // Update the session title and mark as interacted.
    state.session_mut(&session_id).set_title(text);
    state.session_mut(&session_id).touch();
    state.session_mut(&session_id).mark_interacted();

    // Clear local input state; the handler applies the requested scope pop.
    state
        .frontend
        .update_sections(|s| s.rename_input = RenameSessionInputState::default());

    IntentResult::new_message(PersistSession { session_id })
        .with_scope_signal(ScopeSignal::PopIf(rename_scope()))
}

/// Cancels the rename session input popup.
///
/// Discards the input state and requests a conditional pop of the rename
/// scope.
pub fn handle_rename_session_leave(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input = RenameSessionInputState::default());
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(rename_scope()))
}

/// Clears nonempty input while remaining open, or leaves when already empty.
pub fn handle_rename_session_clear_or_leave(state: &mut AppState) -> IntentResult {
    let had_text = !state
        .frontend
        .with_sections(|s| s.rename_input.text.input.is_empty(), || true);
    state
        .frontend
        .update_sections(|s| s.rename_input = RenameSessionInputState::default());
    if had_text {
        IntentResult::empty()
    } else {
        IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(rename_scope()))
    }
}

/// Inserts a character at the cursor position.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.insert_char(ch));
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.delete());
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.delete_forward());
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.cursor_left());
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.cursor_right());
    IntentResult::empty()
}

/// Handles `PasteText` - bulk inserts pasted text at the cursor.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    state
        .frontend
        .update_sections(|s| s.rename_input.text.paste(text));
    IntentResult::empty()
}

/// Inserts a character into the popup's in-progress text (cell-level).
pub fn insert_char(input: &mut RenameSessionInputState, ch: char) {
    input.text.insert_char(ch);
}

/// Deletes the grapheme before the cursor (cell-level).
pub fn delete(input: &mut RenameSessionInputState) {
    input.text.delete();
}

/// Deletes the grapheme at/after the cursor (cell-level).
pub fn delete_forward(input: &mut RenameSessionInputState) {
    input.text.delete_forward();
}

/// Moves the cursor one grapheme left (cell-level).
pub fn cursor_left(input: &mut RenameSessionInputState) {
    input.text.cursor_left();
}

/// Moves the cursor one grapheme right (cell-level).
pub fn cursor_right(input: &mut RenameSessionInputState) {
    input.text.cursor_right();
}

/// Moves the cursor to the start of the input (cell-level).
pub fn cursor_home(input: &mut RenameSessionInputState) {
    input.text.cursor_home();
}

/// Moves the cursor to the end of the input (cell-level).
pub fn cursor_end(input: &mut RenameSessionInputState) {
    input.text.cursor_end();
}

/// Bulk-inserts pasted text at the cursor (cell-level).
pub fn paste(input: &mut RenameSessionInputState, text: &str) {
    input.text.paste(text);
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
    use crate::sections::testing::state_with_bare_sessions;
    use jinn_kernel::common::app_state::AppState;
    use jinn_slices::FocusScope;

    /// Puts the cursor on the session drawn at `row`.
    fn cursor_to_row(state: &mut AppState, row: usize) {
        let id = crate::sections::sessions::state::sorted_open_sessions(state)
            .get(row)
            .map(|entry| entry.id.clone())
            .expect("the row under test exists in the list it indexes");
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(id));
    }

    use super::*;

    #[rstest::rstest]
    fn enter_requests_rename_session_input_scope() {
        // Given a state with a selected session.
        let mut state = state_with_bare_sessions(2);
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        cursor_to_row(&mut state, 0);

        // When handling SidebarRenameSession.
        let result = handle_rename_session_enter(&mut state);

        // Then the rename scope is requested while the sessions scope remains current.
        assert_eq!(result.scope_signal, Some(ScopeSignal::Push(rename_scope())));
        assert_eq!(
            state.frontend.scope(),
            jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope()
        );
    }

    #[rstest::rstest]
    fn enter_seeds_input_with_current_title() {
        // Given a session with a title.
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .session_mut(&session_id)
            .set_title("My Session".to_owned());
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        cursor_to_row(&mut state, 0);

        // When handling SidebarRenameSession.
        let _result = handle_rename_session_enter(&mut state);

        // Then the input is seeded with the session title.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "My Session"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            10
        );
    }

    #[rstest::rstest]
    fn enter_noop_when_no_selection() {
        // Given a state with no session selected.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

        // When handling SidebarRenameSession.
        let result = handle_rename_session_enter(&mut state);

        // Then scope is unchanged.
        assert_eq!(
            state.frontend.sidebar_section(),
            Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
        );
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn confirm_updates_session_title() {
        // Given state in RenameSessionInput scope with input "New Title".
        let mut state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));
        cursor_to_row(&mut state, 0);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "New Title".to_owned(),
                    cursor_pos: 9,
                },
            };
        });

        // When handling RenameSessionConfirm.
        let result = handle_rename_session_confirm(&mut state);

        // Then the session title is updated.
        assert_eq!(state.session_mut(&session_id).title(), Some("New Title"));
        // And the handler requests a conditional pop of rename.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::PopIf(rename_scope()))
        );
        // And input state is cleared.
        assert!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.is_empty(), || true)
        );
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
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));
        cursor_to_row(&mut state, 0);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: String::new(),
                    cursor_pos: 0,
                },
            };
        });

        // When handling RenameSessionConfirm.
        let result = handle_rename_session_confirm(&mut state);

        // Then no commands are emitted.
        assert_eq!(result.message_names, [] as [&str; 0]);
        // And scope is NOT popped (user stays in popup).
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::Dynamic(id) if id ==  rename_scope()
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
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));
        cursor_to_row(&mut state, 0);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Fresh Title".to_owned(),
                    cursor_pos: 11,
                },
            };
        });

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
    fn clear_or_leave_with_text_clears_and_stays_open() {
        // Given state in rename scope with a nonempty title.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));
        state.frontend.update_sections(|s| {
            s.rename_input.text.set("Changed".to_owned());
        });

        // When Ctrl-C is requested.
        let result = handle_rename_session_clear_or_leave(&mut state);

        // Then the input clears without leaving the popup.
        assert!(result.scope_signal.is_none());
        assert!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.is_empty(), || false)
        );
    }

    #[rstest::rstest]
    fn clear_or_leave_with_empty_text_clears_and_leaves() {
        // Given state in rename scope with empty input.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));

        // When Ctrl-C is requested.
        let result = handle_rename_session_clear_or_leave(&mut state);

        // Then the popup requests its own conditional pop.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::PopIf(rename_scope()))
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
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .scope_push(FocusScope::Dynamic(rename_scope()));
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Changed".to_owned(),
                    cursor_pos: 7,
                },
            };
        });

        // When handling RenameSessionLeave.
        let result = handle_rename_session_leave(&mut state);

        // Then the handler requests a conditional pop of rename.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::PopIf(rename_scope()))
        );
        // And input state is cleared.
        assert!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.is_empty(), || true)
        );
        // And session title is unchanged.
        assert_eq!(state.session_mut(&session_id).title(), Some("Original"));
        // And no commands are emitted.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn insert_char_adds_character() {
        // Given state with input "Hello".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 5,
                },
            };
        });

        // When inserting '!'.
        let _result = handle_insert_char(&mut state, '!');

        // Then the input is "Hello!".
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hello!"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            6
        );
    }

    #[rstest::rstest]
    fn delete_removes_grapheme_before_cursor() {
        // Given state with input "Hello" and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 5,
                },
            };
        });

        // When deleting.
        let _result = handle_delete(&mut state);

        // Then input is "Hell" and cursor moved back.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hell"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            4
        );
    }

    #[rstest::rstest]
    fn delete_forward_removes_grapheme_after_cursor() {
        // Given state with input "Hello" and cursor at position 1.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 1,
                },
            };
        });

        // When forward deleting.
        let _result = handle_delete_forward(&mut state);

        // Then input is "Hllo" and cursor stays at 1.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hllo"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            1
        );
    }

    #[rstest::rstest]
    fn cursor_left_moves_back() {
        // Given state with input "Hi" and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hi".to_owned(),
                    cursor_pos: 2,
                },
            };
        });

        // When moving cursor left.
        let _result = handle_cursor_left(&mut state);

        // Then cursor moved to 1.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            1
        );
    }

    #[rstest::rstest]
    fn cursor_right_moves_forward() {
        // Given state with input "Hi" and cursor at start.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hi".to_owned(),
                    cursor_pos: 0,
                },
            };
        });

        // When moving cursor right.
        let _result = handle_cursor_right(&mut state);

        // Then cursor moved to 1.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            1
        );
    }

    #[rstest::rstest]
    fn handle_delete_noop_at_position_zero() {
        // Given state with cursor at position 0 (boundary: > vs >=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 0,
                },
            };
        });

        // When deleting at position 0.
        let _result = handle_delete(&mut state);

        // Then nothing is deleted and cursor stays at 0.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hello"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            0
        );
    }

    #[rstest::rstest]
    fn handle_delete_forward_noop_at_end() {
        // Given state with cursor at end (boundary: < vs <=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 5,
                },
            };
        });

        // When forward deleting at end.
        let _result = handle_delete_forward(&mut state);

        // Then nothing is deleted.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hello"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            5
        );
    }

    #[rstest::rstest]
    fn handle_cursor_left_noop_at_position_zero() {
        // Given state with cursor at position 0 (boundary: > vs >=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 0,
                },
            };
        });

        // When moving cursor left at position 0.
        let _result = handle_cursor_left(&mut state);

        // Then cursor stays at 0.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            0
        );
    }

    #[rstest::rstest]
    fn handle_cursor_right_noop_at_end() {
        // Given state with cursor at end (boundary: < vs <=).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 5,
                },
            };
        });

        // When moving cursor right at end.
        let _result = handle_cursor_right(&mut state);

        // Then cursor stays at end.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            5
        );
    }

    #[rstest::rstest]
    fn handle_paste_inserts_text_and_advances_cursor() {
        // Given state with cursor in the middle.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 2,
                },
            };
        });

        // When pasting "XY".
        let _result = handle_paste(&mut state, "XY");

        // Then text is inserted and cursor advances by text.len().
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "HeXYllo"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            4
        );
    }

    #[rstest::rstest]
    fn handle_paste_noop_when_empty() {
        // Given state.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_slices::LineInput {
                    input: "Hello".to_owned(),
                    cursor_pos: 2,
                },
            };
        });

        // When pasting empty text.
        let _result = handle_paste(&mut state, "");

        // Then nothing changes.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.input.clone(), String::new),
            "Hello"
        );
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.rename_input.text.cursor_pos, || 0),
            2
        );
    }
}
