//! Resume the session under the sidebar cursor without injecting a new user message.
//!
//! See `.plans/retry-continue/plan.md` for the dialectical background: this
//! intent exists for two scenarios — resuming after a rate-limited / errored
//! turn, and resuming a session that was rehydrated from disk after an app
//! kill. In both cases the model needs only the existing history; no new
//! `User` entry is required.

use crate::sections::sessions::state::{SessionEntryKind, sorted_open_sessions};
use jinn_domain::common::app_state::AppState;
use jinn_domain::feat::chat_input::protocol::command::EnqueueResumeTurn;
use jinn_domain::protocol::IntentResult;

/// Resume the session under the sidebar cursor.
///
/// Emits `Command::EnqueueResumeTurn` for the selected session. No new
/// `User` or `Assistant` entry is created — the session actor will push
/// a UI-only `System "↻ session resumed"` marker (excluded from the
/// assembled prompt by default) and re-fire `SendToLlmProvider` against
/// the existing history.
///
/// No-op when:
/// - the sidebar scope is not `Sessions`,
/// - no session is selected,
/// - the selection is not a Session.
///
/// The target session's phase (Idle/Sending/Streaming) is checked
/// downstream in `session_actor::handle_enqueue_resume_turn`; the
/// intent itself always emits the command and lets the actor decide
/// whether to dispatch or ignore.
pub fn handle_session_continue(state: &mut AppState) -> IntentResult {
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

    // Non-session entries are a no-op for resume.
    if !matches!(entry.kind, SessionEntryKind::Session) {
        return IntentResult::empty();
    }

    let session_id = entry.id.clone();

    IntentResult::new_message(EnqueueResumeTurn { session_id })
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
    use crate::sections::navigate_sidebar;
    use crate::sections::section_trait::SidebarIntent;
    use crate::sections::sessions::state::sorted_open_sessions;
    use jinn_domain::common::app_state::AppState;

    #[rstest::rstest]
    fn returns_enqueue_resume_command_for_selected_session() {
        // Given a state with two sessions, sidebar focused on sessions section.
        let mut state = AppState::default_with_scope_focus();
        // Create a second session.
        let second_id = jinn_domain::protocol::SessionId::new();
        let mut second_session = jinn_domain::feat::session::chat_session::ChatSessionState::new();
        second_session.set_session_id(second_id);
        state.session.insert(second_session);
        // Focus sidebar on sessions section.
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        // Navigate to select the second entry in the sorted list.
        navigate_sidebar(&SidebarIntent::MoveDown, &mut state);

        // Determine which session is at index 1 (the selected one).
        let sessions = sorted_open_sessions(&state);
        let _selected_id = sessions[1].id.clone();
        let active_id_before = state.session.active_session_id().clone();

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then an EnqueueResumeTurn command is returned.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("EnqueueResumeTurn"));
        // And the active session is unchanged.
        assert_eq!(state.session.active_session_id(), &active_id_before);
    }

    #[rstest::rstest]
    fn noop_when_no_session_selected() {
        // Given a state with sidebar focused but no selected session.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        // No selection set.
        assert!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index.is_none(), || true)
        );

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then no commands are returned.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn noop_when_not_in_sessions_section() {
        // Given a state with sidebar focused on a different section.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then no commands are returned.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn scope_stack_unchanged_after_continue() {
        // Given a state with sidebar focused on sessions section.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        navigate_sidebar(&SidebarIntent::MoveDown, &mut state);
        let scope_before = state.frontend.scope().clone();

        // When handling session continue.
        let _result = handle_session_continue(&mut state);

        // Then the scope stack is unchanged.
        assert_eq!(
            state.frontend.scope(),
            scope_before,
            "scope should not change after session continue"
        );
    }
}
