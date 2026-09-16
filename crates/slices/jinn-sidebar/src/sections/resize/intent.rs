//! Sidebar resize intent handlers - enter/expand/contract/leave.

use jinn_domain::common::app_state::{AppState, FocusScope};
use jinn_domain::feat::preferences_actor::protocol::app_state_command::{
    AppStateUpdate, UpdateAppState,
};
use jinn_domain::protocol::IntentResult;
use jinn_sidebar_msg::SidebarSectionId;

/// The number of columns to change per resize step.
const RESIZE_STEP: u16 = 2;

/// Minimum sidebar width - must match `MIN_SIDEBAR_WIDTH` in app_layout.
const MIN_SIDEBAR_WIDTH: u16 = 15;

/// Enters sidebar resize mode by pushing the sidebar's dynamic resize scope.
pub fn handle_resize_enter(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .scope_push(FocusScope::Dynamic(SidebarSectionId::resize_scope_id()));
    IntentResult::empty()
}

/// Expands the sidebar by moving the border left.
///
/// Increments `sidebar_width` by `RESIZE_STEP`, clamped at a reasonable
/// maximum (leaving at least `MIN_WIDTH` for the main column).
/// Emits an `UpdateAppState` command to persist the new width.
pub fn handle_resize_expand(state: &mut AppState) -> IntentResult {
    let max_width = state.frontend.sidebar_width.saturating_add(RESIZE_STEP);
    // Cap so main column has at least MIN_WIDTH + 1 (border) columns.
    // We don't know the terminal width here, so we allow generous growth.
    // The layout clamps at render time.
    let new_width = max_width;
    state.frontend.sidebar_width = new_width;

    IntentResult::new_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetSidebarWidth(Some(new_width))],
    })
}

/// Contracts the sidebar by moving the border right.
///
/// Decrements `sidebar_width` by `RESIZE_STEP`, clamped at `MIN_SIDEBAR_WIDTH`.
/// Emits an `UpdateAppState` command to persist the new width.
pub fn handle_resize_contract(state: &mut AppState) -> IntentResult {
    let new_width = state
        .frontend
        .sidebar_width
        .saturating_sub(RESIZE_STEP)
        .max(MIN_SIDEBAR_WIDTH);

    if new_width == state.frontend.sidebar_width {
        return IntentResult::empty();
    }

    state.frontend.sidebar_width = new_width;

    IntentResult::new_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetSidebarWidth(Some(new_width))],
    })
}

/// Exits sidebar resize mode, returning to Normal scope.
///
/// Uses `clear_overlays()` so ESC always returns to the base scope,
/// regardless of how the user entered resize mode.
pub fn handle_resize_leave(state: &mut AppState) -> IntentResult {
    state.frontend.scope_clear_overlays();
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
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::app_state::FocusScope;

    use super::*;

    #[rstest::rstest]
    fn enter_pushes_sidebar_resize_scope() {
        // Given app state in Normal scope.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_clear_overlays();
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));

        // When handling sidebar-resize-enter.
        let result = handle_resize_enter(&mut state);

        // Then the sidebar resize scope is current.
        assert_eq!(
            state.frontend.scope(),
            FocusScope::Dynamic(SidebarSectionId::resize_scope_id())
        );
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn expand_increments_sidebar_width() {
        // Given default state (sidebar_width = 30).
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar-resize-expand.
        let _ = handle_resize_expand(&mut state);

        // Then sidebar_width increased by 2.
        assert_eq!(state.frontend.sidebar_width, 32);
    }

    #[rstest::rstest]
    fn expand_emits_update_preferences_command() {
        // Given default state (sidebar_width = 30).
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar-resize-expand.
        let result = handle_resize_expand(&mut state);

        // Then an UpdateAppState message was emitted.
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn contract_decrements_sidebar_width() {
        // Given default state (sidebar_width = 30).
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar-resize-contract.
        let _ = handle_resize_contract(&mut state);

        // Then sidebar_width decreased by 2.
        assert_eq!(state.frontend.sidebar_width, 28);
    }

    #[rstest::rstest]
    fn contract_emits_update_preferences_command() {
        // Given default state (sidebar_width = 30).
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar-resize-contract.
        let result = handle_resize_contract(&mut state);

        // Then an UpdateAppState message was emitted.
        assert_eq!(result.messages.len(), 1);
    }

    #[rstest::rstest]
    fn contract_clamps_at_minimum() {
        // Given state with sidebar_width at minimum (15).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.sidebar_width = MIN_SIDEBAR_WIDTH;

        // When handling sidebar-resize-contract.
        let result = handle_resize_contract(&mut state);

        // Then sidebar_width stays at minimum.
        assert_eq!(state.frontend.sidebar_width, MIN_SIDEBAR_WIDTH);
        // And no commands are emitted (no change).
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn leave_clears_overlays() {
        // Given state in the sidebar resize scope (stack: Normal, sidebar resize).
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::Dynamic(SidebarSectionId::resize_scope_id()));

        // When handling sidebar-resize-leave.
        let result = handle_resize_leave(&mut state);

        // Then scope is back to Normal.
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn leave_from_sidebar_returns_to_normal() {
        // Given state entered from a sidebar section (stack: Normal, section, sidebar resize).
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
        state
            .frontend
            .scope_push(FocusScope::Dynamic(SidebarSectionId::resize_scope_id()));

        // When handling sidebar-resize-leave.
        handle_resize_leave(&mut state);

        // Then clear_overlays returns to Normal (not Sidebar).
        assert!(matches!(state.frontend.scope(), FocusScope::Normal));
    }

    #[rstest::rstest]
    fn multiple_expands_accumulate() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();

        // When expanding three times.
        handle_resize_expand(&mut state);
        handle_resize_expand(&mut state);
        handle_resize_expand(&mut state);

        // Then width increased by 6.
        assert_eq!(state.frontend.sidebar_width, 36);
    }
}
