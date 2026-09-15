//! Session and model intent validators.
//!
//! Validators for model refresh, prompt template rescan, and session creation.

use crate::common::app_state::AppState;
use wherror::Error;

/// Errors from validating a RefreshModels intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum RefreshModelsError {
    /// No provider is configured.
    NoProvider,
}

/// Validates the RefreshModels intent.
///
/// Returns an error if no provider is configured.
///
/// # Errors
///
/// Returns an error if no provider is configured.
pub fn validate_refresh_models(state: &AppState) -> Result<(), RefreshModelsError> {
    if state.active_session().profile().model.is_no_provider() {
        return Err(RefreshModelsError::NoProvider);
    }
    Ok(())
}

/// Errors from validating a RescanPromptTemplates intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum RescanPromptTemplatesError {
    /// Prompt templates directory is not configured.
    NotConfigured,
}

/// Validates the RescanPromptTemplates intent.
///
/// Always succeeds for now. The error variant exists for future use.
///
/// # Errors
///
/// Returns an error if prompt templates directory is not configured.
pub fn validate_rescan_prompt_templates(
    _state: &AppState,
) -> Result<(), RescanPromptTemplatesError> {
    Ok(())
}

/// Validates the SessionNew intent.
///
/// Always succeeds - there are currently no conditions that prevent session creation.
pub fn validate_session_new(_state: &AppState) {
    // No validation needed.
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
    use crate::feat::session::model_selection::ModelSelection;
    use crate::protocol::PickerKind;

    #[rstest::rstest]
    fn refresh_models_succeeds_with_provider() {
        // Given a state with a configured provider.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama".to_owned()));

        // When validating refresh models.
        let result = validate_refresh_models(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn refresh_models_fails_with_no_provider() {
        // Given a state with the default no-provider ID.
        let state = AppState::default_with_scope_focus();

        // When validating refresh models.
        let result = validate_refresh_models(&state);

        // Then it returns NoProvider error.
        assert!(matches!(result, Err(RefreshModelsError::NoProvider)));
    }

    #[rstest::rstest]
    fn session_new_succeeds_when_no_picker_active() {
        // Given a state with no active picker.
        let state = AppState::default_with_scope_focus();

        // When validating session new.
        validate_session_new(&state);

        // Then validation passes (no panic, no rejection).
    }

    #[rstest::rstest]
    fn session_new_succeeds_when_picker_active() {
        // Given a state with an active picker.
        let state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Provider,
            });

        // When validating session new.
        validate_session_new(&state);

        // Then validation passes (picker is allowed).
    }
}
