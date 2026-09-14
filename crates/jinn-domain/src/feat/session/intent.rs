//! Session management intent handlers - session creation, model refresh, and prompt template rescan.

use crate::RefreshModels;
use crate::RescanPromptTemplates;
use crate::common::app_state::AppState;
use crate::protocol::{ChatEntry, IntentResult};

use super::validator;

/// Creates a new chat session, delegating to the blank lifecycle setup.
pub fn handle_session_new(state: &mut AppState) -> IntentResult {
    crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(state, "", &[], None)
}

/// Refreshes the model list from the active provider.
pub fn handle_refresh_models(state: &mut AppState) -> IntentResult {
    if validator::validate_refresh_models(state).is_err() {
        return IntentResult::empty();
    }

    state
        .active_session_mut()
        .push_entry(ChatEntry::transient("Refreshing models..."));

    IntentResult::new_message(RefreshModels)
}

/// Rescans prompt templates from disk.
pub fn handle_rescan_prompt_templates(state: &mut AppState) -> IntentResult {
    let _ = validator::validate_rescan_prompt_templates(state);

    state
        .active_session_mut()
        .push_entry(ChatEntry::system("Rescanning prompt templates..."));

    let session_id = state.active_session().session_id().clone();
    let cwd = state.active_session().cwd().to_path_buf();

    IntentResult::new_message(RescanPromptTemplates { session_id, cwd })
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
    use crate::common::app_state::AppState;
    use crate::protocol::{ChatEntry, ChatEntryKind, PickerKind};

    use crate::feat::session::model_selection::ModelSelection;

    use super::*;

    #[rstest::rstest]
    fn session_new_creates_fresh_session() {
        // Given a state with an existing session.
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("old"));

        // When handling SessionNew.
        let _result = handle_session_new(&mut state);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        assert!(state.active_session().history().is_empty());
        assert!(!state.frontend.scope_stack.is_picker());
        // And the old session is preserved in the sessions map.
        assert!(state.session.contains(&old_id));
    }

    #[rstest::rstest]
    fn session_new_closes_picker_and_creates_session() {
        // Given a state with an active picker.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Provider,
            });
        let old_id = state.session.active_session_id().clone();

        // When handling SessionNew.
        let _result = handle_session_new(&mut state);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the picker is closed.
        assert!(!state.frontend.scope_stack.is_picker());
    }

    #[rstest::rstest]
    fn refresh_models_posts_transient_message() {
        // Given a state with a provider.
        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama".to_owned()));

        // When handling RefreshModels.
        let _result = handle_refresh_models(&mut state);

        // Then a transient message was posted.
        let last = state
            .active_session()
            .history()
            .last()
            .expect("should have entry");
        assert!(matches!(last.kind, ChatEntryKind::Transient(_)));
    }

    #[rstest::rstest]
    fn refresh_models_returns_refresh_command() {
        // Given a state with a provider.
        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama".to_owned()));
        let _initial_len = state.active_session().history().len();

        // When handling RefreshModels.
        let result = handle_refresh_models(&mut state);

        // Then a message is returned.
        assert!(!result.messages.is_empty());
    }

    #[rstest::rstest]
    fn refresh_models_noop_with_no_provider() {
        // Given a state with no provider.
        let mut state = AppState::default();

        // When handling RefreshModels.
        let result = handle_refresh_models(&mut state);

        // Then no commands.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn rescan_prompt_templates_posts_system_message() {
        // Given a default state.
        let mut state = AppState::default();
        let initial_len = state.active_session().history().len();

        // When handling RescanPromptTemplates.
        let _result = handle_rescan_prompt_templates(&mut state);

        // Then a system message was posted.
        assert_eq!(state.active_session().history().len(), initial_len + 1);
    }

    #[rstest::rstest]
    fn rescan_prompt_templates_returns_rescan_command() {
        // Given a default state.
        let mut state = AppState::default();
        let _initial_len = state.active_session().history().len();

        // When handling RescanPromptTemplates.
        let result = handle_rescan_prompt_templates(&mut state);

        // Then a message is returned.
        assert!(!result.messages.is_empty());
    }

    #[rstest::rstest]
    fn session_new_inherits_active_session_cwd() {
        // Given a state whose active session has a distinct CWD (not the app
        // launch dir).
        let mut state = AppState::default();
        let inherited_cwd = std::path::PathBuf::from("/tmp/inherited-project");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        assert_ne!(state.active_session().cwd(), state.session.default_cwd());

        // When handling SessionNew.
        let _result = handle_session_new(&mut state);

        // Then the new session inherited the active session's CWD.
        assert_eq!(state.active_session().cwd(), inherited_cwd);
    }
}
