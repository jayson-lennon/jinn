//! Cwd input intent handlers - open, confirm, and leave.
//!
//! Skeleton stubs; filled in Phase 4.

use crate::common::app_state::AppState;
use crate::common::focus::FocusScope;
use crate::common::line_input::LineInput;
use crate::common::path_display::shorten_path;
use crate::feat::cwd_input::resolve::{CwdResolution, resolve_cwd_input};
use crate::feat::cwd_input::state::CwdInputState;
use crate::feat::session_lifecycle::protocol::command::SetSessionCwd;
use crate::protocol::IntentResult;

/// Opens the cwd input popup.
///
/// Pushes `FocusScope::CwdInput` and seeds the input with the active session's
/// current cwd, tilde-compressed, with the cursor at the end so the user can
/// immediately append a subdirectory or edit the path. The resolver expands
/// `~` back on confirm, so the seeded value round-trips correctly.
pub fn handle_cwd_input_enter(state: &mut AppState) -> IntentResult {
    // Compute the display string from the active session cwd before the mutable
    // frontend write so there is no shared borrow held across the assignment.
    let mut text = LineInput::new();
    text.set(shorten_path(state.active_session().cwd()));

    state.frontend.cwd_input = CwdInputState { text };
    state.frontend.scope_push(FocusScope::CwdInput);
    IntentResult::empty()
}

/// Confirms the cwd input.
///
// Resolves the typed path against the active session cwd; on success emits a
// [`SetSessionCwd`] command so the session actor applies the new cwd and
// broadcasts `SessionCwdChanged`. The event-driven scan actors pick up the
// new cwd automatically — no imperative scan commands are needed here.
// Then pops the scope and clears state. On failure (not a dir / empty) stays
// open with the inline error shown by the render footer.
pub fn handle_cwd_input_confirm(state: &mut AppState) -> IntentResult {
    let raw = state.frontend.cwd_input.text.input.trim().to_owned();
    let current_cwd = state.active_session().cwd().to_owned();
    let resolution = resolve_cwd_input(&raw, &current_cwd);

    if let CwdResolution::Ok(path) = resolution {
        let session_id = state.session.active_session_id().clone();
        let msg = SetSessionCwd {
            session_id,
            cwd: path,
        };

        state.frontend.scope_pop();
        state.frontend.cwd_input = CwdInputState::default();
        return IntentResult::new_message(msg);
    }

    IntentResult::empty()
}

/// Cancels the cwd input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_cwd_input_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_pop();
    state.frontend.cwd_input = CwdInputState::default();
    IntentResult::empty()
}

/// Inserts a character at the cursor position.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    state.frontend.cwd_input.text.insert_char(ch);
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    state.frontend.cwd_input.text.delete();
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    state.frontend.cwd_input.text.delete_forward();
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    state.frontend.cwd_input.text.cursor_left();
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    state.frontend.cwd_input.text.cursor_right();
    IntentResult::empty()
}

/// Handles `PasteText` - bulk inserts pasted text at the cursor.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    state.frontend.cwd_input.text.paste(text);
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
    use crate::common::app_state::FocusScope;

    #[rstest::rstest]
    fn enter_pushes_cwd_input_scope_and_seeds_cwd() {
        // Given a state whose active session has a known absolute cwd.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/some-project"));

        // When opening the cwd input popup.
        let result = handle_cwd_input_enter(&mut state);

        // Then CwdInput is the current scope and the input is seeded with the
        // session cwd (tilde-compressed by shorten_path), cursor at the end.
        assert!(matches!(state.frontend.scope(), FocusScope::CwdInput));
        assert_eq!(state.frontend.cwd_input.text.input, "/tmp/some-project");
        assert_eq!(
            state.frontend.cwd_input.text.cursor_pos,
            "/tmp/some-project".len()
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn enter_seeds_tilde_compressed_path_when_cwd_under_home() {
        // Given a session whose cwd is under $HOME.
        let mut state = AppState::default_with_scope_focus();
        let home = dirs::home_dir().expect("home dir exists");
        state
            .active_session_mut()
            .set_cwd(home.join("projects/my-app"));

        // When opening the cwd input popup.
        handle_cwd_input_enter(&mut state);

        // Then the seeded input is the tilde-compressed form.
        assert_eq!(state.frontend.cwd_input.text.input, "~/projects/my-app");
    }

    #[rstest::rstest]
    fn enter_leaves_cursor_at_end_of_seeded_text() {
        // Given a session with a known absolute cwd.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/some-project"));

        // When opening the cwd input popup.
        handle_cwd_input_enter(&mut state);

        // Then the cursor sits at the end of the seeded text (appending-friendly).
        let input = &state.frontend.cwd_input.text;
        assert_eq!(input.cursor_pos, input.input.len());
    }

    #[rstest::rstest]
    fn enter_seeds_absolute_path_when_cwd_not_under_home() {
        // Given a session whose cwd is not under $HOME.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/some-project"));

        // When opening the cwd input popup.
        handle_cwd_input_enter(&mut state);

        // Then the seeded input is the raw absolute path (no tilde compression).
        assert_eq!(state.frontend.cwd_input.text.input, "/tmp/some-project");
    }

    #[rstest::rstest]
    fn leave_pops_scope_and_clears_state() {
        // Given a state with cwd_input scope pushed + some typed text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::CwdInput);
        state.frontend.cwd_input.text.input = "/some/path".to_owned();

        // When leaving.
        let result = handle_cwd_input_leave(&mut state);

        // Then the scope is popped and state is cleared.
        assert!(!matches!(state.frontend.scope(), FocusScope::CwdInput));
        assert_eq!(state.frontend.cwd_input.text.input, "");
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_unchanged_seed_emits_set_session_cwd() {
        // Given a real tempdir as the session cwd, then opening the popup seeds
        // the box with that same path (tilde-compressed or not). Simulating an
        // immediate confirm with no edits.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = std::fs::canonicalize(temp.path()).expect("canonicalize");
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(target);
        handle_cwd_input_enter(&mut state); // seeds the box with the cwd

        // When confirming the unchanged seed.
        let result = handle_cwd_input_confirm(&mut state);

        // Then exactly one SetSessionCwd is emitted for the canonicalized cwd.
        // The unchanged seed is intentionally NOT special-cased: confirming it
        // re-applies the cwd and is permitted to re-fire SessionCwdChanged.
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn confirm_valid_dir_returns_set_session_cwd_command() {
        // Given a tempdir and a session whose cwd is the tempdir's parent.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path(); // target is itself a real dir
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(target.parent().unwrap().to_path_buf());
        // And the cwd_input has the tempdir's basename as text (relative).
        state.frontend.cwd_input.text.input =
            target.file_name().unwrap().to_string_lossy().to_string();

        // When confirming.
        let result = handle_cwd_input_confirm(&mut state);

        // Then one message is returned for the active session with the
        // canonicalized target cwd.
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn confirm_valid_dir_pops_scope_and_clears_state() {
        // Given a tempdir and a session whose cwd is the tempdir's parent, with
        // the cwd input scope pushed and some typed text.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path();
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(target.parent().unwrap().to_path_buf());
        state.frontend.scope_push(FocusScope::CwdInput);
        state.frontend.cwd_input.text.input =
            target.file_name().unwrap().to_string_lossy().to_string();

        // When confirming.
        let _ = handle_cwd_input_confirm(&mut state);

        // Then the scope was popped and the input state was cleared.
        assert!(!matches!(state.frontend.scope(), FocusScope::CwdInput));
        assert_eq!(state.frontend.cwd_input.text.input, "");
    }

    #[rstest::rstest]
    fn confirm_nonexistent_path_stays_open_unchanged() {
        // Given a state with cwd_input scope pushed + a bad path.
        let mut state = AppState::default_with_scope_focus();
        let original_cwd = state.active_session().cwd().to_owned();
        state.frontend.scope_push(FocusScope::CwdInput);
        state.frontend.cwd_input.text.input = "/this/does/not/exist".to_owned();

        // When confirming.
        let result = handle_cwd_input_confirm(&mut state);

        // Then nothing changed: scope intact, cwd unchanged, input intact.
        assert!(matches!(state.frontend.scope(), FocusScope::CwdInput));
        assert_eq!(state.active_session().cwd(), original_cwd);
        assert_eq!(state.frontend.cwd_input.text.input, "/this/does/not/exist");
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_empty_input_is_noop() {
        // Given a state with cwd_input scope pushed + empty input.
        let mut state = AppState::default_with_scope_focus();
        let original_cwd = state.active_session().cwd().to_owned();
        state.frontend.scope_push(FocusScope::CwdInput);

        // When confirming with empty input.
        let result = handle_cwd_input_confirm(&mut state);

        // Then nothing changed.
        assert!(matches!(state.frontend.scope(), FocusScope::CwdInput));
        assert_eq!(state.active_session().cwd(), original_cwd);
        assert!(result.message_names.is_empty());
    }
}
