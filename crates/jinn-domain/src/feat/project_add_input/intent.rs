//! Project-add input intent handlers - open, confirm, and leave.

use crate::common::app_state::AppState;
use crate::common::focus::FocusScope;
use crate::common::line_input::LineInput;
use crate::common::path_display::shorten_path;
use crate::feat::cwd_input::resolve::{CwdResolution, resolve_cwd_input};
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::feat::project::ProjectConfig;
use crate::feat::project_add_input::state::ProjectAddInputState;
use crate::protocol::IntentResult;

/// Opens the project-add input popup.
///
/// Pushes `FocusScope::ProjectAddInput` and seeds the input with the active
/// session's current cwd, tilde-compressed, with the cursor at the end so the
/// user can immediately append a subdirectory or edit the path. The resolver
/// expands `~` back on confirm, so the seeded value round-trips correctly.
pub fn handle_project_add_input_enter(state: &mut AppState) -> IntentResult {
    // Compute the display string from the active session cwd before the mutable
    // frontend write so there is no shared borrow held across the assignment.
    let mut text = LineInput::new();
    text.set(shorten_path(state.active_session().cwd()));

    state.frontend.project_add_input = ProjectAddInputState { text };
    state.frontend.scope_push(FocusScope::ProjectAddInput);
    IntentResult::empty()
}

/// Confirms the project-add input.
///
/// Resolves the typed path against the active session cwd; on success appends
/// the canonicalized path to `frontend.preferences.projects` optimistically and
/// emits [`UpdatePreferences`] with [`PreferenceUpdate::AddProject`] so the
/// `PreferencesActor` persists it and broadcasts `PreferencesUpdated`. The
/// state-sync actor overwrites `frontend.preferences` wholesale on the event,
/// which both persists the add and refreshes any open project picker. Then pops
/// the scope and clears state. On failure (not a dir / empty) stays open with
/// the inline error shown by the render footer.
pub fn handle_project_add_input_confirm(state: &mut AppState) -> IntentResult {
    let raw = state
        .frontend
        .project_add_input
        .text
        .input
        .trim()
        .to_owned();
    let current_cwd = state.active_session().cwd().to_owned();
    let resolution = resolve_cwd_input(&raw, &current_cwd);

    let CwdResolution::Ok(path) = resolution else {
        return IntentResult::empty();
    };

    // Optimistic update so the picker (if open underneath) reflects the add
    // immediately; the PreferencesActor re-applies the canonical result on its
    // broadcast and dedupes via AddProject::apply.
    state.frontend.preferences.projects.push(ProjectConfig {
        path: path.clone(),
        command_policy: Vec::new(),
    });

    // Pop scope and clear state.
    state.frontend.scope_pop();
    state.frontend.project_add_input = ProjectAddInputState::default();

    IntentResult::new_message(UpdatePreferences {
        updates: vec![PreferenceUpdate::AddProject(path)],
    })
}

/// Cancels the project-add input popup.
///
/// Pops the scope and discards the input state.
pub fn handle_project_add_input_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_pop();
    state.frontend.project_add_input = ProjectAddInputState::default();
    IntentResult::empty()
}

/// Inserts a character at the cursor position.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    state.frontend.project_add_input.text.insert_char(ch);
    IntentResult::empty()
}

/// Deletes the grapheme before the cursor.
pub fn handle_delete(state: &mut AppState) -> IntentResult {
    state.frontend.project_add_input.text.delete();
    IntentResult::empty()
}

/// Deletes the grapheme at/after the cursor (forward delete).
pub fn handle_delete_forward(state: &mut AppState) -> IntentResult {
    state.frontend.project_add_input.text.delete_forward();
    IntentResult::empty()
}

/// Moves the cursor one grapheme left.
pub fn handle_cursor_left(state: &mut AppState) -> IntentResult {
    state.frontend.project_add_input.text.cursor_left();
    IntentResult::empty()
}

/// Moves the cursor one grapheme right.
pub fn handle_cursor_right(state: &mut AppState) -> IntentResult {
    state.frontend.project_add_input.text.cursor_right();
    IntentResult::empty()
}

/// Handles `PasteText` - bulk inserts pasted text at the cursor.
pub fn handle_paste(state: &mut AppState, text: &str) -> IntentResult {
    state.frontend.project_add_input.text.paste(text);
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
    fn enter_pushes_project_add_input_scope_and_seeds_cwd() {
        // Given a state whose active session has a known absolute cwd.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/some-project"));

        // When opening the project-add input popup.
        let result = handle_project_add_input_enter(&mut state);

        // Then ProjectAddInput is the current scope and the input is seeded with
        // the session cwd, cursor at the end.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::ProjectAddInput
        ));
        assert_eq!(
            state.frontend.project_add_input.text.input,
            "/tmp/some-project"
        );
        assert_eq!(
            state.frontend.project_add_input.text.cursor_pos,
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

        // When opening the project-add input popup.
        handle_project_add_input_enter(&mut state);

        // Then the seeded input is the tilde-compressed form.
        assert_eq!(
            state.frontend.project_add_input.text.input,
            "~/projects/my-app"
        );
    }

    #[rstest::rstest]
    fn leave_pops_scope_and_clears_state() {
        // Given a state with project_add_input scope pushed + some typed text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ProjectAddInput);
        state.frontend.project_add_input.text.input = "/some/path".to_owned();

        // When leaving.
        let result = handle_project_add_input_leave(&mut state);

        // Then the scope is popped and state is cleared.
        assert!(!matches!(
            state.frontend.scope(),
            FocusScope::ProjectAddInput
        ));
        assert_eq!(state.frontend.project_add_input.text.input, "");
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_valid_dir_emits_update_preferences_with_add_project() {
        // Given a tempdir as the session cwd and the popup seeded with it.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = std::fs::canonicalize(temp.path()).expect("canonicalize");
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(target);
        handle_project_add_input_enter(&mut state);

        // When confirming.
        let result = handle_project_add_input_confirm(&mut state);

        // Then exactly one UpdatePreferences with AddProject is emitted.
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn confirm_valid_dir_pops_scope_and_clears_state() {
        // Given a tempdir and a seeded popup.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = std::fs::canonicalize(temp.path()).expect("canonicalize");
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(target);
        handle_project_add_input_enter(&mut state);

        // When confirming.
        let _ = handle_project_add_input_confirm(&mut state);

        // Then the scope was popped and the input state was cleared.
        assert!(!matches!(
            state.frontend.scope(),
            FocusScope::ProjectAddInput
        ));
        assert_eq!(state.frontend.project_add_input.text.input, "");
    }

    #[rstest::rstest]
    fn confirm_nonexistent_path_stays_open_unchanged() {
        // Given a state with project_add_input scope pushed + a bad path.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ProjectAddInput);
        state.frontend.project_add_input.text.input = "/this/does/not/exist".to_owned();

        // When confirming.
        let result = handle_project_add_input_confirm(&mut state);

        // Then nothing changed: scope intact, input intact, no message.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::ProjectAddInput
        ));
        assert_eq!(
            state.frontend.project_add_input.text.input,
            "/this/does/not/exist"
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_empty_input_is_noop() {
        // Given a state with project_add_input scope pushed + empty input.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ProjectAddInput);

        // When confirming with empty input.
        let result = handle_project_add_input_confirm(&mut state);

        // Then nothing changed.
        assert!(matches!(
            state.frontend.scope(),
            FocusScope::ProjectAddInput
        ));
        assert!(result.message_names.is_empty());
    }
}
