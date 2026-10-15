//! Navigation intent handlers - editor and working directory.
//!
//! What remains here is the platform concerns the kernel still holds; the
//! chat log's scroll handlers belong to the `jinn-chat-log-view` slice,
//! which owns them as route rows.

use crate::common::app_state::AppState;
use jinn_slices::RouteResult as IntentResult;

/// Number of lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_STEP: u16 = 3;

/// Scrolls the chat log up by one mouse wheel tick and moves cursor to first visible.
///
/// The keyboard scrolls are route rows owned by the `jinn-chat-log-view`
/// slice, but a wheel event is a crossterm backend handler on the
/// `Keymap` rather than a keymap node, so no route row can express it
/// and these two handlers stay in the kernel.
pub fn handle_mouse_scroll_up(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_up(MOUSE_SCROLL_STEP);
    state.active_session_mut().move_cursor_to_first_visible();
    IntentResult::empty()
}

/// Scrolls the chat log down by one mouse wheel tick and moves cursor to last visible.
pub fn handle_mouse_scroll_down(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_down(MOUSE_SCROLL_STEP);
    state.active_session_mut().move_cursor_to_last_visible();
    IntentResult::empty()
}

/// Opens the input in an external editor.
pub fn handle_edit_input(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_scope(|s| s.signals.edit_requested = true);
    IntentResult::empty()
}

/// Requests a CWD change via the external directory selection command.
///
/// Sets the `change_cwd_requested` TUI signal so the outer platform layer
/// can suspend the TUI and run the configured picker command.
pub fn handle_change_cwd(
    state: &mut AppState,
    root: jinn_slices::cwd_root::CwdRoot,
) -> IntentResult {
    state
        .frontend
        .update_scope(|s| s.signals.change_cwd_requested = Some(root));
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

    use super::*;

    #[rstest::rstest]
    fn edit_input_sets_tui_signal() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling EditInput.
        let _result = handle_edit_input(&mut state);

        // Then the edit_requested signal is set.
        assert!(state.frontend.signals_snapshot().edit_requested);
    }

    #[rstest::rstest]
    fn edit_input_returns_no_commands() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling EditInput.
        let result = handle_edit_input(&mut state);

        // Then no commands are emitted.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn change_cwd_sets_signal_to_session_root() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling ChangeCwd with Session root.
        let result = handle_change_cwd(&mut state, jinn_slices::cwd_root::CwdRoot::Session);

        // Then the signal is set with Session root.
        assert_eq!(
            state.frontend.signals_snapshot().change_cwd_requested,
            Some(jinn_slices::cwd_root::CwdRoot::Session)
        );
        // And no commands are emitted.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn change_cwd_sets_signal_to_home_root() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling ChangeCwd with Home root.
        let result = handle_change_cwd(&mut state, jinn_slices::cwd_root::CwdRoot::Home);

        // Then the signal is set with Home root.
        assert_eq!(
            state.frontend.signals_snapshot().change_cwd_requested,
            Some(jinn_slices::cwd_root::CwdRoot::Home)
        );
        // And no commands are emitted.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }
}
