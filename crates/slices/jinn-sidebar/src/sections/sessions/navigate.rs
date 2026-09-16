//! Navigation within the sessions sidebar section.

use crate::sections::section_trait::{EnterFrom, SectionNavResult, SidebarIntent};
use crate::sections::sessions::state::sorted_open_sessions;
use jinn_domain::common::app_state::AppState;

use super::MAX_VISIBLE_SESSIONS;

/// Adjusts scroll offset to ensure the selected index is visible within the window.
///
/// If no index is selected, does nothing.
pub fn scroll_to_cursor(state: &mut AppState) {
    let total = sorted_open_sessions(state).len();
    scroll_to_cursor_split(&mut state.frontend, total);
}

/// Split-borrow variant of [`scroll_to_cursor`].
pub fn scroll_to_cursor_split(
    frontend: &mut jinn_domain::feat::ui::frontend_state::FrontendState,
    total: usize,
) {
    let Some(index) = frontend.with_sections(|s| s.sessions.selected_index, || None) else {
        return;
    };
    let visible = MAX_VISIBLE_SESSIONS.min(total);
    if visible == 0 {
        return;
    }
    frontend.update_sections(|s| {
        let offset = &mut s.sessions.scroll_offset;

        if index < *offset {
            *offset = index;
        } else if index >= *offset + visible {
            *offset = index - visible + 1;
        } else {
            // index is already visible; no scroll needed.
        }

        // Clamp offset so the window doesn't extend past the end of the list.
        // Without this, archiving sessions can leave the offset too large,
        // causing fewer entries to render than content_height reports and
        // the Sessions footer label to shift upward.
        let max_offset = total.saturating_sub(visible);
        s.sessions.scroll_offset = s.sessions.scroll_offset.min(max_offset);
    });
}

/// No-op: session preview removed with node-graph.
fn update_preview(_state: &mut AppState) {}

/// Navigate within the sessions section.
///
/// Moves the cursor within the sessions list.
/// Returns `Exhausted` when at a boundary or when the list is empty.
/// The cursor lands on all entries (sessions only).
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let sessions = sorted_open_sessions(state);
    if sessions.is_empty() {
        return SectionNavResult::Exhausted;
    }

    let result = match intent {
        SidebarIntent::MoveDown => {
            let current = state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None)
                .unwrap_or(0);
            let new_index = current.saturating_add(1);
            if new_index >= sessions.len() {
                return SectionNavResult::Exhausted;
            }
            state
                .frontend
                .update_sections(|s| s.sessions.selected_index = Some(new_index));
            SectionNavResult::Moved
        }
        SidebarIntent::MoveUp => {
            let current = state
                .frontend
                .with_sections(|s| s.sessions.selected_index, || None)
                .unwrap_or(0);
            if current == 0 {
                return SectionNavResult::Exhausted;
            }
            state
                .frontend
                .update_sections(|s| s.sessions.selected_index = Some(current - 1));
            SectionNavResult::Moved
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    };

    scroll_to_cursor(state);
    update_preview(state);
    result
}

/// Place the cursor on this section from a given direction.
///
/// Positions at the edge of the list: index 0 from top, last index from bottom.
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    let sessions = sorted_open_sessions(state);
    if sessions.is_empty() {
        return;
    }
    let index = match enter_from {
        EnterFrom::Top => 0,
        EnterFrom::Bottom => sessions.len() - 1,
    };
    state
        .frontend
        .update_sections(|s| s.sessions.selected_index = Some(index));
    scroll_to_cursor(state);
    update_preview(state);
}
