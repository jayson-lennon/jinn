//! Sidebar-level intent handlers.
//!
//! Handles entering and leaving the sidebar scope. These are sidebar-panel
//! concerns, not section-specific - sections never handle ESC.

use crate::IntentResult;
use crate::common::app_state::AppState;

/// Handles `SidebarFocus` - enters sidebar scope.
///
/// Defaults focus to the Persona section (topmost section).
pub fn handle_sidebar_focus(state: &mut AppState) -> IntentResult {
    use crate::common::app_state::FocusScope;
    use crate::feat::ui::sidebar::section_trait::{EnterFrom, SidebarSectionId};

    state.frontend.scope_push(FocusScope::SidebarPersona);

    // If a section already has cursor state, restore it.
    let has_existing_cursor = state.frontend.persona_section.cursor.is_some()
        || state.frontend.pins.selected_id().is_some()
        || state.frontend.sessions_section.selected_index.is_some();

    if has_existing_cursor {
        // Restore to whichever section has a cursor.
        let section = if state.frontend.sessions_section.selected_index.is_some() {
            SidebarSectionId::Sessions
        } else if state.frontend.pins.selected_id().is_some() {
            SidebarSectionId::Pins
        } else {
            SidebarSectionId::Persona
        };
        state.frontend.scope_set_sidebar_section(section);

        // Save history position when restoring to Pins with existing cursor.
        if section == SidebarSectionId::Pins && !state.active_session().has_saved_history_position()
        {
            state.active_session_mut().save_history_position();
        }
    } else {
        // First entry - default to Persona at top.
        crate::feat::ui::sidebar::persona_section::receive_cursor(state, EnterFrom::Top);
    }

    IntentResult::empty()
}

/// Handles `SidebarLeave` - returns to the appropriate base scope.
///
/// Derives the return scope from current state, returning to Normal mode
/// when nothing else is active.
/// This ensures ESC always matches what's currently being rendered,
/// regardless of what was showing when the sidebar was opened.
pub fn handle_sidebar_leave(state: &mut AppState) -> IntentResult {
    // Always return to Normal when leaving sidebar.
    state.active_session_mut().discard_saved_history_position();
    state.active_session_mut().scroll_to_selected();
    state
        .frontend
        .scope_swap_base(crate::common::app_state::FocusScope::Normal);
    IntentResult::empty()
}

/// Handles `SidebarFocusSessions` \u{2014} jumps directly to the Sessions sidebar section.
///
/// If already in the sidebar, switches to Sessions section (clearing the
/// previous section's cursor and placing cursor on the first session).
/// If not in the sidebar, pushes `FocusScope::SidebarSessions` and
/// calls `receive_cursor`.
pub fn handle_sidebar_focus_sessions(state: &mut AppState) -> IntentResult {
    use crate::common::app_state::FocusScope;
    use crate::feat::ui::sidebar::section_trait::{EnterFrom, SidebarSectionId};

    if state.frontend.is_sidebar() {
        // Already in sidebar \u{2014} switch section directly.
        let current_section = state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona);

        if current_section == SidebarSectionId::Sessions {
            return IntentResult::empty();
        }

        // Clear cursor on the section we're leaving.
        crate::feat::ui::sidebar::sidebar::clear_cursor(current_section, state);

        // Restore history position when leaving Pins.
        if current_section == SidebarSectionId::Pins {
            state.active_session_mut().restore_history_position();
        }

        // Switch to sessions.
        state
            .frontend
            .scope_set_sidebar_section(SidebarSectionId::Sessions);
        crate::feat::ui::sidebar::sessions::navigate::receive_cursor(state, EnterFrom::Top);
    } else {
        // Not in sidebar \u{2014} enter sidebar directly on Sessions.
        state.frontend.scope_push(FocusScope::SidebarSessions);
        crate::feat::ui::sidebar::sessions::navigate::receive_cursor(state, EnterFrom::Top);
    }

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
    fn sidebar_focus_pushes_sidebar_scope() {
        // Given default app state.
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar focus.
        let result = handle_sidebar_focus(&mut state);

        // Then Sidebar is on the scope stack.
        assert!(state.frontend.is_sidebar());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sidebar_focus_defaults_to_persona() {
        // Given default app state.
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar focus.
        handle_sidebar_focus(&mut state);

        // Then persona section has the cursor.
        assert_eq!(state.frontend.persona_section.cursor, Some(0));
    }

    #[rstest::rstest]
    fn sidebar_focus_preserves_existing_cursor() {
        // Given a state with a pre-existing pins selection.
        use crate::protocol::{ChatEntry, PinPosition};

        let mut state = AppState::default_with_scope_focus();
        let entry = ChatEntry::user("test");
        let id = entry.id.clone();
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().pin_entry(&id, PinPosition::Top);
        state.frontend.pins.select_by_id(id);

        // When handling sidebar focus.
        handle_sidebar_focus(&mut state);

        // Then pins selection is preserved (not reset to persona).
        assert!(state.frontend.pins.selected_id().is_some());
        // And persona does NOT have cursor.
        assert!(state.frontend.persona_section.cursor.is_none());
    }

    #[rstest::rstest]
    fn sidebar_leave_returns_to_normal_scope() {
        // Given a state with Sidebar pushed onto the scope stack.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::SidebarPersona);

        // When handling sidebar leave.
        let result = handle_sidebar_leave(&mut state);

        // Then scope is back to Normal.
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sidebar_leave_from_input_always_returns_to_normal() {
        // Given a state that entered sidebar from Input mode.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Input);
        state.frontend.scope_push(FocusScope::SidebarPersona);

        // When handling sidebar leave.
        handle_sidebar_leave(&mut state);

        // Then scope is Normal (not Input).
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn sidebar_leave_does_not_set_cancel_prompt_when_streaming() {
        // Given a state in Sidebar with an active stream.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::SidebarPersona);
        state.active_session_mut().begin_streaming();

        // When handling sidebar leave.
        let result = handle_sidebar_leave(&mut state);

        // Then no cancel prompt is set.
        assert!(!state.frontend.cancel_stream_prompt);
        assert!(result.message_names.is_empty());
        // And scope is back to Normal.
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn sidebar_focus_sessions_from_normal_enters_sessions_section() {
        // Given default app state (Normal scope).
        let mut state = AppState::default_with_scope_focus();

        // When handling sidebar focus sessions.
        let result = handle_sidebar_focus_sessions(&mut state);

        // Then scope is SidebarSessions.
        assert_eq!(state.frontend.scope(), FocusScope::SidebarSessions);
        // And sessions section has a cursor.
        assert!(state.frontend.sessions_section.selected_index.is_some());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sidebar_focus_sessions_from_input_enters_sessions_section() {
        // Given Input scope.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Input);

        // When handling sidebar focus sessions.
        handle_sidebar_focus_sessions(&mut state);

        // Then scope is SidebarSessions.
        assert_eq!(state.frontend.scope(), FocusScope::SidebarSessions);
    }

    #[rstest::rstest]
    fn sidebar_focus_sessions_from_sidebar_persona_jumps_to_sessions() {
        // Given SidebarPersona scope with persona cursor.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::SidebarPersona);
        state.frontend.persona_section.cursor = Some(0);

        // When handling sidebar focus sessions.
        handle_sidebar_focus_sessions(&mut state);

        // Then scope is SidebarSessions.
        assert_eq!(state.frontend.scope(), FocusScope::SidebarSessions);
        // And persona cursor is cleared.
        assert!(state.frontend.persona_section.cursor.is_none());
        // And sessions has cursor.
        assert!(state.frontend.sessions_section.selected_index.is_some());
    }

    #[rstest::rstest]
    fn sidebar_focus_sessions_already_on_sessions_is_noop() {
        // Given SidebarSessions scope with cursor at index 0.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::SidebarSessions);
        state.frontend.sessions_section.selected_index = Some(0);

        // When handling sidebar focus sessions.
        let result = handle_sidebar_focus_sessions(&mut state);

        // Then scope stays SidebarSessions.
        assert_eq!(state.frontend.scope(), FocusScope::SidebarSessions);
        // And cursor is unchanged.
        assert_eq!(state.frontend.sessions_section.selected_index, Some(0));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sidebar_focus_sessions_from_sidebar_pins_jumps_to_sessions() {
        // Given SidebarPins scope.
        use crate::protocol::{ChatEntry, PinPosition};
        let mut state = AppState::default_with_scope_focus();
        let entry = ChatEntry::user("test");
        let id = entry.id.clone();
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().pin_entry(&id, PinPosition::Top);
        state.frontend.pins.select_by_id(id);
        state.frontend.scope_push(FocusScope::SidebarPins);

        // When handling sidebar focus sessions.
        handle_sidebar_focus_sessions(&mut state);

        // Then scope is SidebarSessions.
        assert_eq!(state.frontend.scope(), FocusScope::SidebarSessions);
        // And sessions has a cursor.
        assert!(state.frontend.sessions_section.selected_index.is_some());
    }

    #[rstest::rstest]
    fn sidebar_leave_from_pins_sets_concrete_scroll_offset() {
        // Given a session with 10 entries, entry 2 pinned, viewport state populated.
        use crate::protocol::{ChatEntry, PinPosition};

        let mut state = AppState::default_with_scope_focus();
        for i in 0..10 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("entry {i}")));
        }
        let history = state.active_session().history();
        let pinned_id = history[2].id.clone();
        state
            .active_session_mut()
            .pin_entry(&pinned_id, PinPosition::Top);

        // Simulate renderer state: each entry is 1 line, viewport height 5.
        let ranges: Vec<(u16, u16)> = (0..10).map(|i| (i, i + 1)).collect();
        state.active_session().set_entry_line_ranges(ranges);
        state.active_session().set_viewport_height(5);
        state.active_session().set_blank_count(0);
        state.active_session().set_last_max_offset(5); // 10 lines - 5 viewport
        state.active_session().set_rendered_scroll_offset(5); // viewport at bottom

        // Enter sidebar pins - this saves history position and syncs cursor to pin.
        state.frontend.scope_push(FocusScope::SidebarPins);
        crate::feat::ui::sidebar::pins::pins_section::receive_cursor(
            &mut state,
            crate::feat::ui::sidebar::section_trait::EnterFrom::Top,
        );

        // Verify cursor is on the pinned entry (index 2).
        assert_eq!(state.active_session().selected_entry_index(), Some(2));

        // When handling sidebar leave.
        let _result = handle_sidebar_leave(&mut state);

        // Then scroll_offset is concrete (Some), not None (auto-scroll).
        assert!(
            state.active_session().scroll_offset().is_some(),
            "scroll_offset should be concrete after leaving sidebar from pins"
        );
    }
}
