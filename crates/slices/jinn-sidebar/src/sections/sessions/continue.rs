//! Resume the session under the sidebar cursor without injecting a new user message.
//!
//! This intent exists for two scenarios: resuming after a rate-limited or errored
//! turn, and resuming a session that was rehydrated from disk after an app
//! kill. In both cases the model needs only the existing history; no new
//! `User` entry is required.

use crate::sections::sessions::bot_actions::selected_section_session;
use jinn_chat_input_msg::EnqueueResumeTurn;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::IntentResult;

/// Resume the session the focused section's cursor names.
///
/// Emits `Command::EnqueueResumeTurn` for the selected session. No new
/// `User` or `Assistant` entry is created — the session actor will push
/// a UI-only `System "↻ session resumed"` marker (excluded from the
/// assembled prompt by default) and re-fire `SendToLlmProvider` against
/// the existing history.
///
/// No-op when:
/// - the sidebar focus is not one of the sections that list a session
///   (sessions, bots, or library),
/// - no session is under that section's cursor,
/// - the cursor names a session not in memory — an archived library member
///   has no live phase to resume until `<enter>` loads it.
///
/// The target session's phase (Idle/Sending/Streaming) is checked
/// downstream in `session_actor::handle_enqueue_resume_turn`; the
/// intent itself always emits the command and lets the actor decide
/// whether to dispatch or ignore.
pub fn handle_session_continue(state: &mut AppState) -> IntentResult {
    let Some(session_id) = selected_section_session(state) else {
        return IntentResult::empty();
    };

    // The cursor names the session, but the map proves it is loaded: an
    // archived member's id is a name without a conversation.
    if state.session.get(&session_id).is_none() {
        return IntentResult::empty();
    }

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
    use jinn_kernel::common::app_state::AppState;

    #[rstest::rstest]
    fn returns_enqueue_resume_command_for_selected_session() {
        // Given a state with two sessions, sidebar focused on sessions section.
        let mut state = AppState::default_with_scope_focus();
        // Create a second session.
        let second_id = jinn_core_types::SessionId::new();
        let mut second_session = jinn_session_state::ChatSessionState::new();
        second_session.set_session_id(second_id);
        state.session.insert(second_session);
        // Focus sidebar on sessions section.
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        // Navigate to select the second entry in the sorted list.
        let _ = navigate_sidebar(
            &SidebarIntent::MoveDown,
            &mut state,
            jinn_slices::empty_config_layer(),
        );

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
                .with_sections(|s| s.sessions.selected_id.is_none(), || true)
        );

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then no commands are returned.
        assert_eq!(result.message_names, [] as [&str; 0]);
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
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn scope_stack_unchanged_after_continue() {
        // Given a state with sidebar focused on sessions section.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        let _ = navigate_sidebar(
            &SidebarIntent::MoveDown,
            &mut state,
            jinn_slices::empty_config_layer(),
        );
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

    #[rstest::rstest]
    fn continues_the_library_member_the_cursor_names() {
        // Given a live session in the library, the library section focused,
        // its cursor on that member — and the sessions cursor naming a
        // different session, so a resolver that fell back to the wrong
        // section's cursor is caught.
        let mut state = AppState::default_with_scope_focus();
        let mut member = jinn_session_state::ChatSessionState::new();
        member.set_in_library(true);
        let member_id = member.session_id().clone();
        state.session.insert(member);
        let active_id = state.session.active_session_id().clone();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Library.focus_scope());
        state.frontend.update_sections(|s| {
            s.library.selected_id = Some(member_id.clone());
            s.sessions.selected_id = Some(active_id);
        });

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then the resume command names the member.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("EnqueueResumeTurn"));
    }

    #[rstest::rstest]
    fn continues_the_bot_the_bots_cursor_names() {
        // Given the bots section focused with its cursor on a bot.
        let mut state = AppState::default_with_scope_focus();
        let parent = jinn_session_state::ChatSessionState::new();
        state.session.insert(parent);
        let bot = {
            let parent_ref = state.session.active_session().clone();
            jinn_session_state::ChatSessionState::new_bot(&parent_ref, true)
        };
        let bot_id = bot.session_id().clone();
        state.session.insert(bot);
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Bot.focus_scope());
        state
            .frontend
            .update_sections(|s| s.bot.selected_id = Some(bot_id.clone()));

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then the resume command names the bot.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("EnqueueResumeTurn"));
    }

    #[rstest::rstest]
    fn an_archived_library_member_is_not_resumed() {
        // Given the library focused with its cursor naming a member that is
        // not in memory.
        let ghost = jinn_core_types::SessionId::new();
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Library.focus_scope());
        state
            .frontend
            .update_sections(|s| s.library.selected_id = Some(ghost));

        // When handling session continue.
        let result = handle_session_continue(&mut state);

        // Then nothing is dispatched: there is no live phase to resume until
        // the member is loaded with `<enter>`.
        assert!(
            result.message_names.is_empty(),
            "an archived member has no phase to resume"
        );
    }
}
