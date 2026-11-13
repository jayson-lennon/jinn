//! Remembers which row each sidebar cursor sits on, every rendered frame.
//!
//! Removing a session deletes the row the sessions cursor names, and an
//! identity cursor cannot recover a position from what is left: after the
//! removal there is no record of *where* the cursor was, only of what it was.
//! So the row is captured here, while the frame that shows it still exists, and
//! the removal reads it back.
//!
//! The same holds for the bots cursor, in the bots section's own
//! units — a row *index* into that list, not a screen row, since the section
//! draws two screen lines per bot.
//!
//! Capture runs unconditionally, on every frame, whichever scope holds focus.
//! A frame drawn in a full-width tab has no sidebar column at all, and a frame
//! drawn while the chat pane holds focus still knows where the sidebar's cursor
//! is; gating either of those away is how a capture ends up stale exactly when
//! it is needed. A frame in which the cursor names nothing resolvable leaves
//! the last good capture alone rather than overwriting it with nothing.
//!
//! Rows resolve through the same list builders the rest of the slice reads, so
//! a capture can never disagree with what the user is looking at.

use jinn_kernel::common::app_state::AppState;

use super::sessions::state::{sorted_open_sessions_split, visible_row_of};

/// Records where the sessions cursor is drawn, if it is drawn at all.
pub fn capture_sessions_cursor_row(state: &AppState) {
    let row = state.frontend.with_sections(
        |sections| {
            let id = sections.sessions.selected_id.clone()?;
            visible_row_of(state, &id)
        },
        || None,
    );
    if let Some(row) = row {
        state
            .frontend
            .update_sections(|sections| sections.sessions.last_cursor_row = Some(row));
    }
}

/// Records which bot the bots cursor is on, if it is on one.
pub fn capture_bot_cursor_row(state: &AppState) {
    let row = state.frontend.with_sections(
        |sections| {
            let id = sections.bot.selected_id.clone()?;
            super::bots_section::row_of(state, &id)
        },
        || None,
    );
    if let Some(row) = row {
        state
            .frontend
            .update_sections(|sections| sections.bot.last_cursor_row = Some(row));
    }
}

/// Records which member the library cursor is on, if it is on one.
pub fn capture_library_cursor_row(state: &AppState) {
    let row = state.frontend.with_sections(
        |sections| {
            let id = sections.library.selected_id.clone()?;
            super::library_section::row_of(state, &id)
        },
        || None,
    );
    if let Some(row) = row {
        state
            .frontend
            .update_sections(|sections| sections.library.last_cursor_row = Some(row));
    }
}

/// The row the sessions cursor would return to after a removal.
///
/// Prefers a captured row, and clamps it into the list that exists now. With
/// nothing captured there is no position to restore, so the first row is the
/// only thing that can be said.
#[must_use]
pub fn restored_sessions_row(state: &AppState) -> usize {
    restored_sessions_row_split(&state.session, &state.frontend)
}

/// Split-borrow variant of [`restored_sessions_row`], for the removal path,
/// which holds the map and the frontend state mutably at once.
#[must_use]
pub fn restored_sessions_row_split(
    session: &jinn_session_state::SessionMap,
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
) -> usize {
    let len = sorted_open_sessions_split(session, frontend).len();
    if len == 0 {
        return 0;
    }
    frontend
        .with_sections(|sections| sections.sessions.last_cursor_row, || None)
        .unwrap_or(0)
        .min(len - 1)
}

/// The bot row the bots cursor would return to after a removal.
#[must_use]
pub fn restored_bot_row(state: &AppState) -> usize {
    restored_bot_row_split(&state.session, &state.frontend)
}

/// Split-borrow variant of [`restored_bot_row`].
#[must_use]
pub fn restored_bot_row_split(
    session: &jinn_session_state::SessionMap,
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
) -> usize {
    let len = super::bots_section::row_count_split(session, frontend);
    if len == 0 {
        return 0;
    }
    frontend
        .with_sections(|sections| sections.bot.last_cursor_row, || None)
        .unwrap_or(0)
        .min(len - 1)
}

#[cfg(test)]
mod tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use crate::sections::testing::state_with_titled_sessions;
    use jinn_kernel::common::app_state::AppState;
    use jinn_session_state::ChatSessionState;

    use super::super::sessions::state::sorted_open_sessions;
    use super::{
        capture_bot_cursor_row, capture_sessions_cursor_row, restored_bot_row,
        restored_sessions_row,
    };

    /// State holding `count` listed sessions, with no cursor on any of them.
    /// The row the sessions cursor sits on, read through the production path.
    fn cursor_row(state: &AppState) -> Option<usize> {
        let id = state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None)?;
        sorted_open_sessions(state)
            .iter()
            .position(|entry| entry.id == id.clone())
    }

    /// Puts the cursor on `row`.
    fn place_cursor(state: &mut AppState, row: usize) {
        let id = sorted_open_sessions(state)
            .get(row)
            .map(|entry| entry.id.clone())
            .expect("the row under test exists in the list it indexes");
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(id));
    }

    /// State whose active session is a fresh parent holding `names` named
    /// bots, one each, in the order given.
    fn state_viewing_bots(names: &[&str]) -> AppState {
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        let mut state = AppState::default_with_scope_focus();
        let default_id = state.session.active_session_id().clone();
        state.session.remove_without_replacement(&default_id);
        state.session.insert(parent);
        state.session.set_active(parent_id.clone());
        for name in names {
            let mut bot = ChatSessionState::new_bot(state.session.get_unchecked(&parent_id), true);
            bot.set_title((*name).to_owned());
            state.session.insert(bot);
        }
        state
    }

    #[rstest::rstest]
    fn the_sessions_cursor_row_is_captured_from_the_row_it_is_drawn_on() {
        // Given a state with four listed sessions and the cursor on row 2.
        let mut state = state_with_titled_sessions(4);
        place_cursor(&mut state, 2);

        // When the frame captures the cursor's row.
        capture_sessions_cursor_row(&state);

        // Then the capture is the row the cursor is drawn on.
        let captured = state
            .frontend
            .with_sections(|s| s.sessions.last_cursor_row, || None);
        assert_eq!(captured, Some(2));
    }

    #[rstest::rstest]
    fn the_sessions_cursor_row_is_captured_while_another_scope_holds_focus() {
        // Given a state with four listed sessions, cursor on row 1, and the
        // chat pane holding focus rather than any sidebar section.
        let mut state = state_with_titled_sessions(4);
        place_cursor(&mut state, 1);
        state
            .frontend
            .scope_swap_base(jinn_slices::FocusScope::Normal);

        // When the frame captures the cursor's row.
        capture_sessions_cursor_row(&state);

        // Then the row is captured anyway — the sidebar's cursor position is
        // a fact about state, not about who is allowed to type.
        let captured = state
            .frontend
            .with_sections(|s| s.sessions.last_cursor_row, || None);
        assert_eq!(captured, Some(1));
    }

    #[rstest::rstest]
    fn an_unresolvable_cursor_leaves_the_previous_capture_alone() {
        // Given a state whose last capture was row 1, and a cursor naming a
        // session that is no longer listed.
        let mut state = state_with_titled_sessions(4);
        place_cursor(&mut state, 1);
        capture_sessions_cursor_row(&state);
        let stale = jinn_core_types::SessionId::new();
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(stale));

        // When another frame captures.
        capture_sessions_cursor_row(&state);

        // Then the previous row survives, because an unresolvable cursor
        // knows nothing — it must not erase what the last good frame knew.
        let captured = state
            .frontend
            .with_sections(|s| s.sessions.last_cursor_row, || None);
        assert_eq!(captured, Some(1));
    }

    #[rstest::rstest]
    fn a_restored_row_is_clamped_to_the_last_row_of_a_shorter_list() {
        // Given a state with three listed sessions whose cursor is captured on
        // row 2, and a list that has since shrunk to two.
        let mut state = state_with_titled_sessions(3);
        place_cursor(&mut state, 2);
        capture_sessions_cursor_row(&state);
        let remaining: Vec<_> = state.session.iter().map(|(id, _)| id.clone()).collect();
        for id in remaining {
            let _ = state.session.remove(&id);
            let mut survivor = ChatSessionState::new();
            survivor.set_title("survivor".to_owned());
            state.session.insert(survivor);
            if state.session.session_count() == 2 {
                break;
            }
        }

        // When the captured row is read back.
        let row = restored_sessions_row(&state);

        // Then it is clamped to the new last row rather than pointing past it.
        assert_eq!(row, sorted_open_sessions(&state).len() - 1);
    }

    #[rstest::rstest]
    fn a_capture_is_taken_when_no_section_holds_focus_and_the_cursor_still_moves() {
        // Given a state with three listed sessions and no sidebar focus at all,
        // so no document cursor row can be derived from the scope stack.
        let mut state = state_with_titled_sessions(3);
        place_cursor(&mut state, 2);
        state
            .frontend
            .scope_swap_base(jinn_slices::FocusScope::Normal);

        // When the frame captures, and the cursor then moves.
        capture_sessions_cursor_row(&state);
        place_cursor(&mut state, 0);

        // Then the capture is from before the move, proving the capture does
        // not depend on focus to run.
        let captured = state
            .frontend
            .with_sections(|s| s.sessions.last_cursor_row, || None);
        assert_eq!(captured, Some(2));
        assert_eq!(cursor_row(&state), Some(0));
    }

    #[rstest::rstest]
    fn the_bot_cursor_row_is_captured_from_the_bot_it_names() {
        // Given a parent holding bots "zulu" and "alpha", with the
        // cursor on the one named "zulu" — which sorts into row 1.
        let mut state = state_viewing_bots(&["zulu", "alpha"]);
        let zulu = state
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.title().map(str::to_owned)))
            .find(|(_, title)| title.as_deref() == Some("zulu"))
            .map(|(id, _)| id)
            .expect("the zulu bot is loaded");
        state
            .frontend
            .update_sections(|s| s.bot.selected_id = Some(zulu));

        // When the frame captures the cursor's row.
        capture_bot_cursor_row(&state);

        // Then the capture is that bot's row in the rendered order.
        let captured = state
            .frontend
            .with_sections(|s| s.bot.last_cursor_row, || None);
        assert_eq!(captured, Some(1));
    }

    #[rstest::rstest]
    fn a_restored_bot_row_is_clamped_to_the_last_one() {
        // Given a state whose captured bot row is 2 and whose list now
        // holds a single bot.
        let mut state = state_viewing_bots(&["only"]);
        state
            .frontend
            .update_sections(|s| s.bot.last_cursor_row = Some(2));

        // When the captured row is read back.
        let row = restored_bot_row(&state);

        // Then it is clamped to the only bot there is.
        assert_eq!(row, 0);
    }
}
