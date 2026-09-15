//! Pins section validators - validate pin/unpin actions for the sidebar pins section.
//!
//! Pin/unpin actions are fallible; navigation intents within the sidebar
//! are handled by the section itself and are infallible.

use crate::common::app_state::AppState;
use wherror::Error;

/// Errors from validating pins section actions.
#[derive(Debug, Error)]
#[error(debug)]
pub enum PinsActionError {
    /// No pinned entry is selected.
    NoSelection,
    /// No pinned entries exist.
    Empty,
}

/// Validates the unpin action.
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_unpin(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Validates the pin-top action.
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_pin_top(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Validates a pin position action (top, bottom, or relative).
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_pin(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Validates the pin-bottom action.
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_pin_bottom(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Validates the pin-relative action.
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_pin_relative(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Validates the pin-cycle action.
///
/// # Errors
///
/// Returns an error if there are no pinned entries or no entry is selected.
pub fn validate_pin_cycle(state: &AppState) -> Result<(), PinsActionError> {
    validate_pin_action(state)
}

/// Shared validation logic for all pin/unpin actions.
///
/// Checks that there are pinned entries and one is selected.
fn validate_pin_action(state: &AppState) -> Result<(), PinsActionError> {
    if state.sorted_pinned_ids().is_empty() {
        return Err(PinsActionError::Empty);
    }
    if state
        .frontend
        .with_sections(|s| s.pins.selected_id().is_none(), || true)
    {
        return Err(PinsActionError::NoSelection);
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
    use crate::protocol::{ChatEntry, PinPosition};

    use super::*;

    fn state_with_selected_pin(text: &str, position: PinPosition) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let entry_id = {
            let index = state.active_session_mut().push_entry(ChatEntry::user(text));
            state.active_session().history()[index].id.clone()
        };
        state.active_session_mut().pin_entry(&entry_id, position);
        state
            .frontend
            .update_sections(|s| s.pins.select_by_id(entry_id));
        state
    }

    fn state_with_unselected_pin(text: &str, position: PinPosition) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let entry_id = {
            let index = state.active_session_mut().push_entry(ChatEntry::user(text));
            state.active_session().history()[index].id.clone()
        };
        state.active_session_mut().pin_entry(&entry_id, position);
        state
    }

    #[rstest::rstest]
    fn unpin_succeeds_with_selected_pinned_entry() {
        // Given a state with a pinned entry that is selected.
        let state = state_with_selected_pin("hello", PinPosition::Top);

        // When validating unpin.
        let result = validate_unpin(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn unpin_fails_with_no_pinned_entries() {
        // Given a state with no pinned entries.
        let state = AppState::default_with_scope_focus();

        // When validating unpin.
        let result = validate_unpin(&state);

        // Then it returns Empty error.
        assert!(matches!(result, Err(PinsActionError::Empty)));
    }

    #[rstest::rstest]
    fn unpin_fails_with_no_selection() {
        // Given a state with pinned entries but nothing selected.
        let state = state_with_unselected_pin("hello", PinPosition::Top);

        // When validating unpin.
        let result = validate_unpin(&state);

        // Then it returns NoSelection error.
        assert!(matches!(result, Err(PinsActionError::NoSelection)));
    }

    #[rstest::rstest]
    fn validate_pin_top_returns_empty_error_when_no_pinned_entries() {
        // Given a state with no pinned entries.
        let state = AppState::default_with_scope_focus();

        // When validating pin top.
        let result = validate_pin_top(&state);

        // Then it returns Empty error.
        assert!(matches!(result, Err(PinsActionError::Empty)));
    }

    #[rstest::rstest]
    fn validate_pin_top_returns_no_selection_error_when_nothing_selected() {
        // Given a state with pinned entries but nothing selected.
        let state = state_with_unselected_pin("hello", PinPosition::Top);

        // When validating pin top.
        let result = validate_pin_top(&state);

        // Then it returns NoSelection error.
        assert!(matches!(result, Err(PinsActionError::NoSelection)));
    }

    #[rstest::rstest]
    fn validate_pin_top_succeeds_with_selected_pinned_entry() {
        // Given a state with a pinned entry that is selected.
        let state = state_with_selected_pin("hello", PinPosition::Top);

        // When validating pin top.
        let result = validate_pin_top(&state);

        // Then it succeeds.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_pin_bottom_returns_empty_error_when_no_pinned_entries() {
        // Given a state with no pinned entries.
        let state = AppState::default_with_scope_focus();

        // When validating pin bottom.
        let result = validate_pin_bottom(&state);

        // Then it returns Empty error.
        assert!(matches!(result, Err(PinsActionError::Empty)));
    }

    #[rstest::rstest]
    fn validate_pin_bottom_returns_no_selection_error_when_nothing_selected() {
        // Given a state with pinned entries but nothing selected.
        let state = state_with_unselected_pin("hello", PinPosition::Top);

        // When validating pin bottom.
        let result = validate_pin_bottom(&state);

        // Then it returns NoSelection error.
        assert!(matches!(result, Err(PinsActionError::NoSelection)));
    }

    #[rstest::rstest]
    fn validate_pin_relative_returns_empty_error_when_no_pinned_entries() {
        // Given a state with no pinned entries.
        let state = AppState::default_with_scope_focus();

        // When validating pin relative.
        let result = validate_pin_relative(&state);

        // Then it returns Empty error.
        assert!(matches!(result, Err(PinsActionError::Empty)));
    }

    #[rstest::rstest]
    fn validate_pin_relative_returns_no_selection_error_when_nothing_selected() {
        // Given a state with pinned entries but nothing selected.
        let state = state_with_unselected_pin("hello", PinPosition::Top);

        // When validating pin relative.
        let result = validate_pin_relative(&state);

        // Then it returns NoSelection error.
        assert!(matches!(result, Err(PinsActionError::NoSelection)));
    }

    #[rstest::rstest]
    fn validate_pin_cycle_returns_empty_error_when_no_pinned_entries() {
        // Given a state with no pinned entries.
        let state = AppState::default_with_scope_focus();

        // When validating pin cycle.
        let result = validate_pin_cycle(&state);

        // Then it returns Empty error.
        assert!(matches!(result, Err(PinsActionError::Empty)));
    }

    #[rstest::rstest]
    fn validate_pin_cycle_returns_no_selection_error_when_nothing_selected() {
        // Given a state with pinned entries but nothing selected.
        let state = state_with_unselected_pin("hello", PinPosition::Top);

        // When validating pin cycle.
        let result = validate_pin_cycle(&state);

        // Then it returns NoSelection error.
        assert!(matches!(result, Err(PinsActionError::NoSelection)));
    }
}
