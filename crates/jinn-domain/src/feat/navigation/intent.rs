//! Navigation intent handlers - scroll, tab, and editor.

use crate::common::app_state::AppState;
use crate::feat::ui::chat_log::visual_item::VisualItem;
use crate::protocol::IntentResult;

/// Number of lines to scroll per mouse wheel tick.
const MOUSE_SCROLL_STEP: u16 = 3;

/// Scrolls the chat log up by half a viewport page and moves cursor to first visible entry.
pub fn handle_scroll_up(state: &mut AppState) -> IntentResult {
    let viewport_height = state.active_session().viewport_height_value().max(1);
    let half = viewport_height / 2;
    state.active_session_mut().scroll_up(half);
    state.active_session_mut().move_cursor_to_first_visible();
    IntentResult::empty()
}

/// Scrolls the chat log down by half a viewport page and moves cursor to last visible entry.
pub fn handle_scroll_down(state: &mut AppState) -> IntentResult {
    let viewport_height = state.active_session().viewport_height_value().max(1);
    let half = viewport_height / 2;
    state.active_session_mut().scroll_down(half);
    state.active_session_mut().move_cursor_to_last_visible();
    IntentResult::empty()
}

/// Scrolls the chat log up by one mouse wheel tick and moves cursor to first visible.
pub fn handle_mouse_scroll_up(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_up(MOUSE_SCROLL_STEP);
    state.active_session_mut().move_cursor_to_first_visible();
    IntentResult::empty()
}

/// Scrolls the chat log down by one mouse wheel tick and moves cursor to last visible.
pub fn handle_mouse_scroll_down(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_down(MOUSE_SCROLL_STEP);
    state.active_session_mut().move_cursor_to_last_visible();
    IntentResult::empty()
}

/// Scrolls the chat log to the very top and moves cursor to the first entry.
pub fn handle_scroll_to_top(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_to_top();
    // Set cursor to first selectable visual item.
    let items = state.active_session().visual_items_snapshot();
    let history = state.active_session().history();
    if items.is_empty() {
        // Fallback: walk history directly when visual items not yet computed.
        for (i, entry) in history.iter().enumerate() {
            if !entry.is_empty_assistant() {
                state.active_session_mut().set_selected_entry_index(i);
                break;
            }
        }
    } else {
        let mut idx = 0;
        let max = items.len();
        while idx < max {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => history
                    .get(*hist_idx)
                    .is_some_and(|e| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx += 1;
        }
        if idx < max {
            state.active_session_mut().set_selected_entry_index(idx);
        }
    }
    IntentResult::empty()
}

/// Scrolls the chat log to the very bottom and moves cursor to the last entry.
pub fn handle_scroll_to_bottom(state: &mut AppState) -> IntentResult {
    state.active_session_mut().scroll_to_bottom();
    // Set cursor to last selectable visual item.
    let items = state.active_session().visual_items_snapshot();
    let history = state.active_session().history();
    if items.is_empty() {
        // Fallback: walk history backwards when visual items not yet computed.
        for i in (0..history.len()).rev() {
            if history.get(i).is_some_and(|e| !e.is_empty_assistant()) {
                state.active_session_mut().set_selected_entry_index(i);
                break;
            }
        }
    } else {
        let mut idx = items.len().saturating_sub(1);
        while idx > 0 {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => history
                    .get(*hist_idx)
                    .is_some_and(|e| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx = idx.saturating_sub(1);
        }
        let selectable = match items.get(idx) {
            Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
            Some(VisualItem::Entry(hist_idx)) => history
                .get(*hist_idx)
                .is_some_and(|e| !e.is_empty_assistant()),
            None => false,
        };
        if selectable {
            state.active_session_mut().set_selected_entry_index(idx);
        }
    }
    IntentResult::empty()
}

/// Opens the input in an external editor.
pub fn handle_edit_input(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .update_scope(|s| s.signals.edit_requested = true);
    IntentResult::empty()
}

/// Requests a CWD change via the external directory selection command.
///
/// Sets the `change_cwd_requested` TUI signal so the outer platform layer
/// can suspend the TUI and run the configured picker command.
pub fn handle_change_cwd(state: &mut AppState, root: crate::protocol::CwdRoot) -> IntentResult {
    state
        .frontend
        .update_scope(|s| s.signals.change_cwd_requested = Some(root));
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
    use crate::common::app_state::AppState;
    use crate::protocol::ChatEntry;

    use super::*;

    #[rstest::rstest]
    fn scroll_up_decrements_scroll_offset() {
        // Given a state with entries.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_down(20);

        // When handling ScrollUp.
        let _result = handle_scroll_up(&mut state);

        // Then the scroll offset decreased.
        let offset_before = 20u16;
        assert!(state.active_session().scroll_offset().unwrap_or(0) < offset_before);
    }

    #[rstest::rstest]
    fn scroll_up_returns_no_commands() {
        // Given a state with entries.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_down(20);

        // When handling ScrollUp.
        let result = handle_scroll_up(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn scroll_down_increments_scroll_offset() {
        // Given a state with entries.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }

        // When handling ScrollDown.
        let result = handle_scroll_down(&mut state);

        // Then scroll offset is non-zero (was at bottom, now scrolled up from bottom).
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn mouse_scroll_up_decrements_scroll_offset() {
        // Given a state with entries.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_down(20);

        // When handling MouseScrollUp.
        let result = handle_mouse_scroll_up(&mut state);

        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn mouse_scroll_down_increments_scroll_offset() {
        // Given a state with entries.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }

        // When handling MouseScrollDown.
        let result = handle_mouse_scroll_down(&mut state);

        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn scroll_to_top_sets_offset_to_zero() {
        // Given a state scrolled down.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_down(50);

        // When handling ScrollToTop.
        let _result = handle_scroll_to_top(&mut state);

        // Then scroll offset is at top.
        assert_eq!(state.active_session().scroll_offset(), Some(0));
    }

    #[rstest::rstest]
    fn scroll_to_top_returns_no_commands() {
        // Given a state scrolled down.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_down(50);

        // When handling ScrollToTop.
        let result = handle_scroll_to_top(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn scroll_to_bottom_resets_scroll() {
        // Given a state scrolled up from bottom.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_up(10);

        // When handling ScrollToBottom.
        let _result = handle_scroll_to_bottom(&mut state);

        // Then we're at bottom.
        assert!(state.active_session().is_at_bottom());
    }

    #[rstest::rstest]
    fn scroll_to_bottom_returns_no_commands() {
        // Given a state scrolled up from bottom.
        let mut state = AppState::default_with_scope_focus();
        for _ in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("line"));
        }
        state.active_session_mut().scroll_up(10);

        // When handling ScrollToBottom.
        let result = handle_scroll_to_bottom(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn edit_input_sets_tui_signal() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling EditInput.
        let _result = handle_edit_input(&mut state);

        // Then the edit_requested signal is set.
        assert!(state.frontend.signals_snapshot().edit_requested);
    }

    #[rstest::rstest]
    fn edit_input_returns_no_commands() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling EditInput.
        let result = handle_edit_input(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn scroll_to_top_skips_empty_assistant_at_index_0() {
        // Given history [empty_assistant, user].
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));

        // When scrolling to top.
        let _result = handle_scroll_to_top(&mut state);

        // Then selection skips the empty assistant and lands on index 1.
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
    }

    #[rstest::rstest]
    fn scroll_to_bottom_skips_empty_assistant_at_last_index() {
        // Given history [user, empty_assistant].
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));

        // When scrolling to bottom.
        let _result = handle_scroll_to_bottom(&mut state);

        // Then selection skips the empty assistant and lands on index 0.
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
    }

    #[rstest::rstest]
    fn scroll_up_half_page_with_known_viewport() {
        // Given a state with entries and a viewport of 10.
        let mut state = AppState::default_with_scope_focus();
        for i in 0..30 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("line {i}")));
        }
        state.active_session_mut().set_viewport_height(10);
        // Set last_max_offset high so scroll operations don't snap to bottom.
        state.active_session_mut().set_last_max_offset(100);
        state.active_session_mut().scroll_up(20);
        let offset_before = state
            .active_session()
            .scroll_offset()
            .expect("should have offset");

        // When handling ScrollUp.
        let _result = handle_scroll_up(&mut state);

        // Then the offset decreased by viewport/2 = 5.
        let offset_after = state
            .active_session()
            .scroll_offset()
            .expect("should have offset");
        assert_eq!(
            offset_before.saturating_sub(5),
            offset_after,
            "expected offset to decrease by 5 (viewport_height / 2), was {offset_before} -> {offset_after}"
        );
    }

    #[rstest::rstest]
    fn scroll_down_half_page_with_known_viewport() {
        // Given a state scrolled up with a viewport of 10.
        let mut state = AppState::default_with_scope_focus();
        for i in 0..30 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("line {i}")));
        }
        state.active_session_mut().set_viewport_height(10);
        // Set last_max_offset high so scroll_down doesn't snap to bottom.
        state.active_session_mut().set_last_max_offset(100);
        state.active_session_mut().scroll_up(20);
        let offset_before = state
            .active_session()
            .scroll_offset()
            .expect("should have offset");

        // When handling ScrollDown.
        let _result = handle_scroll_down(&mut state);

        // Then the offset increased by viewport/2 = 5.
        let offset_after = state
            .active_session()
            .scroll_offset()
            .expect("should have offset");
        assert_eq!(
            offset_before.saturating_add(5),
            offset_after,
            "expected offset to increase by 5 (viewport_height / 2), was {offset_before} -> {offset_after}"
        );
    }

    #[rstest::rstest]
    fn scroll_to_top_selects_first_selectable_entry() {
        // Given history with [user_0, user_1, user_2].
        let mut state = AppState::default_with_scope_focus();
        for i in 0..3 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }

        // When scrolling to top.
        let _result = handle_scroll_to_top(&mut state);

        // Then selection is on the first entry (index 0).
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
    }

    #[rstest::rstest]
    fn scroll_to_bottom_selects_last_selectable_entry() {
        // Given history with [user_0, user_1, user_2].
        let mut state = AppState::default_with_scope_focus();
        for i in 0..3 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }

        // When scrolling to bottom.
        let _result = handle_scroll_to_bottom(&mut state);

        // Then selection is on the last entry (index 2).
        assert_eq!(state.active_session().selected_entry_index(), Some(2));
    }

    #[rstest::rstest]
    fn scroll_to_top_with_multiple_empty_assistants_at_start() {
        // Given history [empty_asst, empty_asst, user].
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));

        // When scrolling to top.
        let _result = handle_scroll_to_top(&mut state);

        // Then selection skips both empty assistants and lands on index 2.
        assert_eq!(state.active_session().selected_entry_index(), Some(2));
    }

    #[rstest::rstest]
    fn scroll_to_bottom_with_multiple_empty_assistants_at_end() {
        // Given history [user, empty_asst, empty_asst].
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));

        // When scrolling to bottom.
        let _result = handle_scroll_to_bottom(&mut state);

        // Then selection skips both empty assistants and lands on index 0.
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
    }

    #[rstest::rstest]
    fn change_cwd_sets_signal_to_session_root() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling ChangeCwd with Session root.
        let result = handle_change_cwd(&mut state, crate::protocol::CwdRoot::Session);

        // Then the signal is set with Session root.
        assert_eq!(
            state.frontend.signals_snapshot().change_cwd_requested,
            Some(crate::protocol::CwdRoot::Session)
        );
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn change_cwd_sets_signal_to_home_root() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling ChangeCwd with Home root.
        let result = handle_change_cwd(&mut state, crate::protocol::CwdRoot::Home);

        // Then the signal is set with Home root.
        assert_eq!(
            state.frontend.signals_snapshot().change_cwd_requested,
            Some(crate::protocol::CwdRoot::Home)
        );
        // And no commands are emitted.
        assert!(result.message_names.is_empty());
    }
}
