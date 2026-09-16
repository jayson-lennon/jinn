//! Cursor reconciliation after session removal.
//!
//! Single source of truth for what happens to the sidebar cursor and active
//! session after a session is removed from the map. All close/archive paths
//! must call [`reconcile_after_session_removal`] instead of doing their own
//! ad-hoc cursor or active-session logic.

use super::state::sorted_open_sessions_split;
use crate::common::app_state::AppState;

/// Reconcile sidebar cursor and active session after a session is removed.
///
/// Must be called AFTER the session has been removed from the SessionMap
/// (and a fresh session created if the map was empty).
///
/// - Clamps `selected_index` to `[0, sessions.len() - 1]`
/// - If the current `active_session_id` is no longer in the sorted list,
///   sets it to whichever session is now under the cursor
/// - Scrolls viewport to make the cursor visible
pub fn reconcile_after_session_removal(state: &mut AppState) {
    reconcile_split(&mut state.session, &mut state.frontend);
}

/// Split-borrow variant of [`reconcile_after_session_removal`] that takes the
/// two sub-structs separately, for use inside tcaps views.
pub fn reconcile_split(
    session: &mut crate::common::session_map::SessionMap,
    frontend: &mut crate::feat::ui::frontend_state::FrontendState,
) {
    let sessions = sorted_open_sessions_split(session, frontend);
    if sessions.is_empty() {
        frontend.update_sections(|s| s.sessions.selected_index = Some(0));
        return;
    }

    let current = frontend
        .with_sections(|s| s.sessions.selected_index, || None)
        .unwrap_or(0);
    let clamped = current.min(sessions.len() - 1);
    frontend.update_sections(|s| s.sessions.selected_index = Some(clamped));

    let active_id = session.active_session_id();
    let active_in_list = sessions.iter().any(|s| &s.id == active_id);
    if !active_in_list && let Some(s) = sessions.get(clamped) {
        session.set_active(s.id.clone());
    }
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
    use super::super::state::sorted_open_sessions;
    use super::*;
    use crate::feat::session::chat_session::ChatSessionState;

    /// Helper: build state with N sessions, activate a specific one, set cursor.
    fn state_with_sessions(
        count: usize,
        active_index: usize,
        cursor_index: usize,
    ) -> (AppState, Vec<crate::protocol::SessionId>) {
        let mut state = AppState::default_with_scope_focus();
        // Remove default session to control exact count.
        let default_id = state.session.active_session_id().clone();
        state.session.remove_without_replacement(&default_id);

        for _ in 0..count {
            state.session.insert(ChatSessionState::new());
        }

        let sorted = sorted_open_sessions(&state);
        state
            .session
            .set_active(sorted[active_index].id.clone())
            .then_some(())
            .expect("active_index is valid");
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(cursor_index));

        (state, sorted.into_iter().map(|s| s.id).collect())
    }

    #[rstest::rstest]
    fn closing_active_session_clamps_cursor_and_sets_active() {
        // Given 3 sessions with cursor at index 2, active session at index 2.
        let (mut state, sorted_ids) = state_with_sessions(3, 2, 2);

        // Simulate removing the active session (index 2).
        let closing_id = sorted_ids[2].clone();
        state.session.remove_without_replacement(&closing_id);

        // When reconciling.
        reconcile_after_session_removal(&mut state);

        // Then cursor is clamped to index 1.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(1)
        );
        // Then active session is now the one at index 1 in the sorted list.
        let sorted = sorted_open_sessions(&state);
        assert_eq!(state.session.active_session_id(), &sorted[1].id);
    }

    #[rstest::rstest]
    fn closing_active_session_at_index_0_keeps_cursor_at_0() {
        // Given 3 sessions with cursor at index 0, active session at index 0.
        let (mut state, sorted_ids) = state_with_sessions(3, 0, 0);

        // Simulate removing the active session (index 0).
        let closing_id = sorted_ids[0].clone();
        state.session.remove_without_replacement(&closing_id);

        // When reconciling.
        reconcile_after_session_removal(&mut state);

        // Then cursor stays at index 0.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(0)
        );
        // Then active session is now the one at index 0 in the new sorted list.
        let sorted = sorted_open_sessions(&state);
        assert_eq!(state.session.active_session_id(), &sorted[0].id);
    }

    #[rstest::rstest]
    fn closing_non_active_session_preserves_active() {
        // Given 3 sessions with cursor at index 2, active session at index 0.
        let (mut state, sorted_ids) = state_with_sessions(3, 0, 2);

        // Simulate removing the session at index 2 (non-active).
        let closing_id = sorted_ids[2].clone();
        state.session.remove_without_replacement(&closing_id);

        // When reconciling.
        reconcile_after_session_removal(&mut state);

        // Then cursor is clamped to index 1.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(1)
        );
        // And active session is unchanged.
        assert_eq!(state.session.active_session_id(), &sorted_ids[0]);
    }

    #[rstest::rstest]
    fn closing_last_session_leaves_cursor_at_zero() {
        // Given 1 session with cursor at index 0, active session at index 0.
        let (mut state, sorted_ids) = state_with_sessions(1, 0, 0);

        // Simulate removing the only session and creating a fresh one.
        let closing_id = sorted_ids[0].clone();
        state
            .session
            .remove_and_replace(&closing_id, ChatSessionState::new());
        let _fresh_id = state.session.active_session_id().clone();

        // When reconciling.
        reconcile_after_session_removal(&mut state);

        // Then cursor is at index 0.
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(0)
        );
        // And active session is the fresh session.
        let sorted = sorted_open_sessions(&state);
        assert_eq!(sorted.len(), 1);
        assert_eq!(state.session.active_session_id(), &sorted[0].id);
    }

    #[rstest::rstest]
    fn reconcile_does_not_panic_with_none_cursor() {
        // Given a state with 2 sessions but no cursor set.
        let mut state = AppState::default_with_scope_focus();
        let second = ChatSessionState::new();
        let _second_id = second.session_id().clone();
        state.session.insert(second);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = None);

        // When reconciling (selected_index is None).
        reconcile_after_session_removal(&mut state);

        // Then cursor defaults to 0 (unwrap_or(0)).
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None),
            Some(0)
        );
    }
}
