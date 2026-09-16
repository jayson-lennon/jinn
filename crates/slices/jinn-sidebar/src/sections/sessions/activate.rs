//! Activates the session under the cursor.

use jinn_domain::common::app_state::AppState;

use crate::sections::sessions::state::sorted_open_sessions;
use jinn_domain::protocol::IntentResult;

/// Activates the session under the cursor.
///
/// Called when the user presses Enter in the sessions section.
/// Uses `swap_base` to replace the entire scope stack, effectively
/// closing the sidebar and switching to the target view.
/// - For session entries: swaps to Normal (chat view). No re-scan commands
///   are emitted: each session's discovered skills/prompts/context-files
///   are ephemeral and persist across activation changes, and were
///   hydrated when the session was created/loaded.
pub fn handle_session_activate(state: &mut AppState) -> IntentResult {
    use jinn_domain::common::app_state::FocusScope;

    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    ) {
        return IntentResult::empty();
    }
    let Some(index) = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
    else {
        return IntentResult::empty();
    };
    let sessions = sorted_open_sessions(state);
    let Some(entry) = sessions.get(index) else {
        return IntentResult::empty();
    };

    state.session.set_active(entry.id.clone());
    state.frontend.scope_swap_base(FocusScope::Normal);
    IntentResult::empty()
}

/// Activates the session under the cursor and enters Insert mode.
///
/// Called when the user presses `i` in the sessions section. Like
/// [`handle_session_activate`] but lands in Input mode instead of Normal,
/// so the user can immediately start typing. The scope stack ends up
/// `[Normal, Input]` — Normal as the base so that ESC (`clear_overlays`)
/// correctly returns to Normal, with Input on top as the active mode.
/// - For session entries: activates the session, swaps to Normal as the
///   base, then pushes Input.
pub fn handle_session_activate_insert(state: &mut AppState) -> IntentResult {
    use jinn_domain::common::app_state::FocusScope;

    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    ) {
        return IntentResult::empty();
    }
    let Some(index) = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
    else {
        return IntentResult::empty();
    };
    let sessions = sorted_open_sessions(state);
    let Some(entry) = sessions.get(index) else {
        return IntentResult::empty();
    };

    state.session.set_active(entry.id.clone());
    state.frontend.scope_swap_base(FocusScope::Normal);
    state.frontend.scope_push(FocusScope::Input);
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
    use jinn_domain::common::app_state::AppState;
    use jinn_slices::FocusScope;

    use jinn_domain::protocol::SessionId;

    /// Two sessions exist in state; cursor points at the second-inserted session.
    fn state_with_two_sessions_cursor_on_second() -> (AppState, SessionId) {
        let mut state = AppState::default_with_scope_focus();
        let _first = state.session.active_session_id().clone();
        let second_session = jinn_domain::feat::session::chat_session::ChatSessionState::default();
        let second = second_session.session_id().clone();
        state.session.insert(second_session);
        // Cursor points at the second session in sorted order.
        let sessions = sorted_open_sessions(&state);
        let target_idx = sessions
            .iter()
            .position(|e| e.id == second)
            .expect("second session present");
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(target_idx));
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        (state, second)
    }

    #[rstest::rstest]
    fn activate_session_switches_active_session_and_emits_no_commands() {
        // Given a sessions sidebar with cursor on a non-active session.
        let (mut state, expected_id) = state_with_two_sessions_cursor_on_second();

        // When activating.
        let result = handle_session_activate(&mut state);

        // Then the active session is now the one under the cursor.
        assert_eq!(state.session.active_session_id(), &expected_id);
        // And no commands are emitted: each session's discovered
        // skills/prompts/context-files are ephemeral and were hydrated when the
        // session was created/loaded, so activation needs no re-scan.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn activate_session_with_no_cursor_emits_nothing() {
        // Given sessions sidebar but no selected index.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        // selected_index stays None.

        // When activating.
        let result = handle_session_activate(&mut state);

        // Then no commands emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn activate_outside_sessions_section_emits_nothing() {
        // Given Normal scope (not sessions sidebar).
        let mut state = AppState::default_with_scope_focus();

        // When activating.
        let result = handle_session_activate(&mut state);

        // Then no commands emitted.
        assert!(result.message_names.is_empty());
    }
    #[rstest::rstest]
    fn activate_insert_switches_active_session() {
        // Given a sessions sidebar with cursor on a non-active session.
        let (mut state, expected_id) = state_with_two_sessions_cursor_on_second();

        // When activating into insert mode.
        handle_session_activate_insert(&mut state);

        // Then the active session is now the one under the cursor.
        assert_eq!(state.session.active_session_id(), &expected_id);
    }

    #[rstest::rstest]
    fn activate_insert_pushes_input_with_normal_base() {
        // Given a sessions sidebar with cursor on a session.
        let (mut state, _expected_id) = state_with_two_sessions_cursor_on_second();

        // When activating into insert mode.
        handle_session_activate_insert(&mut state);

        // Then the top of the stack is Input (insert mode).
        assert_eq!(state.frontend.scope(), FocusScope::Input);
        // And the base is Normal so ESC can return there via clear_overlays.
        assert_eq!(state.frontend.scope_parent(), Some(FocusScope::Normal));
    }

    #[rstest::rstest]
    fn activate_insert_outside_sessions_section_is_noop() {
        // Given Normal scope (not sessions sidebar).
        let mut state = AppState::default_with_scope_focus();
        let initial_scope = state.frontend.scope().clone();

        // When activating into insert mode.
        handle_session_activate_insert(&mut state);

        // Then the scope is unchanged.
        assert_eq!(state.frontend.scope(), initial_scope);
    }

    #[rstest::rstest]
    fn activate_insert_with_no_selected_index_is_noop() {
        // Given sessions sidebar but no selected index.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        let initial_scope = state.frontend.scope().clone();

        // When activating into insert mode.
        handle_session_activate_insert(&mut state);

        // Then the scope is unchanged.
        assert_eq!(state.frontend.scope(), initial_scope);
    }
}
