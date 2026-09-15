//! Picker intent validators.
//!
//! Validators for picker navigation, confirmation, and opening intents.
//! Most are infallible; picker confirm and open picker are fallible.

use crate::common::app_state::AppState;
use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::PickerKind;
use wherror::Error;

/// Validates the PickerInsertChar intent.
pub fn validate_picker_insert_char(_state: &AppState, _ch: char) {}

/// Validates the PickerBackspace intent.
pub fn validate_picker_backspace(_state: &AppState) {}

/// Validates the PickerMoveUp intent.
pub fn validate_picker_move_up(_state: &AppState) {}

/// Validates the PickerMoveDown intent.
pub fn validate_picker_move_down(_state: &AppState) {}

/// Validates the PickerPageUp intent.
pub fn validate_picker_page_up(_state: &AppState) {}

/// Validates the PickerPageDown intent.
pub fn validate_picker_page_down(_state: &AppState) {}

/// Validates the PickerMoveCursorLeft intent.
pub fn validate_picker_move_cursor_left(_state: &AppState) {}

/// Validates the PickerMoveCursorRight intent.
pub fn validate_picker_move_cursor_right(_state: &AppState) {}

/// Errors from validating a PickerConfirm intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum PickerConfirmError {
    /// No picker is active.
    NoActivePicker,
    /// No item is selected in the picker.
    NoSelection,
}

/// Validates the PickerConfirm intent.
///
/// Returns an error if no picker is active or no item is selected.
///
/// # Errors
///
/// Returns an error if no picker is active or no item is selected.
pub fn validate_picker_confirm(state: &AppState) -> Result<(), PickerConfirmError> {
    let kind = state
        .frontend
        .picker_kind()
        .ok_or(PickerConfirmError::NoActivePicker)?;

    let has_selection = match kind {
        PickerKind::Provider => state.provider.provider_picker.selected_item().is_some(),
        PickerKind::Session => state.frontend.session_picker().selected_item().is_some(),
        PickerKind::Persona => state.frontend.persona_picker().selected_item().is_some(),
        PickerKind::Theme => state.frontend.theme_picker().selected_item().is_some(),
        PickerKind::SessionLifecycle => state
            .frontend
            .session_lifecycle_picker()
            .selected_item()
            .is_some(),
        PickerKind::ReasoningEffort => state
            .frontend
            .reasoning_effort_picker()
            .selected_item()
            .is_some(),
        PickerKind::Tool => state.frontend.tool_picker().selected_item().is_some(),
        PickerKind::Skill => state.frontend.skill_picker().selected_item().is_some(),
        // TaskList and Plugin are read-only; Enter is a no-op. Skip the
        // selection gate so the confirm handler (which itself returns
        // empty) is always reached.
        PickerKind::TaskList | PickerKind::Plugin => true,
        PickerKind::Project => state.frontend.project_picker().selected_item().is_some(),
        PickerKind::McpServer => state.frontend.mcp_server_picker().selected_item().is_some(),
        PickerKind::Endpoint => state.frontend.endpoint_picker().selected_item().is_some(),
        // CompactionModel has no picker state (the kind is retired).
        PickerKind::CompactionModel => false,
    };

    if has_selection {
        Ok(())
    } else {
        Err(PickerConfirmError::NoSelection)
    }
}

/// Errors from validating an OpenPicker intent.
#[derive(Debug, Error)]
#[error(debug)]
pub enum OpenPickerError {
    /// Already in picker mode.
    AlreadyInPicker,
}

/// Validates the OpenPicker intent.
///
/// Returns an error if a picker is already active.
///
/// # Errors
///
/// Returns an error if a picker is already active.
pub fn validate_open_picker(state: &AppState, _kind: &PickerKind) -> Result<(), OpenPickerError> {
    if state.frontend.is_picker() {
        return Err(OpenPickerError::AlreadyInPicker);
    }
    Ok(())
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
    use crate::common::app_state::AppState;
    use crate::common::app_state::FocusScope;

    #[rstest::rstest]
    fn validate_picker_confirm_rejects_no_active_picker() {
        // If the validator always returned Ok, confirming with no picker would be allowed.
        let state = AppState::default_with_scope_focus();

        let result = validate_picker_confirm(&state);

        assert!(
            result.is_err(),
            "should reject confirm when no picker is active"
        );
    }

    #[rstest::rstest]
    fn validate_open_picker_rejects_when_already_in_picker() {
        // If the validator always returned Ok, nested pickers would be allowed.
        let state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });

        let result = validate_open_picker(&state, &PickerKind::Session);

        assert!(result.is_err(), "should reject opening a second picker");
    }

    #[rstest::rstest]
    fn validate_open_picker_allows_when_no_picker_active() {
        // Verifies the positive case - opening a picker when none is active.
        let state = AppState::default_with_scope_focus();

        let result = validate_open_picker(&state, &PickerKind::Provider);

        assert!(
            result.is_ok(),
            "should allow opening picker when none is active"
        );
    }

    #[rstest::rstest]
    fn validate_picker_confirm_accepts_reasoning_with_selection() {
        // If the selection gate were broken, confirming with a selection would
        // be rejected.
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};

        let mut state = AppState::default_with_scope_focus();
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        let wrapped = crate::feat::picker::registry::build_picker_registry()
            .make_items(
                crate::feat::picker::registry::REASONING_EFFORT_ID,
                vec![entry],
            )
            .unwrap_or_default();
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(wrapped);
        state.frontend.reasoning_effort_picker_mut().move_down(1);
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::ReasoningEffort,
        });

        let result = validate_picker_confirm(&state);

        assert!(
            result.is_ok(),
            "should accept confirm when a reasoning entry is selected"
        );
    }

    #[rstest::rstest]
    fn validate_picker_confirm_rejects_reasoning_without_selection() {
        // If the selection gate were broken, confirming with no selection
        // would be allowed.
        let state = AppState::default_with_scope_focus();
        // No entries set, so no selection.
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::ReasoningEffort,
        });

        let result = validate_picker_confirm(&state);

        assert!(
            result.is_err(),
            "should reject confirm when no reasoning entry is selected"
        );
    }
}
