#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::common::app_state::{AppState, FocusScope};
use crate::common::render_ctx::RenderCtx;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::ui::sidebar::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use crate::feat::ui::sidebar::sessions::{
    SessionCloseError, SessionsSection, handle_session_activate, handle_session_close, navigate,
    receive_cursor, scroll_to_cursor, sorted_open_sessions, validate_session_close,
};

use crate::protocol::ChatEntry;
use ratatui::style::Color;

/// Helper: get a session title from the live session map via a tree entry.
fn entry_title(state: &AppState, id: &crate::protocol::SessionId) -> String {
    state
        .session
        .get(id)
        .map(|s| s.title().unwrap_or("Untitled Session").to_owned())
        .unwrap_or_default()
}

/// Helper: get a session's created_at from the live session map.
fn entry_created_at(state: &AppState, id: &crate::protocol::SessionId) -> jiff::Timestamp {
    state
        .session
        .get(id)
        .map(|s| *s.created_at())
        .unwrap_or_default()
}

/// Helper: check if the session's last entry is an error.
fn entry_last_is_error(state: &AppState, id: &crate::protocol::SessionId) -> bool {
    state.session.get(id).is_some_and(|s| {
        s.history()
            .last()
            .is_some_and(|e| matches!(&e.kind, crate::protocol::ChatEntryKind::Error(..)))
    })
}

// Helper: create state with N sessions.
fn state_with_sessions(count: usize) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    // Default state already has 1 session. Add more as needed.
    for i in 1..count {
        let session = ChatSessionState::new();
        let _id = session.session_id().clone();
        // Give each additional session a title.
        state.session.insert({
            let mut s = ChatSessionState::new();
            s.push_entry(ChatEntry::user(format!("message for session {i}")));
            s
        });
    }
    state
}

#[rstest::rstest]
fn section_id_is_sessions() {
    let section = SessionsSection::new();
    assert_eq!(section.id(), SidebarSectionId::Sessions);
}

#[rstest::rstest]
fn content_height_with_one_session() {
    let section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();
    assert_eq!(
        {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            section.content_height(&RenderCtx::new(&state, &slices, &overlay_views))
        },
        2
    ); // 1 session + footer
}

#[rstest::rstest]
fn content_height_with_three_sessions() {
    let section = SessionsSection::new();
    let state = state_with_sessions(3);
    assert_eq!(
        {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            section.content_height(&RenderCtx::new(&state, &slices, &overlay_views))
        },
        4
    ); // 3 sessions + footer
}

#[rstest::rstest]
fn content_height_capped_at_max_visible() {
    // Given state with 20 sessions (more than MAX_VISIBLE_SESSIONS = 15).
    let section = SessionsSection::new();
    let state = state_with_sessions(20);

    // When computing content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

    // Then it is capped at 15 + 1 = 16, not 20 + 1 = 21.
    assert_eq!(height, 16);
}
#[rstest::rstest]
fn navigate_down_moves_cursor_without_switching() {
    // Given state with 3 sessions, cursor at index 0.
    let mut state = state_with_sessions(3);
    let original_active = state.session.active_session_id().clone();
    state.frontend.sessions_section.selected_index = Some(0);

    // When navigating down.
    let result = navigate(&SidebarIntent::MoveDown, &mut state);

    // Then the result is Moved.
    assert_eq!(result, SectionNavResult::Moved);
    // And the cursor moved to index 1.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(1));
    // And the active session did NOT change.
    assert_eq!(*state.session.active_session_id(), original_active);
}

#[rstest::rstest]
fn navigate_up_moves_cursor_without_switching() {
    // Given state with 3 sessions, cursor at index 2.
    let mut state = state_with_sessions(3);
    let sessions = sorted_open_sessions(&state);
    state.session.set_active(sessions[2].id.clone());
    state.frontend.sessions_section.selected_index = Some(2);
    let original_active = state.session.active_session_id().clone();

    // When navigating up.
    let result = navigate(&SidebarIntent::MoveUp, &mut state);

    // Then the result is Moved.
    assert_eq!(result, SectionNavResult::Moved);
    // And the cursor moved to index 1.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(1));
    // And the active session did NOT change.
    assert_eq!(*state.session.active_session_id(), original_active);
}

#[rstest::rstest]
fn navigate_down_at_bottom_returns_exhausted() {
    // Given state with 2 sessions, cursor at last index.
    let mut state = state_with_sessions(2);
    let sessions = sorted_open_sessions(&state);
    state.frontend.sessions_section.selected_index = Some(sessions.len() - 1);

    // When navigating down.
    let result = navigate(&SidebarIntent::MoveDown, &mut state);

    // Then the result is Exhausted.
    assert_eq!(result, SectionNavResult::Exhausted);
}

#[rstest::rstest]
fn navigate_up_at_top_returns_exhausted() {
    // Given state with 2 sessions, cursor at index 0.
    let mut state = state_with_sessions(2);
    state.frontend.sessions_section.selected_index = Some(0);

    // When navigating up.
    let result = navigate(&SidebarIntent::MoveUp, &mut state);

    // Then the result is Exhausted.
    assert_eq!(result, SectionNavResult::Exhausted);
}

#[rstest::rstest]
fn navigate_action_returns_moved() {
    let mut state = AppState::default_with_scope_focus();
    let result = navigate(&SidebarIntent::Action(crate::Intent::Quit), &mut state);
    assert_eq!(result, SectionNavResult::Moved);
}

#[rstest::rstest]
fn scroll_to_cursor_adjusts_offset_when_cursor_above_window() {
    // Given 20 sessions with scroll_offset at 5, cursor at index 3.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 5;
    state.frontend.sessions_section.selected_index = Some(3);

    // When scrolling to cursor.
    scroll_to_cursor(&mut state);

    // Then scroll_offset moves to 3.
    assert_eq!(state.frontend.sessions_section.scroll_offset, 3);
}

#[rstest::rstest]
fn scroll_to_cursor_adjusts_offset_when_cursor_below_window() {
    // Given 20 sessions with scroll_offset at 0, cursor at index 18.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 0;
    state.frontend.sessions_section.selected_index = Some(18);

    // When scrolling to cursor.
    scroll_to_cursor(&mut state);

    // Then scroll_offset moves to 18 - 15 + 1 = 4.
    assert_eq!(state.frontend.sessions_section.scroll_offset, 4);
}

#[rstest::rstest]
fn scroll_to_cursor_noop_when_cursor_visible() {
    // Given 20 sessions with scroll_offset at 5, cursor at index 10.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 5;
    state.frontend.sessions_section.selected_index = Some(10);

    // When scrolling to cursor.
    scroll_to_cursor(&mut state);

    // Then scroll_offset stays at 5 (10 is within 5..20).
    assert_eq!(state.frontend.sessions_section.scroll_offset, 5);
}

#[rstest::rstest]
fn scroll_to_cursor_noop_when_no_selection() {
    // Given 20 sessions with no selection.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 5;
    state.frontend.sessions_section.selected_index = None;

    // When scrolling to cursor.
    scroll_to_cursor(&mut state);

    // Then scroll_offset stays at 5.
    assert_eq!(state.frontend.sessions_section.scroll_offset, 5);
}

#[rstest::rstest]
fn scroll_to_cursor_clamps_offset_when_list_shrinks() {
    // Given 20 sessions with scroll_offset at 5, cursor at index 10.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 5;
    state.frontend.sessions_section.selected_index = Some(10);

    // Remove 15 sessions, leaving only 5.
    let sorted = sorted_open_sessions(&state);
    for entry in &sorted[5..] {
        state.session.remove_without_replacement(&entry.id);
    }
    // Cursor is now clamped to index 4 by the caller (reconcile).
    state.frontend.sessions_section.selected_index = Some(4);

    // When scrolling to cursor.
    scroll_to_cursor(&mut state);

    // Then scroll_offset is clamped to 0 (5 sessions fit in MAX_VISIBLE_SESSIONS).
    assert_eq!(state.frontend.sessions_section.scroll_offset, 0);
}

#[rstest::rstest]
fn navigate_down_scrolls_viewport_at_bottom() {
    // Given 20 sessions, scroll_offset at 0, cursor at index 14 (last visible).
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 0;
    state.frontend.sessions_section.selected_index = Some(14);

    // When navigating down to index 15.
    navigate(&SidebarIntent::MoveDown, &mut state);

    // Then cursor is at 15 and scroll_offset moved to 1.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(15));
    assert_eq!(state.frontend.sessions_section.scroll_offset, 1);
}

#[rstest::rstest]
fn navigate_up_scrolls_viewport_at_top() {
    // Given 20 sessions, scroll_offset at 5, cursor at index 5.
    let mut state = state_with_sessions(20);
    state.frontend.sessions_section.scroll_offset = 5;
    state.frontend.sessions_section.selected_index = Some(5);

    // When navigating up to index 4.
    navigate(&SidebarIntent::MoveUp, &mut state);

    // Then cursor is at 4 and scroll_offset moved to 4.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(4));
    assert_eq!(state.frontend.sessions_section.scroll_offset, 4);
}

#[rstest::rstest]
fn receive_cursor_from_top_positions_at_index_zero() {
    // Given state with 3 sessions.
    let mut state = state_with_sessions(3);

    // When receiving cursor from top.
    receive_cursor(&mut state, EnterFrom::Top);

    // Then the selected index is 0.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(0));
}

#[rstest::rstest]
fn receive_cursor_from_bottom_positions_at_last_index() {
    // Given state with 3 sessions.
    let mut state = state_with_sessions(3);
    let count = sorted_open_sessions(&state).len();

    // When receiving cursor from bottom.
    receive_cursor(&mut state, EnterFrom::Bottom);

    // Then the selected index is the last one.
    assert_eq!(
        state.frontend.sessions_section.selected_index,
        Some(count - 1)
    );
}

#[rstest::rstest]
fn receive_cursor_noop_when_empty() {
    // Given state with no sessions (manually clear default).
    let mut state = AppState::default_with_scope_focus();
    state.session.sessions_mut().clear();

    // When receiving cursor.
    receive_cursor(&mut state, EnterFrom::Top);

    // Then no index is selected.
    assert_eq!(state.frontend.sessions_section.selected_index, None);
}

#[rstest::rstest]
fn sorted_sessions_orders_by_created_at_descending() {
    let state = state_with_sessions(3);
    let sessions = sorted_open_sessions(&state);
    // Then sessions are sorted by created_at descending (newest first).
    // Read created_at from live sessions, not from the tree entry.
    assert_eq!(sessions.len(), 3);
    let a = state
        .session
        .get(&sessions[0].id)
        .map(|s| *s.created_at())
        .unwrap_or_default();
    let b = state
        .session
        .get(&sessions[1].id)
        .map(|s| *s.created_at())
        .unwrap_or_default();
    let c = state
        .session
        .get(&sessions[2].id)
        .map(|s| *s.created_at())
        .unwrap_or_default();
    assert!(a >= b);
    assert!(b >= c);
}

#[rstest::rstest]
fn sorted_sessions_count_matches_hashmap() {
    let state = state_with_sessions(4);
    assert_eq!(sorted_open_sessions(&state).len(), 4);
}

#[rstest::rstest]
fn busy_session_is_not_idle() {
    // Given a session that has active busy operations.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_busy();

    // When collecting sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the session entry is not idle (busy_count > 0 shows throbber).
    assert_eq!(sessions.len(), 1);
    assert!(
        !sessions[0].is_idle,
        "busy session should show throbber in sidebar"
    );
}

#[rstest::rstest]
fn idle_and_not_busy_is_idle() {
    // Given a session with no busy operations and phase Idle.
    let state = AppState::default_with_scope_focus();

    // When collecting sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the session entry is idle.
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].is_idle,
        "idle session with no busy ops should be idle"
    );
}

#[rstest::rstest]
fn working_complete_returns_to_idle() {
    // Given a session that was working but completed.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_busy();
    state.active_session_mut().complete_busy();

    // When collecting sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the session entry is idle again.
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].is_idle);
}

use jinn_testutil::setup_term;

fn render_rows(
    section: &mut SessionsSection,
    state: &AppState,
    width: u16,
    height: u16,
) -> Vec<String> {
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(" ", ratatui::buffer::Cell::symbol)
                })
                .collect()
        })
        .collect()
}

#[rstest::rstest]
fn render_shows_sessions_footer() {
    // Given a sessions section with default state.
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();

    // When rendering.
    let rows = render_rows(&mut section, &state, 30, 5);

    // Then the last row contains "Sessions" (footer at the bottom).
    let combined = rows.join("\n");
    assert!(
        combined.contains("Sessions"),
        "should contain 'Sessions' in footer, got: {combined}"
    );
}

#[rstest::rstest]
fn render_shows_active_indicator_on_active_session() {
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();
    let rows = render_rows(&mut section, &state, 30, 5);
    let combined = rows.join("\n");
    assert!(
        combined.contains("\u{25b8}"),
        "should contain active indicator, got: {combined}"
    );
}

#[rstest::rstest]
fn render_shows_untitled_for_session_without_title() {
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();
    let rows = render_rows(&mut section, &state, 30, 5);
    let combined = rows.join("\n");
    assert!(
        combined.contains("Untitled Session"),
        "should contain 'Untitled Session', got: {combined}"
    );
}

#[rstest::rstest]
fn render_shows_down_arrow_when_entries_hidden_below() {
    // Given 20 sessions with scroll_offset at 0 (15 visible, 5 hidden below).
    let mut section = SessionsSection::new();
    let state = {
        let mut s = state_with_sessions(20);
        s.frontend.sessions_section.scroll_offset = 0;
        s
    };
    // content_height = 3 + 15 = 18, but we'll render in a taller area to be safe.
    let rows = render_rows(&mut section, &state, 30, 20);

    // Then the ↓ indicator appears on the last visible entry row.
    // Row layout: 0..14=entries (15), 15=footer.
    // Last entry row is row 14 (index 14 in visible window).
    let last_entry_row = &rows[14];
    assert!(
        last_entry_row.contains("\u{2193}"),
        "last entry row should contain ↓, got: {last_entry_row}"
    );
}

#[rstest::rstest]
fn render_shows_up_arrow_when_entries_hidden_above() {
    // Given 20 sessions with scroll_offset at 5 (15 visible, 5 hidden above).
    let mut section = SessionsSection::new();
    let state = {
        let mut s = state_with_sessions(20);
        s.frontend.sessions_section.scroll_offset = 5;
        s
    };
    let rows = render_rows(&mut section, &state, 30, 20);

    // Then the ↑ indicator appears on the first visible entry row (row 0).
    let first_entry_row = &rows[0];
    assert!(
        first_entry_row.contains("\u{2191}"),
        "first entry row should contain ↑, got: {first_entry_row}"
    );
}

#[rstest::rstest]
fn render_shows_both_arrows_when_viewport_in_middle() {
    // Given 20 sessions with scroll_offset at 3 (3 hidden above, 2 hidden below).
    let mut section = SessionsSection::new();
    let state = {
        let mut s = state_with_sessions(20);
        s.frontend.sessions_section.scroll_offset = 3;
        s
    };
    let rows = render_rows(&mut section, &state, 30, 20);

    // Then both indicators appear.
    let first_entry_row = &rows[0];
    let last_entry_row = &rows[14];
    assert!(
        first_entry_row.contains("\u{2191}"),
        "first entry row should contain ↑, got: {first_entry_row}"
    );
    assert!(
        last_entry_row.contains("\u{2193}"),
        "last entry row should contain ↓, got: {last_entry_row}"
    );
}

#[rstest::rstest]
fn render_no_arrows_when_all_entries_visible() {
    // Given 5 sessions (fewer than MAX_VISIBLE_SESSIONS).
    let mut section = SessionsSection::new();
    let state = state_with_sessions(5);
    let rows = render_rows(&mut section, &state, 30, 10);

    // Then no arrow indicators appear on entry rows.
    let combined = rows.join("");
    assert!(
        !combined.contains("\u{2191}") && !combined.contains("\u{2193}"),
        "should not contain scroll indicators, got: {combined}"
    );
}

#[rstest::rstest]
fn render_arrow_has_inverted_colors() {
    // Given 20 sessions with scroll_offset at 0 (↓ indicator visible).
    let mut section = SessionsSection::new();
    let state = {
        let mut s = state_with_sessions(20);
        s.frontend.sessions_section.scroll_offset = 0;
        s
    };
    let (mut terminal, area) = setup_term(30, 20);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the ↓ indicator on row 14 has fg=Black, bg=LightGreen.
    // Row layout: 0..14=entries (15), 15=footer.
    let buffer = terminal.backend().buffer();
    let arrow_cell = buffer.cell((29, 14)).expect("cell should exist");
    assert_eq!(arrow_cell.symbol(), "\u{2193}");
    assert_eq!(arrow_cell.style().fg, Some(Color::Black));
    assert_eq!(arrow_cell.style().bg, Some(Color::LightGreen));
}

#[rstest::rstest]
fn render_footer_uses_focus_accent_when_sidebar_focused() {
    // Given a sessions section with sidebar focused.
    let mut section = SessionsSection::new();
    let state = {
        let s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::SidebarSessions);
        s
    };

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the footer box-drawing corner (╰) at column 0, row 1 has focus_accent color.
    let buffer = terminal.backend().buffer();
    let corner_cell = buffer.cell((0, 1)).expect("corner cell should exist");
    assert_eq!(corner_cell.symbol(), "\u{2570}");
    assert_eq!(
        corner_cell.style().fg,
        Some(state.frontend.theme.focus_accent)
    );
}

#[rstest::rstest]
fn render_footer_uses_border_unfocused_when_sidebar_not_focused() {
    // Given a sessions section with default state (no sidebar focus).
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the footer box-drawing corner (╰) at column 0, row 1 has border_unfocused color.
    let buffer = terminal.backend().buffer();
    let corner_cell = buffer.cell((0, 1)).expect("corner cell should exist");
    assert_eq!(corner_cell.symbol(), "\u{2570}");
    assert_eq!(
        corner_cell.style().fg,
        Some(state.frontend.theme.border_unfocused)
    );
}

#[rstest::rstest]
fn render_footer_uses_border_unfocused_when_other_sidebar_section_focused() {
    // Given a sessions section rendered while persona section is focused (not sessions).
    let mut section = SessionsSection::new();
    let state = {
        let s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::SidebarPersona);
        s
    };

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the footer box-drawing corner uses border_unfocused (not focus_accent).
    let buffer = terminal.backend().buffer();
    let corner_cell = buffer.cell((0, 1)).expect("corner cell should exist");
    assert_eq!(corner_cell.symbol(), "\u{2570}");
    assert_eq!(
        corner_cell.style().fg,
        Some(state.frontend.theme.border_unfocused)
    );
}

#[rstest::rstest]
fn close_session_switches_to_next() {
    // Given state with 3 sessions, sessions section focused, cursor at index 0 (active session).
    let mut state = state_with_sessions(3);
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    // Active session is at index 0 (sorted newest-first, default is oldest → last, but we
    // set active to index 0 explicitly to test active-session close).
    state.session.set_active(sessions[0].id.clone());
    let closing_id = sessions[0].id.clone();
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the active session.
    handle_session_close(&mut state);

    // Then the closed session is removed and active session changed.
    assert!(!state.session.contains(&closing_id));
    assert_eq!(state.session.session_count(), 2);
    assert_ne!(*state.session.active_session_id(), closing_id);
}

#[rstest::rstest]
fn close_non_active_session_keeps_active() {
    // Given state with 3 sessions, sessions section focused, cursor at index 1 (not active).
    let mut state = state_with_sessions(3);
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    // Active session is at index 0.
    state.session.set_active(sessions[0].id.clone());
    let active_id = state.session.active_session_id().clone();
    // Close session at index 1 (non-active).
    let closing_id = sessions[1].id.clone();
    state.frontend.sessions_section.selected_index = Some(1);

    // When closing the non-active session.
    handle_session_close(&mut state);

    // Then the closed session is removed.
    assert!(!state.session.contains(&closing_id));
    // And the active session did NOT change.
    assert_eq!(*state.session.active_session_id(), active_id);
}

#[rstest::rstest]
fn close_last_session_creates_new() {
    // Given state with 1 session, sessions section focused.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let original_id = state.session.active_session_id().clone();
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the session.
    handle_session_close(&mut state);

    // Then a new session is created.
    assert_eq!(state.session.session_count(), 1);
    assert_ne!(*state.session.active_session_id(), original_id);
    assert_eq!(state.frontend.sessions_section.selected_index, Some(0));
}

#[rstest::rstest]
fn close_last_session_seeds_new_session_reasoning_effort_from_global() {
    // Given a single session with a global default effort of High.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.app_state.reasoning_effort = Some(crate::ReasoningEffort::High);
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the last session (forces a replacement).
    handle_session_close(&mut state);

    // Then the replacement session is seeded with the global effort.
    assert_eq!(
        state.active_session().profile().reasoning_effort,
        Some(crate::ReasoningEffort::High),
        "replacement session should be seeded from the global default"
    );
}

#[rstest::rstest]
fn close_last_session_seeds_disabled_sets_from_preferences() {
    // Given a single session and preferences disabling a tool and skill.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.preferences.disabled_tools = ["bash"].iter().map(|s| (*s).to_owned()).collect();
    state.frontend.preferences.disabled_skills = ["phased-task-loop"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the last session (forces a replacement).
    handle_session_close(&mut state);

    // Then the replacement session carries both disablement sets.
    assert!(
        state.active_session().disabled_tools().contains("bash"),
        "replacement should seed disabled_tools from jinn.toml"
    );
    assert!(
        state
            .active_session()
            .disabled_skills()
            .contains("phased-task-loop"),
        "replacement should seed disabled_skills from jinn.toml"
    );
}

#[rstest::rstest]
fn close_last_session_with_auto_enable_returns_enablement_message() {
    // Given a single session and one auto-enabled server in preferences.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.preferences.mcp_server = [(
        "excalimate".to_owned(),
        crate::feat::mcp::McpServerConfig {
            command: Some("npx".to_owned()),
            auto_enable: true,
            ..Default::default()
        },
    )]
    .into_iter()
    .collect();
    let original_id = state.session.active_session_id().clone();
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the last session (forces a replacement).
    let result = handle_session_close(&mut state);

    // Then an McpEnablementChanged message is attached for the replacement.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("McpEnablementChanged"))
    );
    // And the replacement has the server enabled.
    assert_ne!(*state.session.active_session_id(), original_id);
    assert!(state.active_session().is_mcp_server_enabled("excalimate"));
}

#[rstest::rstest]
fn close_last_session_without_auto_enable_emits_no_enablement() {
    // Given a single session with no auto-enabled servers.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing the last session.
    let result = handle_session_close(&mut state);

    // Then no enablement message is emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("McpEnablementChanged"))
    );
}

#[rstest::rstest]
fn close_session_clamps_index() {
    // Given state with 3 sessions, sessions section focused, cursor at last index.
    let mut state = state_with_sessions(3);
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    state.session.set_active(sessions[2].id.clone());
    // Move cursor to index 2 (the active session, sorted to 0, so use index 0)
    state.frontend.sessions_section.selected_index = Some(0);

    // When closing.
    handle_session_close(&mut state);

    // Then index is clamped to valid range.
    let selected = state.frontend.sessions_section.selected_index;
    assert!(selected.is_some());
    assert!(selected.unwrap() < state.session.session_count());
}

#[rstest::rstest]
fn close_session_adjusts_scroll_offset() {
    // Given 20 sessions with scroll_offset at 10, sessions section focused, cursor at 10.
    let mut state = state_with_sessions(20);
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.scroll_offset = 10;
    state.frontend.sessions_section.selected_index = Some(10);

    // When closing the session at index 10.
    handle_session_close(&mut state);

    // Then scroll_offset is adjusted to keep the cursor visible.
    // After removal there are 19 sessions. The clamped index is 10.
    // scroll_to_cursor ensures index 10 is visible in a window of 15 from offset 10.
    assert_eq!(state.frontend.sessions_section.selected_index, Some(10));
    assert!(state.frontend.sessions_section.scroll_offset <= 10);
}

#[rstest::rstest]
fn close_session_rejected_when_streaming() {
    // Given state with a streaming session, sessions section focused.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = Some(0);
    state.active_session_mut().begin_streaming();

    // When validating close.
    let result = validate_session_close(&state);

    // Then validation fails with SessionBusy.
    assert_eq!(result, Err(SessionCloseError::SessionBusy));
}

#[rstest::rstest]
fn close_session_rejected_when_working_phase() {
    // Given state with a session in Working phase.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = Some(0);
    state.active_session_mut().begin_busy();

    // When validating close.
    let result = validate_session_close(&state);

    // Then validation fails with SessionBusy.
    assert_eq!(result, Err(SessionCloseError::SessionBusy));
}

#[rstest::rstest]
fn close_session_rejected_when_wrong_section() {
    // Given state with sessions section NOT focused.
    let state = AppState::default_with_scope_focus();

    // When validating close.
    let result = validate_session_close(&state);

    // Then validation fails with WrongSection.
    assert_eq!(result, Err(SessionCloseError::WrongSection));
}

#[rstest::rstest]
fn render_session_title_is_red_when_last_entry_is_error() {
    // Given a session whose last history entry is an error.
    let mut section = SessionsSection::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        // Push an error entry into the active (only) session.
        s.active_session_mut()
            .push_entry(ChatEntry::error("teardown failed"));
        s
    };

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the title text on row 0 (first entry row) has red foreground.
    let buffer = terminal.backend().buffer();
    // The title starts after indicator(1) + space(1) + prefix(2) = column 4.
    let title_cell = buffer.cell((4, 0)).expect("title cell should exist");
    assert_eq!(title_cell.style().fg, Some(Color::Red));
}

#[rstest::rstest]
fn render_session_title_is_normal_when_last_entry_is_not_error() {
    // Given a session whose last history entry is a user message (not error).
    let mut section = SessionsSection::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s
    };
    let primary_text = state.frontend.theme.primary_text;

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the title text on row 0 has the primary_text color (active session).
    let buffer = terminal.backend().buffer();
    let title_cell = buffer.cell((4, 0)).expect("title cell should exist");
    assert_eq!(title_cell.style().fg, Some(primary_text));
}

#[rstest::rstest]
fn sorted_sessions_reports_last_entry_is_error() {
    // Given a session whose last entry is an error.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .push_entry(ChatEntry::error("boom"));

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the entry has last_entry_is_error = true.
    assert!(entry_last_is_error(&state, &sessions[0].id));
}

#[rstest::rstest]
fn sorted_sessions_reports_last_entry_not_error() {
    // Given a session whose last entry is a user message.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .push_entry(ChatEntry::user("hello"));

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the entry has last_entry_is_error = false.
    assert!(!entry_last_is_error(&state, &sessions[0].id));
}

#[rstest::rstest]
fn sorted_sessions_empty_history_is_not_error() {
    // Given a session with no history entries.
    let state = AppState::default_with_scope_focus();

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the entry has last_entry_is_error = false.
    assert!(!entry_last_is_error(&state, &sessions[0].id));
}

#[rstest::rstest]
fn activate_switches_to_cursor_session() {
    // Given state with 3 sessions, sessions section focused, cursor at index 1.
    let mut state = state_with_sessions(3);
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    state.frontend.sessions_section.selected_index = Some(1);
    let target_id = sessions[1].id.clone();

    // When activating.
    handle_session_activate(&mut state);

    // Then the active session is the one at cursor.
    assert_eq!(*state.session.active_session_id(), target_id);
}

#[rstest::rstest]
fn activate_is_noop_when_not_sessions_section() {
    // Given state with persona section focused.
    let mut state = state_with_sessions(3);
    let original_active = state.session.active_session_id().clone();
    state.frontend.sessions_section.selected_index = Some(1);

    // When activating.
    handle_session_activate(&mut state);

    // Then active session is unchanged.
    assert_eq!(*state.session.active_session_id(), original_active);
}

#[rstest::rstest]
fn session_new_with_lifecycle_opens_picker_from_normal_mode() {
    // Given default app state (Normal mode).
    let mut state = AppState::default_with_scope_focus();

    // When handling the intent via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SessionNewWithLifecycle,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the picker scope is pushed with SessionLifecycle kind.
    assert!(state.frontend.is_picker());
    assert_eq!(
        state.frontend.picker_kind(),
        Some(crate::protocol::PickerKind::SessionLifecycle)
    );
    // And no commands emitted (lifecycle entries are loaded synchronously).
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn session_new_with_lifecycle_opens_picker_from_sidebar_sessions() {
    // Given sidebar focused on sessions section.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);

    // When handling the intent via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SessionNewWithLifecycle,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the picker scope is pushed with SessionLifecycle kind.
    assert!(state.frontend.is_picker());
    assert_eq!(
        state.frontend.picker_kind(),
        Some(crate::protocol::PickerKind::SessionLifecycle)
    );
    // And no commands emitted (lifecycle entries are loaded synchronously).
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_only_emits_run_session_teardown() {
    // Given a session with a lifecycle that has a teardown command.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.preferences.session_lifecycles.push(
        crate::feat::preferences_actor::user_preferences::SessionLifecycle {
            name: "fossil branch".to_owned(),
            description: None,
            setup: Some(
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                    "echo setup".to_owned(),
                ),
            ),
            teardown: Some(
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                    "cleanup.sh $1".to_owned(),
                ),
            ),
        },
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("fossil branch".to_owned()));
    state
        .active_session_mut()
        .set_lifecycle_args(vec!["my-branch".to_owned()]);
    state.frontend.sessions_section.selected_index = Some(0);
    state
        .frontend
        .scope_push(crate::common::app_state::FocusScope::SidebarSessions);

    // When handling SidebarSessionTeardown via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SidebarSessionTeardown,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then a RunSessionTeardown command is emitted with the rendered teardown command.
    assert_eq!(result.message_names.len(), 1);
    assert!(result.message_names[0].contains("RunSessionTeardown"));
}

#[rstest::rstest]
fn teardown_only_is_noop_without_lifecycle_teardown() {
    // Given a session with a lifecycle that has NO teardown command.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.preferences.session_lifecycles.push(
        crate::feat::preferences_actor::user_preferences::SessionLifecycle {
            name: "plain".to_owned(),
            description: None,
            setup: Some(
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                    "echo setup".to_owned(),
                ),
            ),
            teardown: None,
        },
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("plain".to_owned()));
    state.frontend.sessions_section.selected_index = Some(0);
    state
        .frontend
        .scope_push(crate::common::app_state::FocusScope::SidebarSessions);

    // When handling SidebarSessionTeardown via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SidebarSessionTeardown,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then no commands are emitted (no teardown command to run).
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_only_is_noop_when_session_busy() {
    // Given a session with a teardown command that is currently busy.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.preferences.session_lifecycles.push(
        crate::feat::preferences_actor::user_preferences::SessionLifecycle {
            name: "fossil branch".to_owned(),
            description: None,
            setup: Some(
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                    "echo setup".to_owned(),
                ),
            ),
            teardown: Some(
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                    "cleanup.sh $1".to_owned(),
                ),
            ),
        },
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("fossil branch".to_owned()));
    state
        .active_session_mut()
        .set_lifecycle_args(vec!["my-branch".to_owned()]);
    // Mark the session busy so close validation rejects it.
    state.active_session_mut().begin_busy();
    state.frontend.sessions_section.selected_index = Some(0);
    state
        .frontend
        .scope_push(crate::common::app_state::FocusScope::SidebarSessions);

    // When handling SidebarSessionTeardown via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SidebarSessionTeardown,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then no commands are emitted (validation gates on busy state).
    assert!(result.message_names.is_empty());
}

// ---------------------------------------------------------------------------
// Pure render helper tests
// ---------------------------------------------------------------------------

use crate::feat::ui::sidebar::sessions::render::entry_line::{
    arrow_span, entry_title_style, indicator_span,
};
use crate::feat::ui::sidebar::sessions::render::truncate::truncate_str;
use ratatui::style::Modifier;
use throbber_widgets_tui::ThrobberState;

fn default_theme() -> crate::feat::theme::Theme {
    AppState::default_with_scope_focus().frontend.theme
}

/// Minimal entry for exercising `entry_title_style` precedence rules.
fn style_entry(
    is_active: bool,
    last_entry_is_error: bool,
    is_subagent: bool,
) -> crate::feat::ui::sidebar::sessions::state::SessionEntry {
    crate::feat::ui::sidebar::sessions::state::SessionEntry {
        kind: crate::feat::ui::sidebar::sessions::state::SessionEntryKind::Session,
        id: crate::protocol::SessionId::new(),
        title: "Test".to_owned(),
        is_active,
        created_at: jiff::Timestamp::now(),
        is_idle: true,
        last_entry_is_error,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: false,
        is_subagent,
        has_live_term: false,
    }
}

#[rstest::rstest]
fn title_style_is_red_reversed_when_error_and_selected() {
    // Given an entry with error and selected.
    let theme = default_theme();
    let entry = style_entry(false, true, false);

    // When computing title style.
    let style = entry_title_style(&entry, true, &theme);

    // Then the style is red + reversed.
    assert_eq!(style.fg, Some(Color::Red));
    assert!(style.add_modifier.contains(Modifier::REVERSED));
}

#[rstest::rstest]
fn title_style_is_red_when_error_not_selected() {
    // Given an entry with error but not selected.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, true, false), false, &theme);

    // Then the style is red, no reversed.
    assert_eq!(style.fg, Some(Color::Red));
    assert!(!style.add_modifier.contains(Modifier::REVERSED));
}

#[rstest::rstest]
fn title_style_is_reversed_when_selected_no_error() {
    // Given a selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, false), true, &theme);

    // Then the style is reversed, no specific fg.
    assert!(style.add_modifier.contains(Modifier::REVERSED));
    assert_eq!(style.fg, None);
}

#[rstest::rstest]
fn title_style_is_primary_text_when_active_not_selected() {
    // Given an active, not selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(true, false, false), false, &theme);

    // Then the style has primary text fg.
    assert_eq!(style.fg, Some(theme.primary_text));
}

#[rstest::rstest]
fn title_style_is_muted_text_when_inactive_not_selected() {
    // Given an inactive, not selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, false), false, &theme);

    // Then the style has muted text fg.
    assert_eq!(style.fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn title_style_uses_subagent_fg_for_subagent_when_inactive_not_selected() {
    // Given an inactive, not selected subagent entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, true), false, &theme);

    // Then the style has the subagent fg.
    assert_eq!(style.fg, Some(theme.subagent_fg));
    // And it differs from a regular session's muted text.
    assert_ne!(style.fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn title_style_uses_subagent_fg_for_active_subagent_not_selected() {
    // Given an active, not selected subagent entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(true, false, true), false, &theme);

    // Then the style has the subagent fg rather than primary text.
    assert_eq!(style.fg, Some(theme.subagent_fg));
}

#[rstest::rstest]
fn title_style_stays_red_for_errored_subagent() {
    // Given an errored, not selected subagent entry.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, true, true), false, &theme);

    // Then error red outranks the subagent color.
    assert_eq!(style.fg, Some(Color::Red));
}

#[rstest::rstest]
fn indicator_span_returns_blank_space_when_idle() {
    // Given an idle entry.
    let throbber = ThrobberState::default();

    // When computing indicator span.
    let span = indicator_span(true, &throbber);

    // Then it is a blank space.
    assert_eq!(span.content, " ");
}

#[rstest::rstest]
fn indicator_span_returns_throbber_character_when_working() {
    // Given a working entry (not idle).
    let throbber = ThrobberState::default();

    // When computing indicator span.
    let span = indicator_span(false, &throbber);

    // Then it is a non-space character with Cyan fg.
    assert_ne!(span.content, " ");
    assert!(!span.content.is_empty());
    assert_eq!(span.style.fg, Some(Color::Cyan));
}

#[rstest::rstest]
fn arrow_span_returns_active_prefix_when_active() {
    // Given an active session.
    let theme = default_theme();

    // When computing arrow span.
    let span = arrow_span(true, &theme);

    // Then it contains the active prefix.
    assert_eq!(span.content, "\u{25b8} ");
}

#[rstest::rstest]
fn arrow_span_returns_inactive_prefix_when_not_active() {
    // Given an inactive session.
    let theme = default_theme();

    // When computing arrow span.
    let span = arrow_span(false, &theme);

    // Then it contains the inactive prefix (two spaces).
    assert_eq!(span.content, "  ");
}

#[rstest::rstest]
fn truncate_str_returns_original_when_short() {
    // Given a string that fits within max_len.
    let s = "hello";

    // When truncating with max_len = 10.
    let result = truncate_str(s, 10);

    // Then the original string is returned.
    assert_eq!(result, "hello");
}

#[rstest::rstest]
fn truncate_str_appends_ellipsis_when_long() {
    // Given a string that exceeds max_len.
    let s = "hello world";

    // When truncating with max_len = 5.
    let result = truncate_str(s, 5);

    // Then the result is 5 graphemes ending with ellipsis.
    assert_eq!(result, "hell\u{2026}");
}

#[rstest::rstest]
fn truncate_str_returns_empty_when_max_len_zero() {
    // Given max_len of zero.
    let s = "hello";

    // When truncating with max_len = 0.
    let result = truncate_str(s, 0);

    // Then an empty string is returned.
    assert_eq!(result, "");
}

// ---------------------------------------------------------------------------
// Tree integration tests
// ---------------------------------------------------------------------------

/// Helper: create a state with a known parent-child tree.
///
/// Creates:
/// - root_a (oldest root)
///   - child_a1 (oldest child of root_a)
///     - grandchild_a1a
///   - child_a2
/// - root_b (newest root)
fn state_with_tree() -> AppState {
    let mut state = AppState::default_with_scope_focus();

    // Create root_a with a title.
    let mut root_a = ChatSessionState::new();
    root_a.push_entry(ChatEntry::user("root a"));
    root_a.set_title("root a".to_owned());
    let root_a_id = root_a.session_id().clone();
    state.session.insert(root_a);

    // Create child_a1 under root_a.
    let mut child_a1 = ChatSessionState::new();
    child_a1.set_title("child a1".to_owned());
    child_a1.set_parent_session(root_a_id.clone());
    let child_a1_id = child_a1.session_id().clone();
    state.session.insert(child_a1);

    // Create grandchild_a1a under child_a1.
    let mut grandchild = ChatSessionState::new();
    grandchild.set_title("grandchild a1a".to_owned());
    grandchild.set_parent_session(child_a1_id);
    let _grandchild_id = grandchild.session_id().clone();
    state.session.insert(grandchild);

    // Create child_a2 under root_a.
    let mut child_a2 = ChatSessionState::new();
    child_a2.set_title("child a2".to_owned());
    child_a2.set_parent_session(root_a_id.clone());
    let _child_a2_id = child_a2.session_id().clone();
    state.session.insert(child_a2);

    // Create root_b with a title (newest root).
    let mut newest_root = ChatSessionState::new();
    newest_root.push_entry(ChatEntry::user("root b"));
    newest_root.set_title("root b".to_owned());
    let newest_root_id = newest_root.session_id().clone();
    state.session.insert(newest_root);

    // Remove the default session (created at AppState::default).
    let default_id = state.session.active_session_id().clone();
    if default_id != root_a_id && default_id != newest_root_id {
        state.session.remove(&default_id);
    }

    // Set active to root_b.
    state.session.set_active(newest_root_id);

    state
}

#[rstest::rstest]
fn tree_roots_sorted_by_created_at_descending() {
    // Given state with two root sessions.
    let state = state_with_tree();

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the roots appear first and are ordered newest-first.
    // root_b was created last, root_a was created first.
    let roots: Vec<_> = sessions.iter().filter(|s| s.depth == 0).collect();
    assert_eq!(roots.len(), 2, "should have 2 roots");
    assert!(
        entry_created_at(&state, &roots[0].id) >= entry_created_at(&state, &roots[1].id),
        "roots should be sorted newest-first"
    );
}

#[rstest::rstest]
fn tree_children_sorted_by_created_at_ascending_under_parent() {
    // Given state with root_a having two children.
    let state = state_with_tree();

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Find root_a's children (depth 1, parent is root_a).
    let root_a_id = sessions
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("root a"))
        .map(|s| s.id.clone())
        .expect("root a should exist");
    let children: Vec<_> = sessions
        .iter()
        .filter(|s| s.parent_id.as_ref() == Some(&root_a_id))
        .collect();

    // Then children are sorted oldest-first.
    assert_eq!(children.len(), 2, "root_a should have 2 children");
    assert!(
        entry_created_at(&state, &children[0].id) <= entry_created_at(&state, &children[1].id),
        "children should be sorted oldest-first"
    );
}

#[rstest::rstest]
fn tree_dfs_order_is_correct() {
    // Given state with root_a -> child_a1 -> grandchild_a1a, child_a2, root_b.
    let state = state_with_tree();

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then DFS order is: root_b (newest root), root_a, child_a1, grandchild_a1a, child_a2.
    // Or root_a first, depending on creation timing.
    // The invariant is: root_a appears before its children, child_a1 appears before grandchild_a1a.
    let root_a_pos = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    let child_a1_pos = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    let grandchild_pos = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("grandchild"))
        .expect("grandchild");
    let child_a2_pos = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a2"))
        .expect("child a2");

    assert!(root_a_pos < child_a1_pos, "root_a before child_a1");
    assert!(child_a1_pos < grandchild_pos, "child_a1 before grandchild");
    assert!(
        grandchild_pos < child_a2_pos,
        "grandchild before child_a2 (DFS)"
    );
}

#[rstest::rstest]
fn orphan_session_appears_as_root() {
    // Given a session with a parent that is not loaded.
    let mut state = AppState::default_with_scope_focus();
    let mut orphan = ChatSessionState::new();
    orphan.set_title("orphan".to_owned());
    orphan.set_parent_session(crate::protocol::SessionId::new());
    state.session.insert(orphan);

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the orphan appears as a root (depth 0).
    let orphan_entry = sessions
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("orphan"));
    assert!(orphan_entry.is_some(), "orphan should appear");
    assert_eq!(
        orphan_entry.unwrap().depth,
        0,
        "orphan should be treated as root"
    );
}

#[rstest::rstest]
fn navigate_down_from_root_goes_to_first_child() {
    // Given state with a tree and cursor on root_a.
    let mut state = state_with_tree();
    let sessions = sorted_open_sessions(&state);
    let root_a_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    state.frontend.sessions_section.selected_index = Some(root_a_index);

    // When navigating down.
    navigate(&SidebarIntent::MoveDown, &mut state);

    // Then the cursor is on the next entry (root_a's first child in DFS order).
    let new_sessions = sorted_open_sessions(&state);
    let new_index = state.frontend.sessions_section.selected_index.unwrap();
    assert_eq!(
        new_index,
        root_a_index + 1,
        "cursor should move to next DFS entry"
    );
    // And the entry is child_a1.
    assert!(
        entry_title(&state, &new_sessions[new_index].id).contains("child a1"),
        "next entry should be child_a1"
    );
}

#[rstest::rstest]
fn navigate_up_from_child_goes_to_parent() {
    // Given state with a tree and cursor on child_a1.
    let mut state = state_with_tree();
    let sessions = sorted_open_sessions(&state);
    let child_a1_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    state.frontend.sessions_section.selected_index = Some(child_a1_index);

    // When navigating up.
    navigate(&SidebarIntent::MoveUp, &mut state);

    // Then the cursor is on root_a (parent).
    let new_index = state.frontend.sessions_section.selected_index.unwrap();
    assert_eq!(
        new_index,
        child_a1_index - 1,
        "cursor should move to parent"
    );
    let new_sessions = sorted_open_sessions(&state);
    assert!(
        entry_title(&state, &new_sessions[new_index].id).contains("root a"),
        "previous entry should be root_a"
    );
}

#[rstest::rstest]
fn close_child_session_clamps_cursor() {
    // Given state with a tree, cursor on child_a1.
    let mut state = state_with_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let child_a1_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    let child_a1_id = sessions[child_a1_index].id.clone();
    state.frontend.sessions_section.selected_index = Some(child_a1_index);

    // When closing child_a1.
    handle_session_close(&mut state);

    // Then child_a1 is removed.
    assert!(!state.session.contains(&child_a1_id));
    // And the cursor is clamped to valid range.
    let remaining = sorted_open_sessions(&state);
    let selected = state.frontend.sessions_section.selected_index.unwrap();
    assert!(selected < remaining.len());
}

#[rstest::rstest]
fn close_root_session_promotes_children_to_roots() {
    // Given state with root_a having children.
    let mut state = state_with_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let root_a_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    let root_a_id = sessions[root_a_index].id.clone();
    state.frontend.sessions_section.selected_index = Some(root_a_index);

    // When closing root_a.
    handle_session_close(&mut state);

    // Then root_a is removed.
    assert!(!state.session.contains(&root_a_id));
    // And its former children are now orphans (roots in the new tree).
    let remaining = sorted_open_sessions(&state);
    let former_children: Vec<_> = remaining
        .iter()
        .filter(|s| {
            entry_title(&state, &s.id).contains("child a")
                || entry_title(&state, &s.id).contains("grandchild")
        })
        .collect();
    assert!(
        !former_children.is_empty(),
        "former children should still be present"
    );
    // All former children should now be roots or have adjusted depth.
    // The children of root_a become orphans → treated as roots.
    let child_a1_entry = remaining
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("child a1"));
    assert!(child_a1_entry.is_some(), "child_a1 should still exist");
    assert_eq!(
        child_a1_entry.unwrap().depth,
        0,
        "child_a1 should now be a root (orphan)"
    );
}

#[rstest::rstest]
fn activate_child_session_switches_active() {
    // Given state with a tree, cursor on child_a1.
    let mut state = state_with_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let child_a1_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    let child_a1_id = sessions[child_a1_index].id.clone();
    state.frontend.sessions_section.selected_index = Some(child_a1_index);

    // When activating.
    handle_session_activate(&mut state);

    // Then the active session is child_a1.
    assert_eq!(*state.session.active_session_id(), child_a1_id);
}

#[rstest::rstest]
fn render_tree_shows_tree_characters() {
    // Given state with a tree.
    let mut section = SessionsSection::new();
    let state = state_with_tree();

    // When rendering.
    let (mut terminal, area) = jinn_testutil::setup_term(30, 15);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the buffer contains tree connector characters.
    let buffer = terminal.backend().buffer();
    let text: String = (0..15)
        .flat_map(|y| {
            (0..30).map(move |x| {
                buffer
                    .cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
        })
        .collect();
    assert!(
        text.contains('├') || text.contains('└'),
        "rendered output should contain tree connectors, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// Visual reparenting tests
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn archiving_intermediate_parent_reparents_grandchild_under_grandparent() {
    // Given state with root_a -> child_a1 -> grandchild_a1a.
    let mut state = state_with_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let child_a1_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    let child_a1_id = sessions[child_a1_index].id.clone();
    let root_a_id = sessions
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("root a"))
        .map(|s| s.id.clone())
        .expect("root a");
    let grandchild_id = sessions
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("grandchild"))
        .map(|s| s.id.clone())
        .expect("grandchild");
    state.frontend.sessions_section.selected_index = Some(child_a1_index);

    // When closing child_a1 (the intermediate parent).
    handle_session_close(&mut state);

    // Then child_a1 is removed.
    assert!(!state.session.contains(&child_a1_id));
    // And the visual_parents index maps grandchild -> root_a.
    assert_eq!(
        state
            .frontend
            .sessions_section
            .visual_parents
            .get(&grandchild_id),
        Some(&root_a_id),
        "grandchild should be reparented to root_a in visual_parents"
    );
    // And sorted_open_sessions shows grandchild at depth 1 under root_a.
    let remaining = sorted_open_sessions(&state);
    let grandchild_entry = remaining
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("grandchild"))
        .expect("grandchild should exist");
    assert_eq!(
        grandchild_entry.depth, 1,
        "grandchild should be at depth 1 under root_a, got depth {}",
        grandchild_entry.depth
    );
    assert_eq!(
        grandchild_entry.parent_id,
        Some(root_a_id),
        "grandchild's effective parent should be root_a"
    );
}

#[rstest::rstest]
fn archiving_root_does_not_create_visual_parents_for_orphaned_children() {
    // Given state with root_a having children.
    let mut state = state_with_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let root_a_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    let _root_a_id = sessions[root_a_index].id.clone();
    state.frontend.sessions_section.selected_index = Some(root_a_index);

    // When closing root_a (no loaded ancestor to reparent to).
    handle_session_close(&mut state);

    // Then the visual_parents index should be empty (root has no loaded ancestor).
    assert!(
        state.frontend.sessions_section.visual_parents.is_empty(),
        "no visual_parents entries should exist when root is closed"
    );
}

#[rstest::rstest]
fn multi_level_intermediate_hiding_reparents_to_nearest_loaded_ancestor() {
    // Given a chain: root -> A -> B -> leaf.
    let mut state = AppState::default_with_scope_focus();

    let mut root = ChatSessionState::new();
    root.set_title("root".to_owned());
    let root_id = root.session_id().clone();
    state.session.insert(root);

    let mut a = ChatSessionState::new();
    a.set_title("session A".to_owned());
    a.set_parent_session(root_id.clone());
    let a_id = a.session_id().clone();
    state.session.insert(a);

    let mut b = ChatSessionState::new();
    b.set_title("session B".to_owned());
    b.set_parent_session(a_id.clone());
    let b_id = b.session_id().clone();
    state.session.insert(b);

    let mut leaf = ChatSessionState::new();
    leaf.set_title("leaf".to_owned());
    leaf.set_parent_session(b_id.clone());
    let leaf_id = leaf.session_id().clone();
    state.session.insert(leaf);

    // Remove default session.
    let default_id = state.session.active_session_id().clone();
    if default_id != root_id {
        state.session.remove(&default_id);
    }
    state.session.set_active(root_id.clone());

    // When archiving A.
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(&state);
    let a_index = sessions.iter().position(|s| s.id == a_id).expect("A");
    state.frontend.sessions_section.selected_index = Some(a_index);
    handle_session_close(&mut state);

    // Then B is reparented to root.
    assert_eq!(
        state.frontend.sessions_section.visual_parents.get(&b_id),
        Some(&root_id),
        "B should be reparented to root"
    );

    // When archiving B.
    let sessions = sorted_open_sessions(&state);
    let b_index = sessions.iter().position(|s| s.id == b_id).expect("B");
    state.frontend.sessions_section.selected_index = Some(b_index);
    handle_session_close(&mut state);

    // Then leaf is reparented to root (transitive via B visual parent).
    assert_eq!(
        state.frontend.sessions_section.visual_parents.get(&leaf_id),
        Some(&root_id),
        "leaf should be reparented to root (transitive)"
    );

    // And the sidebar tree shows leaf at depth 1 under root.
    let remaining = sorted_open_sessions(&state);
    let leaf_entry = remaining.iter().find(|s| s.id == leaf_id).expect("leaf");
    assert_eq!(
        leaf_entry.depth, 1,
        "leaf should be at depth 1 under root, got depth {}",
        leaf_entry.depth
    );
}

#[rstest::rstest]
fn sorted_sessions_last_root_is_marked_as_last_child() {
    // Given 3 root sessions (no parent-child relationships).
    let state = state_with_sessions(3);

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the last root is marked as is_last_child, others are not.
    assert_eq!(sessions.len(), 3, "should have 3 sessions");
    assert!(
        !sessions[0].is_last_child,
        "first root should not be last child"
    );
    assert!(
        !sessions[1].is_last_child,
        "second root should not be last child"
    );
    assert!(sessions[2].is_last_child, "last root should be last child");
}

#[rstest::rstest]
fn sorted_sessions_single_root_is_marked_as_last_child() {
    // Given a single root session.
    let state = state_with_sessions(1);

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Then it is marked as last child.
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].is_last_child,
        "single root should be last child"
    );
}

// --- dfs_children: is_last_child for non-root entries (kills == -> !=

#[rstest::rstest]
fn tree_children_last_child_flag_is_correct() {
    // Given state with root_a having two children.
    let state = state_with_tree();

    // When collecting sorted sessions.
    let sessions = sorted_open_sessions(&state);

    // Find root_a's children.
    let root_a_id = sessions
        .iter()
        .find(|s| entry_title(&state, &s.id).contains("root a"))
        .map(|s| s.id.clone())
        .expect("root a should exist");
    let children: Vec<_> = sessions
        .iter()
        .filter(|s| s.parent_id.as_ref() == Some(&root_a_id))
        .collect();

    assert_eq!(children.len(), 2, "root_a should have 2 children");
    assert!(
        !children[0].is_last_child,
        "first child should not be last child"
    );
    assert!(
        children[1].is_last_child,
        "second child should be last child"
    );
}

#[rstest::rstest]
fn update_visual_parents_on_removal_reparents_only_children_of_removed_session() {
    // Given a chain: root -> A -> B.
    use crate::feat::ui::sidebar::sessions::update_visual_parents_on_removal;

    let mut state = AppState::default_with_scope_focus();

    let mut root = ChatSessionState::new();
    root.set_title("root".to_owned());
    let root_id = root.session_id().clone();
    state.session.insert(root);

    let mut a = ChatSessionState::new();
    a.set_title("session A".to_owned());
    a.set_parent_session(root_id.clone());
    let a_id = a.session_id().clone();
    state.session.insert(a);

    let mut b = ChatSessionState::new();
    b.set_title("session B".to_owned());
    b.set_parent_session(a_id.clone());
    let b_id = b.session_id().clone();
    state.session.insert(b);

    // Also add an unrelated session with its own visual parent.
    let mut unrelated_parent = ChatSessionState::new();
    unrelated_parent.set_title("unrelated parent".to_owned());
    let unrelated_parent_id = unrelated_parent.session_id().clone();
    state.session.insert(unrelated_parent);

    let mut unrelated_child = ChatSessionState::new();
    unrelated_child.set_title("unrelated child".to_owned());
    unrelated_child.set_parent_session(unrelated_parent_id);
    let unrelated_child_id = unrelated_child.session_id().clone();
    state.session.insert(unrelated_child);

    // Remove default session.
    let default_id = state.session.active_session_id().clone();
    state.session.remove(&default_id);
    state.session.set_active(root_id.clone());

    // When removing A.
    update_visual_parents_on_removal(&mut state, &a_id);

    // Then B is reparented to root.
    assert_eq!(
        state.frontend.sessions_section.visual_parents.get(&b_id),
        Some(&root_id),
        "B should be reparented to root"
    );
    // And the unrelated child is NOT reparented (it has a different parent).
    assert_eq!(
        state
            .frontend
            .sessions_section
            .visual_parents
            .get(&unrelated_child_id),
        None,
        "unrelated child should not be reparented - its parent is not being removed"
    );
}

#[rstest::rstest]
fn clear_visual_parents_on_load_removes_only_entries_pointing_to_loaded_session() {
    // Given a state with visual_parents entries.
    use crate::feat::ui::sidebar::sessions::clear_visual_parents_on_load;

    let mut state = AppState::default_with_scope_focus();
    let id_x = crate::protocol::SessionId::new();
    let id_y = crate::protocol::SessionId::new();
    let loaded_id = crate::protocol::SessionId::new();
    let other_id = crate::protocol::SessionId::new();

    // entry_x -> loaded_id (should be removed after load)
    state
        .frontend
        .sessions_section
        .visual_parents
        .insert(id_x.clone(), loaded_id.clone());
    // entry_y -> other_id (should be kept)
    state
        .frontend
        .sessions_section
        .visual_parents
        .insert(id_y.clone(), other_id.clone());

    // When clearing on load for loaded_id.
    clear_visual_parents_on_load(&mut state, &loaded_id);

    // Then only entry_x is removed (pointed to loaded_id).
    assert_eq!(
        state.frontend.sessions_section.visual_parents.get(&id_x),
        None,
        "entry pointing to loaded session should be removed"
    );
    assert_eq!(
        state.frontend.sessions_section.visual_parents.get(&id_y),
        Some(&other_id),
        "entry pointing to other session should be kept"
    );
}

#[rstest::rstest]
fn clear_visual_parents_on_load_actually_removes_entries() {
    // Given a state with a visual_parents entry that should be cleared.
    use crate::feat::ui::sidebar::sessions::clear_visual_parents_on_load;

    let mut state = AppState::default_with_scope_focus();
    let child_id = crate::protocol::SessionId::new();
    let loaded_id = crate::protocol::SessionId::new();

    state
        .frontend
        .sessions_section
        .visual_parents
        .insert(child_id, loaded_id.clone());
    assert_eq!(state.frontend.sessions_section.visual_parents.len(), 1);

    // When clearing on load.
    clear_visual_parents_on_load(&mut state, &loaded_id);

    // Then the entry is removed.
    assert!(
        state.frontend.sessions_section.visual_parents.is_empty(),
        "visual_parents should be empty after clearing the loaded session's entries"
    );
}

#[rstest::rstest]
fn sidebar_marks_only_subagent_origin() {
    // Given a state holding a task-tool child and a plain root session.
    let mut state = AppState::default_with_scope_focus();
    let parent = ChatSessionState::new();
    let child = ChatSessionState::new_child(&parent.session_id().clone(), true);
    let child_id = child.session_id().clone();
    state.session.insert(child);
    let root = ChatSessionState::new();
    let root_id = root.session_id().clone();
    state.session.insert(root);

    // When collecting open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then only the Subagent-origin session is marked.
    let child_entry = sessions
        .iter()
        .find(|e| e.id == child_id)
        .expect("child entry");
    assert!(child_entry.is_subagent);
    let root_entry = sessions
        .iter()
        .find(|e| e.id == root_id)
        .expect("root entry");
    assert!(!root_entry.is_subagent);
}

#[rstest::rstest]
fn sidebar_unmarks_forked_sessions() {
    // Given a session with a parent link but User origin — the shape of a
    // forked session (fork stamps Fork origin, which is never marked).
    let mut state = AppState::default_with_scope_focus();
    let parent = ChatSessionState::new();
    let mut forked = ChatSessionState::new();
    forked.set_parent_session(parent.session_id().clone());
    let forked_id = forked.session_id().clone();
    state.session.insert(forked);

    // When collecting open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the parent-linked session is not marked as a subagent.
    let entry = sessions
        .iter()
        .find(|e| e.id == forked_id)
        .expect("forked entry");
    assert!(!entry.is_subagent);
}

// ---------------------------------------------------------------------------
// Archive tree - validator
// ---------------------------------------------------------------------------

use crate::feat::ui::sidebar::sessions::archive_tree::{
    ArchiveTreeError, ArchiveTreePrompt, TreePromptAction, archive_tree_members,
};

/// Helper: builds a session tree of root -> child -> grandchild, plus an
/// unrelated survivor root. All sessions get titles for lookup. Returns the
/// state and the IDs of its members.
fn state_with_archive_tree() -> (AppState, [crate::protocol::SessionId; 4]) {
    let mut state = AppState::default_with_scope_focus();
    let mut root = ChatSessionState::new();
    root.push_entry(ChatEntry::user("tree root"));
    root.set_title("tree root".to_owned());
    let root_id = root.session_id().clone();
    state.session.insert(root);

    let mut child = ChatSessionState::new();
    child.set_title("tree child".to_owned());
    child.set_parent_session(root_id.clone());
    let child_id = child.session_id().clone();
    state.session.insert(child);

    let mut grandchild = ChatSessionState::new();
    grandchild.set_title("tree grandchild".to_owned());
    grandchild.set_parent_session(child_id.clone());
    let grandchild_id = grandchild.session_id().clone();
    state.session.insert(grandchild);

    let mut survivor = ChatSessionState::new();
    survivor.push_entry(ChatEntry::user("survivor"));
    survivor.set_title("survivor".to_owned());
    let survivor_id = survivor.session_id().clone();
    state.session.insert(survivor);

    // Drop the default session if it survived under another name.
    let default_id = state.session.active_session_id().clone();
    if ![&root_id, &child_id, &grandchild_id, &survivor_id]
        .iter()
        .any(|id| **id == default_id)
    {
        state.session.remove(&default_id);
    }

    (state, [root_id, child_id, grandchild_id, survivor_id])
}

/// Helper: puts the sessions section into the sidebar focus stack and selects
/// the entry with the given title.
fn focus_sessions_and_select(state: &mut AppState, title: &str) {
    state.frontend.scope_push(FocusScope::SidebarSessions);
    let sessions = sorted_open_sessions(state);
    let index = sessions
        .iter()
        .position(|e| entry_title(state, &e.id) == title)
        .unwrap_or_else(|| panic!("session titled {title} not in sidebar"));
    state.frontend.sessions_section.selected_index = Some(index);
}

#[rstest::rstest]
fn archive_tree_members_returns_selection_and_transitive_descendants() {
    // Given a focused sessions section with root -> child -> grandchild.
    let (mut state, [root_id, child_id, grandchild_id, _survivor]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");

    // When resolving the archive-tree members.
    let members = archive_tree_members(&state).expect("members");

    // Then the members are the root, child, and grandchild (BFS order).
    assert_eq!(members, vec![root_id, child_id, grandchild_id]);
}

#[rstest::rstest]
fn archive_tree_members_includes_fork_children() {
    // Given a root whose only descendant is a fork-shaped child (parent link
    // set, forked sessions are never marked as subagents).
    let mut state = AppState::default_with_scope_focus();
    let mut root = ChatSessionState::new();
    root.push_entry(ChatEntry::user("fork root"));
    root.set_title("fork root".to_owned());
    let root_id = root.session_id().clone();
    state.session.insert(root);

    let mut forked = ChatSessionState::new();
    forked.set_title("forked child".to_owned());
    forked.set_parent_session(root_id.clone());
    let forked_id = forked.session_id().clone();
    state.session.insert(forked);
    let default_id = state.session.active_session_id().clone();
    if default_id != root_id && default_id != forked_id {
        state.session.remove(&default_id);
    }
    focus_sessions_and_select(&mut state, "fork root");

    // When resolving the archive-tree members.
    let members = archive_tree_members(&state).expect("members");

    // Then both the root and the fork are members.
    assert_eq!(members, vec![root_id, forked_id]);
}

#[rstest::rstest]
fn archive_tree_members_rejects_when_a_descendant_is_busy() {
    // Given a tree whose grandchild is busy.
    let (mut state, [.., grandchild_id, _survivor]) = state_with_archive_tree();
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .begin_busy();
    focus_sessions_and_select(&mut state, "tree root");

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then validation fails with SubtreeBusy (all-or-nothing).
    assert_eq!(result, Err(ArchiveTreeError::SubtreeBusy));
}

#[rstest::rstest]
fn archive_tree_members_rejects_when_selection_is_busy() {
    // Given a tree whose selected root itself is busy.
    let (mut state, [root_id, ..]) = state_with_archive_tree();
    state.session.get_mut(&root_id).expect("root").begin_busy();
    focus_sessions_and_select(&mut state, "tree root");

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then validation fails with SubtreeBusy.
    assert_eq!(result, Err(ArchiveTreeError::SubtreeBusy));
}

#[rstest::rstest]
fn archive_tree_members_returns_single_member_for_leaf() {
    // Given a focused sessions section with the grandchild selected.
    let (mut state, [.., grandchild_id, _survivor]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree grandchild");

    // When resolving the archive-tree members.
    let members = archive_tree_members(&state).expect("members");

    // Then the only member is the selection itself.
    assert_eq!(members, vec![grandchild_id]);
}

#[rstest::rstest]
fn archive_tree_members_rejected_when_no_selection() {
    // Given a focused sessions section with no cursor.
    let (mut state, _) = state_with_archive_tree();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = None;

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then validation fails with NoSelection.
    assert_eq!(result, Err(ArchiveTreeError::NoSelection));
}

#[rstest::rstest]
fn archive_tree_members_rejected_when_wrong_section() {
    // Given the sessions section is not focused.
    let (state, _) = state_with_archive_tree();

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then validation fails with WrongSection.
    assert_eq!(result, Err(ArchiveTreeError::WrongSection));
}

// ---------------------------------------------------------------------------
// Archive tree - intent flow (arm, confirm, dismiss, busy flip)
// ---------------------------------------------------------------------------

use crate::feat::intent::IntentHandler;
use crate::protocol::Intent;

#[rstest::rstest]
fn archive_tree_arm_sets_confirm_prompt_with_subtree_count() {
    // Given a focused idle subtree of three sessions.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first SidebarSessionArchiveTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the confirm prompt is armed with the subtree size.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Confirm {
            count: 3,
            action: TreePromptAction::Archive,
        })
    );
    // And no commands were emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn archive_tree_arm_sets_busy_prompt_when_subtree_busy() {
    // Given a focused subtree containing a busy grandchild.
    let (mut state, [.., grandchild_id, _survivor]) = state_with_archive_tree();
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .begin_busy();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first SidebarSessionArchiveTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the busy prompt is armed.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Busy)
    );
    // And no commands were emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn archive_tree_second_press_emits_archive_command() {
    // Given an armed confirm prompt over an idle subtree.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling a second SidebarSessionArchiveTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the ArchiveSessionTree command is emitted.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("ArchiveSessionTree")),
        "should emit ArchiveSessionTree: {:?}",
        result.message_names
    );
    // And the prompt is cleared.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn archive_tree_confirm_after_member_became_busy_switches_to_busy_prompt() {
    // Given an armed confirm prompt whose grandchild then becomes busy.
    let (mut state, [.., grandchild_id, _survivor]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .begin_busy();

    // When handling a second SidebarSessionArchiveTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the prompt flipped to Busy instead of archiving.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Busy)
    );
    // And no archive command was emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("ArchiveSessionTree")),
        "should not emit ArchiveSessionTree: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn archive_tree_other_intent_dismisses_prompt_and_processes_normally() {
    // Given an armed confirm prompt.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling a different intent (SidebarSectionNext).
    let _result = IntentHandler::handle(
        &Intent::SidebarSectionNext,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the prompt is dismissed.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn archive_tree_invalid_context_leaves_no_prompt() {
    // Given the sessions section is not focused.
    let (mut state, _) = state_with_archive_tree();

    // When handling SidebarSessionArchiveTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then no prompt is armed and no commands are emitted.
    assert_eq!(state.frontend.archive_tree_prompt, None);
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_tree_arm_sets_confirm_prompt_with_action() {
    // Given a focused idle subtree of three sessions.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first SidebarSessionTeardownTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the confirm prompt is armed for teardown-and-archive.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Confirm {
            count: 3,
            action: TreePromptAction::TeardownAndArchive,
        })
    );
    // And no commands were emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_tree_arm_sets_busy_prompt_when_subtree_busy() {
    // Given a focused subtree containing a busy grandchild.
    let (mut state, [.., grandchild_id, _survivor]) = state_with_archive_tree();
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .begin_busy();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first SidebarSessionTeardownTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the busy prompt is armed.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Busy)
    );
    // And no commands were emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_tree_second_press_emits_teardown_tree_command() {
    // Given an armed teardown confirm prompt over an idle subtree.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling a second SidebarSessionTeardownTree.
    let result = IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the TeardownSessionTree command is emitted.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("TeardownSessionTree")),
        "should emit TeardownSessionTree: {:?}",
        result.message_names
    );
    // And no archive-tree command was emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("ArchiveSessionTree")),
        "should not emit ArchiveSessionTree: {:?}",
        result.message_names
    );
    // And the prompt is cleared.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn teardown_tree_other_intent_dismisses_prompt() {
    // Given an armed teardown confirm prompt.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling a different intent (SidebarSectionNext).
    let _result = IntentHandler::handle(
        &Intent::SidebarSectionNext,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the prompt is dismissed.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn busy_tree_prompt_dismisses_on_other_intent() {
    // Given the busy notice is showing.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);

    // When handling a different intent (SidebarSectionNext).
    let _result = IntentHandler::handle(
        &Intent::SidebarSectionNext,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the busy notice is dismissed.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn busy_tree_prompt_still_confirms_on_tree_key() {
    // Given the busy notice is showing and the subtree has become idle.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);

    // When handling SidebarSessionTeardownTree (the notice's own key).
    let result = IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the re-validation passes and the teardown-tree command is emitted.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("TeardownSessionTree")),
        "should emit TeardownSessionTree: {:?}",
        result.message_names
    );
    // And the prompt is cleared.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn a_key_over_teardown_prompt_dismisses_then_arms_archive_prompt() {
    // Given an armed teardown confirm prompt.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling SidebarSessionArchiveTree (the sibling tree key).
    let _result = IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the teardown prompt was replaced by a fresh archive prompt.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Confirm {
            count: 3,
            action: TreePromptAction::Archive,
        })
    );
}

#[rstest::rstest]
fn x_key_over_archive_prompt_dismisses_then_arms_teardown_prompt() {
    // Given an armed archive confirm prompt.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    IntentHandler::handle(
        &Intent::SidebarSessionArchiveTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // When handling SidebarSessionTeardownTree (the sibling tree key).
    let _result = IntentHandler::handle(
        &Intent::SidebarSessionTeardownTree,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        &empty_pickers(),
    );

    // Then the archive prompt was replaced by a fresh teardown prompt.
    assert_eq!(
        state.frontend.archive_tree_prompt,
        Some(ArchiveTreePrompt::Confirm {
            count: 3,
            action: TreePromptAction::TeardownAndArchive,
        })
    );
}

// ---------------------------------------------------------------------------
// Archive tree - prompt render
// ---------------------------------------------------------------------------

use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Empty slice registry + route table for handler tests that don't
/// exercise slices or route rows.
fn empty_slices() -> crate::common::slices::Slices {
    crate::common::slices::Slices::new()
}

fn empty_pickers() -> jinn_picker::PickerRegistry {
    jinn_picker::PickerRegistry::new()
}

fn empty_routes() -> crate::common::slices::key_routes::KeyRoutes {
    crate::common::slices::key_routes::KeyRoutes::new()
}

#[rstest::rstest]
fn archive_tree_prompt_renders_yellow_confirm_with_count() {
    // Given a focused selection with an armed confirm prompt of 3 sessions.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Confirm {
        count: 3,
        action: TreePromptAction::Archive,
    });

    // When rendering the archive-tree prompt overlay.
    let text = render_archive_tree_prompt_overlay(&state, 60);

    // Then the confirm text with the count appears.
    assert!(
        text.contains("Press A again to archive 3 sessions"),
        "rendered: {text}"
    );
}

#[rstest::rstest]
fn close_session_prompt_right_aligns_inside_the_frame() {
    // Given a narrow sidebar (20 of 60 columns) with the close prompt armed
    // on a selected session — the banner text (50 columns) cannot fit inside
    // the sidebar.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.close_session_prompt = true;

    // When rendering the section plus the late overlay.
    let rows = render_sessions_with_close_prompt(&state, 20);

    // Then the banner is rendered fully, right-aligned to the frame's right
    // edge (extending left over the main column) — not clipped to the sidebar.
    let banner_row = rows
        .iter()
        .position(|row| row.contains("Press x again to teardown"))
        .expect("banner is rendered");
    let row = &rows[banner_row];
    assert!(
        row.contains("Press x again to teardown and archive 1 session"),
        "banner is complete, not clipped: {row}"
    );
    // And its right edge touches the frame's right edge.
    let right = row.trim_end().len();
    assert_eq!(right, 59, "banner right edge at frame column 59: {row}");
    // And it sits one row above the cursor row.
    let cursor_row = rows
        .iter()
        .position(|r| r.contains("tree root"))
        .expect("cursor session row is visible");
    assert_eq!(
        banner_row,
        cursor_row.saturating_sub(1),
        "banner must sit 1 row above the cursor row"
    );
}

#[rstest::rstest]
fn archive_tree_prompt_renders_red_busy_notice() {
    // Given a focused selection with the busy prompt showing.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);

    // When rendering the archive-tree prompt overlay.
    let text = render_archive_tree_prompt_overlay(&state, 60);

    // Then the busy notice appears.
    assert!(
        text.contains("Cannot archive tree while a session is busy"),
        "rendered: {text}"
    );
}

#[rstest::rstest]
fn archive_tree_prompt_anchors_above_the_cursor_row_at_top_of_list() {
    // Given the survivor root selected — with two roots, the newest-first
    // root sort puts the survivor at the very top row of the sessions list —
    // and the busy prompt armed. The overlay helper must render through the
    // Sidebar container: the production bottom-anchor lands the first entry
    // at row 0, so a clamped sub(1) would paint the banner ON the cursor.
    let (mut state, [.., survivor_id]) = state_with_archive_tree();
    let survivor_title = entry_title(&state, &survivor_id);
    focus_sessions_and_select(&mut state, &survivor_title);
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);

    // When rendering the sessions section followed by the overlay (the same
    // late-overlay pass jinn-tui performs).
    let rows = render_sessions_with_archive_tree_prompt(&state, 60);

    // Then the banner renders on the row ABOVE the cursor row — never on
    // the cursor row itself.
    let cursor_row = rows
        .iter()
        .position(|row| row.contains(&survivor_title))
        .expect("selected session row is visible");
    let banner_row = rows
        .iter()
        .position(|row| row.contains("Cannot archive tree"))
        .expect("banner is rendered");
    assert_eq!(
        banner_row,
        cursor_row.saturating_sub(1),
        "banner must sit 1 row above the cursor row"
    );
}

#[rstest::rstest]
fn teardown_tree_prompt_renders_teardown_confirm_text() {
    // Given a focused selection with an armed teardown confirm prompt.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Confirm {
        count: 3,
        action: TreePromptAction::TeardownAndArchive,
    });

    // When rendering the archive-tree prompt overlay.
    let text = render_archive_tree_prompt_overlay(&state, 60);

    // Then the teardown confirm text with the count appears.
    assert!(
        text.contains("Press X again to teardown and archive 3 sessions"),
        "rendered: {text}"
    );
}

#[rstest::rstest]
fn archive_tree_prompt_spans_past_the_sidebar_over_the_main_column() {
    // Given the busy prompt showing with a narrow sidebar (30 cols) inside a
    // 60-wide frame — the 46-col banner cannot fit in the sidebar alone.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);

    // When rendering the overlay.
    let text = render_archive_tree_prompt_overlay(&state, 30);

    // Then the full banner renders, spanning left over the main column.
    assert!(
        text.contains("Cannot archive tree while a session is busy"),
        "rendered: {text}"
    );
}

/// Helper: renders the archive-tree prompt overlay with the given sidebar
/// width inside a 60-wide frame and returns the buffer as one string per row.
fn render_archive_tree_prompt_rows(state: &AppState, sidebar_width: u16) -> Vec<String> {
    let (width, height) = (60, 12);
    let frame_area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let sidebar_rect = ratatui::layout::Rect {
        x: width - sidebar_width,
        y: 0,
        width: sidebar_width,
        height,
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let ctx = RenderCtx::new(state, &slices, &overlay_views);
    terminal
        .draw(|frame| {
            crate::feat::ui::sidebar::sessions::render_archive_tree_prompt_for_state(
                frame,
                sidebar_rect,
                frame_area,
                &ctx,
            );
        })
        .expect("draw");
    buffer_rows(&terminal, width, height)
}

/// Helper: renders the sessions section then the archive-tree prompt overlay
/// (the same two-pass render jinn-tui performs) and returns one buffer string
/// per row. Goes through the `Sidebar` container so the sessions section gets
/// the same bottom-anchored sub-rect the production render gives it.
fn render_sessions_with_archive_tree_prompt(state: &AppState, sidebar_width: u16) -> Vec<String> {
    let (width, height) = (60, 20);
    let frame_area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let sidebar_rect = ratatui::layout::Rect {
        x: width - sidebar_width,
        y: 0,
        width: sidebar_width,
        height,
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let ctx = RenderCtx::new(state, &slices, &overlay_views);
    let mut sidebar = crate::feat::ui::sidebar::Sidebar::default();
    sidebar.register(Box::new(SessionsSection::new()));
    terminal
        .draw(|frame| {
            sidebar.render(frame, sidebar_rect, &ctx);
            crate::feat::ui::sidebar::sessions::render_archive_tree_prompt_for_state(
                frame,
                sidebar_rect,
                frame_area,
                &ctx,
            );
        })
        .expect("draw");
    buffer_rows(&terminal, width, height)
}

/// Helper: flattens a terminal's test backend into one string per row.
fn buffer_rows(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect()
        })
        .collect()
}

/// Helper: renders the sessions section then the close-session prompt overlay
/// (the same two-pass render jinn-tui performs) and returns one buffer string
/// per row. Goes through the `Sidebar` container so the sessions section gets
/// the same bottom-anchored sub-rect the production render gives it.
fn render_sessions_with_close_prompt(state: &AppState, sidebar_width: u16) -> Vec<String> {
    let (width, height) = (60, 20);
    let frame_area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let sidebar_rect = ratatui::layout::Rect {
        x: width - sidebar_width,
        y: 0,
        width: sidebar_width,
        height,
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let ctx = RenderCtx::new(state, &slices, &overlay_views);
    let mut sidebar = crate::feat::ui::sidebar::Sidebar::default();
    sidebar.register(Box::new(SessionsSection::new()));
    terminal
        .draw(|frame| {
            sidebar.render(frame, sidebar_rect, &ctx);
            crate::feat::ui::sidebar::sessions::render_close_session_prompt_for_state(
                frame,
                sidebar_rect,
                frame_area,
                &ctx,
            );
        })
        .expect("draw");
    buffer_rows(&terminal, width, height)
}

/// Helper: renders the archive-tree prompt overlay with the given sidebar
/// width inside a 60-wide frame and returns the full buffer text.
fn render_archive_tree_prompt_overlay(state: &AppState, sidebar_width: u16) -> String {
    render_archive_tree_prompt_rows(state, sidebar_width).concat()
}

#[rstest::rstest]
fn sidebar_after_archive_tree_cascade_shows_survivors_only() {
    // Given a tree (root -> child -> grandchild) plus a survivor root, with
    // the tree root selected and clamping in range.
    let (mut state, [root_id, child_id, grandchild_id, survivor_id]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");

    // When simulating the cascade the actor performs: remove each member
    // with visual-parent maintenance, then reconcile the sidebar.
    for member in [&root_id, &child_id, &grandchild_id] {
        crate::feat::ui::sidebar::sessions::update_visual_parents_on_removal(&mut state, member);
        state.session.remove(member);
        crate::feat::ui::sidebar::sessions::reconcile_after_session_removal(&mut state);
    }

    // Then the sidebar lists only the survivor.
    let sessions = sorted_open_sessions(&state);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, survivor_id);

    // And no stale visual_parents entries remain.
    assert!(
        state.frontend.sessions_section.visual_parents.is_empty(),
        "visual_parents should be empty, got {:?}",
        state.frontend.sessions_section.visual_parents
    );

    // And the cursor is valid: either None or in bounds.
    if let Some(index) = state.frontend.sessions_section.selected_index {
        assert!(index < sessions.len(), "cursor out of bounds: {index}");
    }
}
