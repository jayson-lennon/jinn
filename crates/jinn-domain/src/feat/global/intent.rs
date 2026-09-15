//! Global intent handlers - quit, toggle which-key, and interrupt.

use crate::common::app_state::AppState;
use crate::feat::chat_input::ChatInputBoxState;
use crate::feat::provider::protocol::command::CancelStream;
use crate::protocol::SessionId;
use crate::protocol::{Intent, IntentResult};

use super::validator;

/// Handles the Quit intent.
///
/// Validates and latches the quit request on the frontend state.
pub fn handle_quit(state: &mut AppState) -> IntentResult {
    validator::validate_quit(state);
    state.frontend.set_quit(true);
    IntentResult::empty()
}

/// Handles the ToggleWhichkey intent.
///
/// Validates and sets the `toggle_whichkey` TUI signal.
pub fn handle_toggle_whichkey(state: &mut AppState) -> IntentResult {
    validator::validate_toggle_whichkey(state);
    state
        .frontend
        .update_scope(|s| s.signals.toggle_whichkey = true);
    IntentResult::empty()
}

/// Handles the `ToggleAuditPopup` intent.
///
/// Flips the global `audit_popup_visible` flag on `FrontendState`. Always
/// succeeds (no validator). The popup is rendered by the TUI layer when the
/// flag is `true`.
pub fn handle_toggle_audit_popup(state: &mut AppState) -> IntentResult {
    state.frontend.audit_popup_visible = !state.frontend.audit_popup_visible;
    IntentResult::empty()
}

/// Handles the Interrupt intent.
///
/// When `target` is `None`, clears the input buffer.
/// When `target` is `Some(id)`, cancels the targeted session's stream
/// (for headless/scripted use).
pub fn handle_interrupt(state: &mut AppState, target: Option<&SessionId>) -> IntentResult {
    if let Some(id) = target {
        state
            .session_mut(id)
            .cancel_streaming(jiff::Timestamp::now());
        return IntentResult::new_message(CancelStream {
            session_id: id.clone(),
        });
    }

    // None path: just clear the input buffer.
    state.update_active_input(ChatInputBoxState::reset);
    IntentResult::empty()
}

/// Handles the `CtrlClear` intent (universal `<c-c>` clear-or-leave).
///
/// Behavior is scope-aware:
/// - `Input`               -> clear chat input (empty input is a no-op).
/// - `Picker { .. }`       -> empty filter, or if already empty, redispatch `EnterNormalMode` to close the picker.
/// - `ArgInput`            -> clear input, or if already empty, pop the scope + reset state.
/// - `RenameSessionInput`  -> clear input, or if already empty, redispatch `RenameSessionLeave`.
///
/// Returns `(IntentResult, Option<Intent>)` matching the `PickerConfirm` redispatch pattern.
pub fn handle_ctrl_clear(state: &mut AppState) -> (IntentResult, Option<Intent>) {
    use crate::common::app_state::ArgInputState;
    use crate::common::focus::FocusScope;

    match state.frontend.scope() {
        FocusScope::Input => {
            state.update_active_input(ChatInputBoxState::reset);
            (IntentResult::empty(), None)
        }
        FocusScope::Picker { .. } => {
            if let Some(picker) = state.active_picker_ops() {
                if picker.is_filter_empty() {
                    (IntentResult::empty(), Some(Intent::EnterNormalMode))
                } else {
                    picker.clear_filter();
                    (IntentResult::empty(), None)
                }
            } else {
                (IntentResult::empty(), None)
            }
        }
        FocusScope::ArgInput => {
            if state.frontend.arg_input.text.input.is_empty() {
                state.frontend.scope_pop();
                state.frontend.arg_input = ArgInputState::default();
            } else {
                state.frontend.arg_input.text.set(String::new());
            }
            (IntentResult::empty(), None)
        }
        FocusScope::RenameSessionInput => {
            if state.frontend.rename_session_input.text.input.is_empty() {
                (IntentResult::empty(), Some(Intent::RenameSessionLeave))
            } else {
                let input = &mut state.frontend.rename_session_input;
                input.text.input.clear();
                input.text.cursor_pos = 0;
                (IntentResult::empty(), None)
            }
        }
        // Sidebar / Normal / SidebarResize / sidebar sections: <c-c> remains bound to Quit.
        _ => (IntentResult::empty(), None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::items_after_statements,
        reason = "test code"
    )]

    /// Empty slice registry + route table for handler tests that don't
    /// exercise slices or route rows.
    fn empty_slices() -> crate::common::slices::Slices {
        crate::common::slices::Slices::new()
    }

    fn empty_pickers() -> jinn_picker::PickerRegistry {
        jinn_picker::PickerRegistry::new()
    }

    fn empty_routes() -> crate::common::slices::key_routes::KeyRoutes {
        crate::common::slices::key_routes::KeyRoutes::new()
    }
    use super::*;
    use crate::common::focus::FocusScope;
    use crate::feat::session::phase_machine::PhaseKind;

    fn handle_quit(state: &mut AppState) -> IntentResult {
        super::handle_quit(state)
    }

    fn handle_toggle_whichkey(state: &mut AppState) -> IntentResult {
        super::handle_toggle_whichkey(state)
    }

    fn handle_toggle_audit_popup(state: &mut AppState) -> IntentResult {
        super::handle_toggle_audit_popup(state)
    }

    fn handle_interrupt(state: &mut AppState) -> IntentResult {
        super::handle_interrupt(state, None)
    }

    #[rstest::rstest]
    fn quit_sets_should_quit() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling Quit.
        let result = handle_quit(&mut state);

        // Then the quit latch is set.
        assert!(state.frontend.quit());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn toggle_whichkey_sets_tui_signal() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling ToggleWhichkey.
        let result = handle_toggle_whichkey(&mut state);

        // Then the toggle_whichkey signal is set.
        assert!(state.frontend.signals_snapshot().toggle_whichkey);
        assert!(result.message_names.is_empty());
    }
    #[rstest::rstest]
    fn toggle_audit_popup_off_to_on_sets_visibility_flag() {
        // Given a default state (popup hidden).
        let mut state = AppState::default_with_scope_focus();
        assert!(!state.frontend.audit_popup_visible);

        // When toggling once.
        let result = handle_toggle_audit_popup(&mut state);

        // Then the flag flips to true.
        assert!(state.frontend.audit_popup_visible);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn toggle_audit_popup_on_to_off_clears_visibility_flag() {
        // Given a state with the popup toggled on.
        let mut state = AppState::default_with_scope_focus();
        handle_toggle_audit_popup(&mut state);
        assert!(state.frontend.audit_popup_visible);

        // When toggling a second time.
        let result = handle_toggle_audit_popup(&mut state);

        // Then the flag flips back to false.
        assert!(!state.frontend.audit_popup_visible);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn audit_popup_remains_visible_when_input_mode_entered() {
        // Given a state with the audit popup toggled on, scoped to Normal mode.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_swap_base(crate::common::focus::FocusScope::Normal);
        handle_toggle_audit_popup(&mut state);
        assert!(state.frontend.audit_popup_visible);

        // When the user enters Input mode (pushes Input focus scope).
        state
            .frontend
            .scope_push(crate::common::focus::FocusScope::Input);

        // Then the popup flag remains on — it lives on FrontendState, not Mode.
        assert!(state.frontend.audit_popup_visible);
        // And the scope stack reflects Input mode (the `a` keybind is not
        // registered in Input mode, so the toggle cannot be flipped from here).
        assert_eq!(state.frontend.scope(), FocusScope::Input);
    }

    #[rstest::rstest]
    fn audit_popup_remains_visible_after_input_mode_exited() {
        // Given a state with the popup toggled on, scoped to Normal, then Input mode entered.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_swap_base(crate::common::focus::FocusScope::Normal);
        handle_toggle_audit_popup(&mut state);
        state
            .frontend
            .scope_push(crate::common::focus::FocusScope::Input);
        assert!(state.frontend.audit_popup_visible);

        // When the user pops back to Normal.
        state.frontend.scope_pop();

        // Then the flag still persists.
        assert!(state.frontend.audit_popup_visible);
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn interrupt_clears_buffer_when_non_empty() {
        // Given a state with text in the buffer.
        let mut state = AppState::default_with_scope_focus();
        state.update_active_input(|i| i.insert_grapheme_at_cursor('h'));

        // When handling Interrupt.
        let result = handle_interrupt(&mut state);

        // Then the buffer is cleared.
        assert!(
            state
                .active_session()
                .with_input(jinn_slices::ChatInputBoxState::is_empty, || true)
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn interrupt_clears_empty_buffer_is_noop() {
        // Given a state with empty buffer.
        let mut state = AppState::default_with_scope_focus();

        // When handling Interrupt.
        let result = handle_interrupt(&mut state);

        // Then no commands and buffer is still empty.
        assert!(
            state
                .active_session()
                .with_input(jinn_slices::ChatInputBoxState::is_empty, || true)
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn interrupt_does_not_cancel_stream() {
        // Given a state with empty buffer and active stream.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().begin_streaming();

        // When handling Interrupt.
        let result = handle_interrupt(&mut state);

        // Then no CancelStream command is emitted.
        assert!(result.message_names.is_empty());
        // And the session is still streaming.
        assert!(matches!(
            state.active_session().phase(),
            PhaseKind::Streaming
        ));
    }

    #[rstest::rstest]
    fn interrupt_with_specific_session_cancels_stream() {
        // Given two sessions, the second one streaming.
        use crate::protocol::SessionId;

        let mut state = AppState::default_with_scope_focus();
        let second_id = SessionId::new();
        let mut second_session = AppState::default_with_scope_focus();
        second_session.active_session_mut().begin_streaming();
        let mut second_session: crate::feat::session::chat_session::ChatSessionState =
            second_session
                .session
                .sessions_mut()
                .drain()
                .map(|(_, v)| v)
                .next()
                .unwrap();
        second_session.set_session_id(second_id.clone());
        state.session.insert(second_session);

        // When handling Interrupt targeting the second session.
        let result = super::handle_interrupt(&mut state, Some(&second_id));

        // Then the targeted session's stream is cancelled.
        assert!(matches!(
            state.session.get_unchecked(&second_id).phase(),
            PhaseKind::Idle
        ));
        // And a CancelStream message is returned for that session.
        assert_eq!(result.messages.len(), 1);
    }

    // ============================================================
    // CtrlClear tests
    // ============================================================

    fn handle_ctrl_clear(state: &mut AppState) -> (IntentResult, Option<Intent>) {
        super::handle_ctrl_clear(state)
    }

    #[rstest::rstest]
    fn ctrl_clear_input_nonempty_clears_buffer() {
        // Given a state in Input scope with text in the buffer.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Input);
        state.update_active_input(|i| i.insert_grapheme_at_cursor('h'));
        state.update_active_input(|i| i.insert_grapheme_at_cursor('i'));

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then the buffer is cleared and no redispatch is requested.
        assert!(
            state
                .active_session()
                .with_input(jinn_slices::ChatInputBoxState::is_empty, || true)
        );
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
    }

    #[rstest::rstest]
    fn ctrl_clear_input_empty_is_noop() {
        // Given a state in Input scope with empty buffer.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Input);

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then no commands, no redispatch, scope unchanged.
        assert!(
            state
                .active_session()
                .with_input(jinn_slices::ChatInputBoxState::is_empty, || true)
        );
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
        assert_eq!(state.frontend.scope(), FocusScope::Input);
    }

    #[rstest::rstest]
    fn ctrl_clear_picker_filter_nonempty_clears_filter() {
        // Given a state in Picker scope with a non-empty filter.
        use crate::protocol::PickerKind;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        {
            let picker = state.active_picker_ops().expect("picker active");
            picker.insert_char('a');
            picker.insert_char('b');
            assert!(!picker.is_filter_empty());
        }

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then the filter is cleared and no redispatch is requested.
        let picker = state.active_picker_ops().expect("picker still active");
        assert!(picker.is_filter_empty());
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
        assert!(state.frontend.is_picker());
    }

    #[rstest::rstest]
    fn ctrl_clear_picker_filter_empty_closes_picker() {
        // Given a state in Picker scope with an empty filter.
        use crate::feat::intent::handler::IntentHandler;
        use crate::protocol::PickerKind;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });

        // When handling CtrlClear via the IntentHandler (exercises redispatch).
        let result = IntentHandler::handle(
            &Intent::CtrlClear,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then scope is back to Normal (picker closed).
        assert!(!state.frontend.is_picker());
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn ctrl_clear_arg_input_nonempty_clears_input() {
        // Given a state in ArgInput scope with text in the input.
        use crate::common::app_state::ArgInputState;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input = ArgInputState {
            text: crate::common::line_input::LineInput {
                input: "some arg".to_owned(),
                cursor_pos: 8,
            },
            lifecycle_name: "abc".to_owned(),
            template_display: String::new(),
        };
        state.frontend.scope_push(FocusScope::ArgInput);

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then the input is cleared and scope is unchanged.
        assert!(state.frontend.arg_input.text.input.is_empty());
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 0);
        assert_eq!(state.frontend.scope(), FocusScope::ArgInput);
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
    }

    #[rstest::rstest]
    fn ctrl_clear_arg_input_empty_closes_scope() {
        // Given a state in ArgInput scope with empty input.
        use crate::common::app_state::ArgInputState;
        let lifecycle = "abc".to_owned();
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input = ArgInputState {
            text: crate::common::line_input::LineInput {
                input: String::new(),
                cursor_pos: 0,
            },
            lifecycle_name: lifecycle,
            template_display: String::new(),
        };
        state.frontend.scope_push(FocusScope::ArgInput);

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then scope is popped and arg_input is reset to default.
        assert_eq!(state.frontend.scope(), FocusScope::Input);
        // Default ArgInputState has empty lifecycle_name.
        assert_eq!(state.frontend.arg_input.lifecycle_name, "");
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
    }

    #[rstest::rstest]
    fn ctrl_clear_rename_nonempty_clears_input() {
        // Given a state in RenameSessionInput scope with text in the input.
        use crate::common::app_state::RenameSessionInputState;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "New Name".to_owned(),
                cursor_pos: 8,
            },
        };
        state.frontend.scope_push(FocusScope::RenameSessionInput);

        // When handling CtrlClear.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then the input is cleared and scope is unchanged.
        assert!(state.frontend.rename_session_input.text.input.is_empty());
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 0);
        assert_eq!(state.frontend.scope(), FocusScope::RenameSessionInput);
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
    }

    #[rstest::rstest]
    fn ctrl_clear_rename_empty_closes_popup() {
        // Given a state in RenameSessionInput scope with empty input.
        use crate::common::app_state::RenameSessionInputState;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState::default();
        state.frontend.scope_push(FocusScope::RenameSessionInput);

        // When handling CtrlClear via IntentHandler (exercises RenameSessionLeave redispatch).
        use crate::feat::intent::handler::IntentHandler;
        let result = IntentHandler::handle(
            &Intent::CtrlClear,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then scope is popped back to Normal and rename_session_input is reset.
        assert_eq!(state.frontend.scope(), FocusScope::Input);
        assert!(state.frontend.rename_session_input.text.input.is_empty());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn ctrl_clear_picker_two_presses_clears_then_closes() {
        // First <c-c> on a populated picker clears the filter;
        // the second <c-c> closes the picker (equivalent to <esc>).
        use crate::feat::intent::handler::IntentHandler;
        use crate::protocol::PickerKind;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        {
            let picker = state.active_picker_ops().expect("picker active");
            picker.insert_char('a');
            picker.insert_char('b');
            assert!(!picker.is_filter_empty());
        }

        // First press: filter is non-empty, so it should be cleared.
        let result1 = IntentHandler::handle(
            &Intent::CtrlClear,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );
        assert!(state.frontend.is_picker());
        assert!(
            state
                .active_picker_ops()
                .expect("picker still active")
                .is_filter_empty()
        );
        assert!(result1.messages.is_empty());

        // Second press: filter is now empty, so picker should close.
        let result2 = IntentHandler::handle(
            &Intent::CtrlClear,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );
        assert!(!state.frontend.is_picker());
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
        assert!(result2.messages.is_empty());
    }

    #[rstest::rstest]
    fn ctrl_clear_rename_pre_populated_clears_without_persisting() {
        // The rename popup is the only one pre-populated with the current session
        // title. A single <c-c> must clear the visible text without persisting
        // the rename (i.e. scope stays on RenameSessionInput).
        use crate::common::app_state::RenameSessionInputState;
        let mut state = AppState::default_with_scope_focus();
        state.frontend.rename_session_input = RenameSessionInputState {
            text: crate::common::line_input::LineInput {
                input: "My Session".to_owned(),
                cursor_pos: 10,
            },
        };
        state.frontend.scope_push(FocusScope::RenameSessionInput);

        // When handling CtrlClear once.
        let (result, maybe_intent) = handle_ctrl_clear(&mut state);

        // Then text is cleared but scope is unchanged (NOT persisted/closed).
        assert!(state.frontend.rename_session_input.text.input.is_empty());
        assert_eq!(state.frontend.rename_session_input.text.cursor_pos, 0);
        assert_eq!(state.frontend.scope(), FocusScope::RenameSessionInput);
        assert!(result.message_names.is_empty());
        assert!(maybe_intent.is_none());
    }
}
