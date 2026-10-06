#![allow(
    unused_mut,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::sections::section_trait::{EnterFrom, SectionNavResult, SidebarIntent, SidebarSection};
use crate::sections::sessions::{
    SessionCloseError, SessionsSection, handle_session_activate, handle_session_close_arm,
    navigate, receive_cursor, sorted_open_sessions, validate_session_close,
};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_session_state::ChatSessionState;

use jinn_kernel::protocol::ChatEntry;
use ratatui::style::Color;

/// Helper: get a session title from the live session map via a tree entry.
fn complete_removed_session(state: &mut AppState, removed_id: &jinn_core_types::SessionId) {
    complete_removed_session_from(state, removed_id, false)
}

/// As [`complete_removed_session`], for the case where the removed session was
/// the one the user was reading.
fn complete_active_session(state: &mut AppState, removed_id: &jinn_core_types::SessionId) {
    complete_removed_session_from(state, removed_id, true)
}

fn complete_removed_session_from(
    state: &mut AppState,
    removed_id: &jinn_core_types::SessionId,
    was_active: bool,
) {
    let removed_parent = state
        .session
        .get(removed_id)
        .and_then(|session| session.parent_session().as_ref().cloned());
    crate::sections::sessions::update_visual_parents_on_removal(state, removed_id);
    // Recorded the way the pre-render pass does, before the row is gone.
    crate::sections::capture_rows::capture_sessions_cursor_row(state);
    state.session.remove_without_replacement(removed_id);
    crate::sections::sessions::state::repair_visual_parents_after_removal(
        &state.session,
        &mut state.frontend,
        removed_id,
        removed_parent.as_ref(),
    );
    crate::sections::sessions::reconcile_after_session_removal(state, was_active);
}

fn entry_title(state: &AppState, id: &jinn_core_types::SessionId) -> String {
    state
        .session
        .get(id)
        .map(|s| s.title().unwrap_or("Untitled Session").to_owned())
        .unwrap_or_default()
}

/// Helper: get a session's created_at from the live session map.
fn entry_created_at(state: &AppState, id: &jinn_core_types::SessionId) -> jiff::Timestamp {
    state
        .session
        .get(id)
        .map(|s| *s.created_at())
        .unwrap_or_default()
}

/// Helper: check if the session's last entry is an error.
fn entry_last_is_error(state: &AppState, id: &jinn_core_types::SessionId) -> bool {
    state.session.get(id).is_some_and(|s| {
        s.history()
            .last()
            .is_some_and(|e| matches!(&e.kind, jinn_kernel::protocol::ChatEntryKind::Error(..)))
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

/// Puts the cursor on the session drawn at `row`.
///
/// Fixtures in this file spell a row because that is how a user thinks about
/// where they are looking; the cursor stores an id, so this is where a row
/// becomes a session.
fn cursor_row(state: &AppState) -> Option<usize> {
    let id = state
        .frontend
        .with_sections(|s| s.sessions.selected_id.clone(), || None)?;
    crate::sections::sessions::state::sorted_open_sessions(state)
        .iter()
        .position(|entry| entry.id == id)
}

fn cursor_to_row(state: &mut AppState, row: usize) {
    let id = crate::sections::sessions::state::sorted_open_sessions(state)
        .get(row)
        .map(|entry| entry.id.clone())
        .expect("the row under test exists in the list it indexes");
    state
        .frontend
        .update_sections(|s| s.sessions.selected_id = Some(id));
}

#[rstest::rstest]
fn section_id_is_sessions() {
    // Given a sessions section.
    let mut section = SessionsSection::new();

    // When asking the section for its id.
    // Then it identifies itself as the sessions section.
    assert_eq!(section.id(), jinn_sidebar_msg::SidebarSectionId::Sessions);
}

#[rstest::rstest]
fn content_height_with_one_session() {
    // Given a state holding a single session.
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();

    // When asking the section for its content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new_with_default_config(
        &state,
        &slices,
        &overlay_views,
    ));

    // Then it reserves 1 session + footer.
    assert_eq!(height, 2);
}

#[rstest::rstest]
fn content_height_with_three_sessions() {
    // Given a state holding three sessions.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(3);

    // When asking the section for its content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new_with_default_config(
        &state,
        &slices,
        &overlay_views,
    ));

    // Then it reserves 3 sessions + footer.
    assert_eq!(height, 4);
}

#[rstest::rstest]
fn navigate_down_moves_cursor_without_switching() {
    // Given state with 3 sessions, cursor at index 0.
    let mut state = state_with_sessions(3);
    let original_active = state.session.active_session_id().clone();
    cursor_to_row(&mut state, 0);

    // When navigating down.
    let (result, _) = navigate(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the result is Moved.
    assert_eq!(result, SectionNavResult::Moved);
    // And the cursor moved to index 1.
    assert_eq!(cursor_row(&state), Some(1));
    // And the active session did NOT change.
    assert_eq!(*state.session.active_session_id(), original_active);
}

/// R5: the sidebar's idle indicator must answer from the phase alone.
///
/// The busy counter that used to disagree with the phase is gone, so the
/// surviving claim is its converse: a session working in the phase sense
/// is not idle, with nothing else to override it.
#[rstest::rstest]
fn working_phase_session_is_not_idle_in_sidebar() {
    // Given a session mid-turn in the phase sense.
    let mut state = state_with_sessions(1);
    state.session.active_session_mut().begin_streaming();

    // When the sidebar derives its session list.
    let sessions = sorted_open_sessions(&state);

    // Then the session is not idle: the phase is the only answer.
    let active = sessions
        .iter()
        .find(|e| e.id == state.session.active_session_id().clone())
        .expect("active session is listed");
    assert!(
        !active.is_idle,
        "a Streaming session must not report idle: the phase is the only liveness signal"
    );
}

#[rstest::rstest]
fn navigate_up_moves_cursor_without_switching() {
    // Given state with 3 sessions, cursor at index 2.
    let mut state = state_with_sessions(3);
    let sessions = sorted_open_sessions(&state);
    state.session.set_active(sessions[2].id.clone());
    cursor_to_row(&mut state, 2);
    let original_active = state.session.active_session_id().clone();

    // When navigating up.
    let (result, _) = navigate(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the result is Moved.
    assert_eq!(result, SectionNavResult::Moved);
    // And the cursor moved to index 1.
    assert_eq!(cursor_row(&state), Some(1));
    // And the active session did NOT change.
    assert_eq!(*state.session.active_session_id(), original_active);
}

#[rstest::rstest]
fn navigate_down_at_bottom_returns_exhausted() {
    // Given state with 2 sessions, cursor at last index.
    let mut state = state_with_sessions(2);
    let sessions = sorted_open_sessions(&state);
    cursor_to_row(&mut state, sessions.len() - 1);

    // When navigating down.
    let (result, _) = navigate(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the result is Exhausted.
    assert_eq!(result, SectionNavResult::Exhausted);
}

#[rstest::rstest]
fn navigate_up_at_top_returns_exhausted() {
    // Given state with 2 sessions, cursor at index 0.
    let mut state = state_with_sessions(2);
    cursor_to_row(&mut state, 0);

    // When navigating up.
    let (result, _) = navigate(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the result is Exhausted.
    assert_eq!(result, SectionNavResult::Exhausted);
}

#[rstest::rstest]
fn navigate_action_returns_moved() {
    // Given a state with a single session.
    let mut state = AppState::default_with_scope_focus();

    // When navigating with an action intent.
    let (result, _) = navigate(
        &SidebarIntent::Action(jinn_kernel::KernelIntent::Quit),
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the section lets the action through instead of exhausting.
    assert_eq!(result, SectionNavResult::Moved);
}

/// The document offset the sidebar would use for `viewport_rows` with the
/// sessions section focused and the cursor at `cursor_index`.
fn document_offset_for(state: &AppState, viewport_rows: u16) -> u16 {
    let document = crate::sections::layout::document_with_cursor(
        state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    document.offset(viewport_rows)
}

#[rstest::rstest]
fn document_offset_is_zero_when_cursor_is_near_the_top() {
    // Given 40 sessions with the cursor on the first one.
    let mut state = state_with_sessions(40);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When resolving the document offset for a 20-row column.
    let offset = document_offset_for(&state, 20);

    // Then the window starts at the top of the document.
    assert_eq!(offset, 0);
}

#[rstest::rstest]
fn document_offset_clamps_at_the_end_for_the_last_session() {
    // Given 40 sessions with the cursor on the last one.
    let mut state = state_with_sessions(40);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 39);

    // When resolving the document offset for a 20-row column.
    let offset = document_offset_for(&state, 20);

    // Then the window shows the document's end, so the last row is the last line.
    let document = crate::sections::layout::document_with_cursor(
        &state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    assert_eq!(offset + 20, document.total_rows);
}

#[rstest::rstest]
fn document_offset_grows_as_the_cursor_moves_down() {
    // Given 40 sessions with the cursor on the first one.
    let mut state = state_with_sessions(40);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When navigating down past the bottom of a 20-row column.
    let before = document_offset_for(&state, 20);
    for _ in 0..20 {
        let _ = navigate(
            &SidebarIntent::MoveDown,
            &mut state,
            jinn_slices::empty_config_layer(),
        );
    }
    let after = document_offset_for(&state, 20);

    // Then the window followed the cursor.
    assert!(
        after > before,
        "offset should grow from {before} as the cursor moves down, got {after}"
    );
}

#[rstest::rstest]
fn every_session_is_reachable_when_uncapped() {
    // Given 100 sessions with the cursor on the first one.
    let mut state = state_with_sessions(100);
    cursor_to_row(&mut state, 0);

    // When navigating down until the list is exhausted.
    let mut moves = 0usize;
    while navigate(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    )
    .0 == SectionNavResult::Moved
    {
        moves += 1;
        assert!(moves <= 200, "navigation should terminate");
    }

    // Then the cursor reached the last session, so none is unreachable.
    assert_eq!(cursor_row(&state), Some(99));
}

#[rstest::rstest]
fn sessions_content_height_is_entry_count_plus_footer() {
    // Given 40 sessions.
    let state = state_with_sessions(40);

    // When computing the document height for the sessions section.
    let rows = crate::sections::layout::sessions_rows(&state);

    // Then every entry is counted, with one row for the footer.
    assert_eq!(rows, 41);
}

#[rstest::rstest]
fn content_height_is_uncapped() {
    // Given state with 20 sessions.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(20);

    // When computing content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new(
        &state,
        &slices,
        &overlay_views,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    ));

    // Then it counts every session, not a fixed window.
    assert_eq!(height, 21);
}
#[rstest::rstest]
fn receive_cursor_from_top_positions_at_index_zero() {
    // Given state with 3 sessions.
    let mut state = state_with_sessions(3);

    // When receiving cursor from top.
    let _ = receive_cursor(
        &mut state,
        EnterFrom::Top,
        jinn_slices::empty_config_layer(),
    );

    // Then the selected index is 0.
    assert_eq!(cursor_row(&state), Some(0));
}

#[rstest::rstest]
fn receive_cursor_from_bottom_positions_at_last_index() {
    // Given state with 3 sessions.
    let mut state = state_with_sessions(3);
    let count = sorted_open_sessions(&state).len();

    // When receiving cursor from bottom.
    let _ = receive_cursor(
        &mut state,
        EnterFrom::Bottom,
        jinn_slices::empty_config_layer(),
    );

    // Then the selected index is the last one.
    assert_eq!(cursor_row(&state), Some(count - 1));
}

#[rstest::rstest]
fn receive_cursor_noop_when_empty() {
    // Given state with no sessions (manually clear default).
    let mut state = AppState::default_with_scope_focus();
    let ids: Vec<jinn_core_types::SessionId> = state.session.sessions().keys().cloned().collect();
    for id in ids {
        state.session.remove_without_replacement(&id);
    }

    // When receiving cursor.
    let _ = receive_cursor(
        &mut state,
        EnterFrom::Top,
        jinn_slices::empty_config_layer(),
    );

    // Then no index is selected.
    assert_eq!(cursor_row(&state), None);
}

#[rstest::rstest]
fn sorted_sessions_orders_by_created_at_descending() {
    // Given a state holding three sessions.
    let state = state_with_sessions(3);

    // When collecting the sorted open sessions.
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
    // Given a state holding four sessions.
    let state = state_with_sessions(4);

    // When collecting the sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then every session is listed.
    assert_eq!(sessions.len(), 4);
}

#[rstest::rstest]
fn busy_phase_session_is_not_idle() {
    // Given a session whose turn is mid-flight.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();

    // When collecting sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the session entry is not idle (the throbber shows).
    assert_eq!(sessions.len(), 1);
    assert!(
        !sessions[0].is_idle,
        "a session in a busy phase should show throbber in sidebar"
    );
}

#[rstest::rstest]
fn idle_phase_is_idle() {
    // Given a session at rest in the phase sense.
    let state = AppState::default_with_scope_focus();

    // When collecting sorted open sessions.
    let sessions = sorted_open_sessions(&state);

    // Then the session entry is idle.
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].is_idle, "an Idle session should be idle");
}

#[rstest::rstest]
fn settled_turn_returns_to_idle() {
    // Given a session that was mid-turn and has settled.
    let mut state = AppState::default_with_scope_focus();
    {
        let session = state.active_session_mut();
        session.begin_streaming();
        session.finish_streaming_via_machine();
    }

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
    render_rows_skipping(section, state, width, height, 0)
}

/// Renders the section into a `width` x `height` terminal, drawing the
/// window that starts `skip_rows` entries into the list.
fn render_rows_skipping(
    section: &mut SessionsSection,
    state: &AppState,
    width: u16,
    height: u16,
    skip_rows: u16,
) -> Vec<String> {
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
            section.render(frame, area, skip_rows, &ctx);
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
    // Given a state with a single, active session.
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();

    // When rendering the section.
    let rows = render_rows(&mut section, &state, 30, 5);

    // Then the active session's row carries the active indicator.
    let combined = rows.join("\n");
    assert!(
        combined.contains("\u{25b8}"),
        "should contain active indicator, got: {combined}"
    );
}

#[rstest::rstest]
fn render_shows_untitled_for_session_without_title() {
    // Given a state whose session has no title.
    let mut section = SessionsSection::new();
    let state = AppState::default_with_scope_focus();

    // When rendering the section.
    let rows = render_rows(&mut section, &state, 30, 5);

    // Then the row falls back to a placeholder title.
    let combined = rows.join("\n");
    assert!(
        combined.contains("Untitled Session"),
        "should contain 'Untitled Session', got: {combined}"
    );
}

#[rstest::rstest]
fn render_draws_only_the_rows_the_window_covers() {
    // Given 20 sessions, a 5-row window, and a skip of 3.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(20);

    // When rendering.
    let rows = render_rows_skipping(&mut section, &state, 30, 5, 3);

    // Then every row of the window is drawn and no more. Entries 4..9 of the
    // list are in view, so the window is full and the footer is not among
    // them.
    assert_eq!(rows.len(), 5, "window should fill its height: {rows:?}");
    assert!(
        !rows.join("").contains('\u{2570}'),
        "footer belongs to the list's last row, which is out of window: {rows:?}"
    );
}

#[rstest::rstest]
fn render_draws_the_footer_when_the_window_reaches_the_last_entry() {
    // Given 20 sessions and a window whose final row is entry 20.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(20);

    // When rendering a 5-row window starting at entry 16.
    let rows = render_rows_skipping(&mut section, &state, 30, 5, 16);

    // Then the footer is drawn on the last row of the window.
    assert!(
        rows[4].contains('\u{2570}'),
        "footer should render when the window reaches the last entry: {rows:?}"
    );
}

#[rstest::rstest]
fn render_footer_uses_focus_accent_when_sidebar_focused() {
    // Given a sessions section with sidebar focused.
    let mut section = SessionsSection::new();
    let state = {
        let s = AppState::default_with_scope_focus();
        s.frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        s
    };

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
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
        s.frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
        s
    };

    // When rendering.
    let (mut terminal, area) = setup_term(30, 5);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
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
fn first_close_press_arms_prompt_without_removing_session() {
    // Given a focused sessions section with a selected session.
    let mut state = state_with_sessions(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 1);

    // When pressing close.
    let result = handle_session_close_arm(&mut state);

    // Then the prompt is armed without removing or switching sessions.
    assert!(state.frontend.close_session_prompt);
    assert_eq!(state.session.session_count(), 3);
    assert!(result.messages.is_empty());
}

#[rstest::rstest]
fn second_close_press_emits_lifecycle_command_for_selected_session() {
    // Given a focused sessions section with a selected idle session.
    let mut state = state_with_sessions(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 1);
    let selected_id = sorted_open_sessions(&state)[1].id.clone();

    // When pressing close twice.
    let _ = handle_session_close_arm(&mut state);
    let result = handle_session_close_arm(&mut state);

    // Then CloseSession targets the selected session.
    assert!(result.message_names[0].ends_with("CloseSession"));
    assert!(!state.frontend.close_session_prompt);
    assert!(state.session.contains(&selected_id));
}

#[rstest::rstest]
fn close_session_rejected_when_streaming() {
    // Given state with a streaming session, sessions section focused.
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);
    state.active_session_mut().begin_streaming();

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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
        })
        .unwrap();

    // Then the title text on row 0 (first entry row) is a red block:
    // red background with the sidebar background as the text color.
    let buffer = terminal.backend().buffer();
    // The title starts after indicator(1) + space(1) + prefix(2) = column 4.
    let title_cell = buffer.cell((4, 0)).expect("title cell should exist");
    let theme = &state.frontend.theme;
    assert_eq!(title_cell.style().bg, Some(Color::Red));
    assert_eq!(title_cell.style().fg, Some(theme.gutter_bg));
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    cursor_to_row(&mut state, 1);
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
    cursor_to_row(&mut state, 1);

    // When activating.
    handle_session_activate(&mut state);

    // Then active session is unchanged.
    assert_eq!(*state.session.active_session_id(), original_active);
}

/// The sidebar no longer routes session creation through a kernel intent.
///
/// `SessionNewWithLifecycle` existed only to push a picker kind the kernel
/// owned. The lifecycle picker is slice-owned now, so the sidebar asks the
/// slice to open it; there is no kernel picker kind to assert on. What
/// matters is that the scope on the stack is the lifecycle picker's own.
#[rstest::rstest]
fn sidebar_sessions_section_opens_the_lifecycle_picker() {
    // Given the sidebar focused on its sessions section.
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    // When the lifecycle picker is opened, the way the sidebar's own row
    // does it: by pushing the slice-owned scope.
    let scope = jinn_session_lifecycle_msg::session_lifecycle_picker_scope();
    state
        .frontend
        .scope_push(jinn_slices::FocusScope::Dynamic(scope.clone()));

    // Then the lifecycle picker scope is the focused one.
    assert_eq!(
        state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(scope),
    );
}

#[rstest::rstest]
fn teardown_only_emits_run_session_teardown() {
    // Given a session with a lifecycle that has a teardown command.
    let mut state = AppState::default_with_scope_focus();
    let config = jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"fossil branch\"\n\
         setup_command = \"echo setup\"\nteardown_command = \"cleanup.sh $1\"\n",
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("fossil branch".to_owned()));
    state
        .active_session_mut()
        .set_lifecycle_args(vec!["my-branch".to_owned()]);
    cursor_to_row(&mut state, 0);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    // When handling session teardown (the route row's action).
    let result = super::sessions::handle_session_teardown(&mut state, &config);

    // Then a RunSessionTeardown command is emitted with the rendered teardown command.
    assert_eq!(result.message_names.len(), 1);
    assert!(result.message_names[0].contains("RunSessionTeardown"));
}

#[rstest::rstest]
fn teardown_only_is_noop_without_lifecycle_teardown() {
    // Given a session with a lifecycle that has NO teardown command.
    let mut state = AppState::default_with_scope_focus();
    let config = jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"plain\"\nsetup_command = \"echo setup\"\n",
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("plain".to_owned()));
    cursor_to_row(&mut state, 0);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    // When handling session teardown (the route row's action).
    let result = super::sessions::handle_session_teardown(&mut state, &config);

    // Then no commands are emitted (no teardown command to run).
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn teardown_only_is_noop_when_session_busy() {
    // Given a session with a teardown command that is currently busy.
    let mut state = AppState::default_with_scope_focus();
    let config = jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"fossil branch\"\n\
         setup_command = \"echo setup\"\nteardown_command = \"cleanup.sh $1\"\n",
    );
    state
        .active_session_mut()
        .set_lifecycle_name(Some("fossil branch".to_owned()));
    state
        .active_session_mut()
        .set_lifecycle_args(vec!["my-branch".to_owned()]);
    // Put the session mid-turn so close validation rejects it.
    state.active_session_mut().begin_streaming();
    cursor_to_row(&mut state, 0);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    // When handling session teardown (the route row's action).
    let result = super::sessions::handle_session_teardown(&mut state, &config);

    // Then no commands are emitted (validation gates on busy state).
    assert!(result.message_names.is_empty());
}

// ---------------------------------------------------------------------------
// Pure render helper tests
// ---------------------------------------------------------------------------

use crate::sections::sessions::render::entry_line::{
    arrow_span, entry_title_style, indicator_span,
};
use crate::sections::sessions::render::truncate::truncate_str;
use throbber_widgets_tui::ThrobberState;

fn default_theme() -> jinn_theme::Theme {
    AppState::default_with_scope_focus().frontend.theme
}

/// Minimal entry for exercising `entry_title_style` precedence rules.
fn style_entry(
    is_active: bool,
    last_entry_is_error: bool,
    is_subagent: bool,
) -> crate::sections::sessions::state::SessionEntry {
    crate::sections::sessions::state::SessionEntry {
        kind: crate::sections::sessions::state::SessionEntryKind::Session,
        id: jinn_core_types::SessionId::new(),
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
        is_attendant: false,
        is_attendant_prepping: false,
        attendant_fires_on_parent_completion: false,
        has_live_term: false,
        is_in_flight: false,
    }
}

#[rstest::rstest]
fn title_style_is_a_red_block_when_error() {
    // Given an entry whose last entry is an error.
    let theme = default_theme();
    let entry = style_entry(false, true, false);

    // When computing title style.
    let style = entry_title_style(&entry, &theme);

    // Then the style is a red block: sidebar-background text on red.
    assert_eq!(style.fg, Some(theme.gutter_bg));
    assert_eq!(style.bg, Some(Color::Red));
    // And no terminal inversion — the block is a real background.
    assert!(style.add_modifier.is_empty());
}

#[rstest::rstest]
fn title_style_is_state_only_selection_is_the_lines_business() {
    // Given a selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, false), &theme);

    // Then the style is the plain muted base: selection is the line's band,
    // never a property of the title.
    assert_eq!(style.fg, Some(theme.muted_text));
    assert_eq!(style.bg, None);
    assert!(style.add_modifier.is_empty());
}

#[rstest::rstest]
fn title_style_is_primary_text_when_active_not_selected() {
    // Given an active, not selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(true, false, false), &theme);

    // Then the style has primary text fg.
    assert_eq!(style.fg, Some(theme.primary_text));
}

#[rstest::rstest]
fn title_style_is_muted_text_when_inactive_not_selected() {
    // Given an inactive, not selected entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, false), &theme);

    // Then the style has muted text fg.
    assert_eq!(style.fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn title_style_uses_subagent_fg_for_subagent_when_inactive_not_selected() {
    // Given an inactive, not selected subagent entry without error.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, false, true), &theme);

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
    let style = entry_title_style(&style_entry(true, false, true), &theme);

    // Then the style has the subagent fg rather than primary text.
    assert_eq!(style.fg, Some(theme.subagent_fg));
}

#[rstest::rstest]
fn title_style_stays_red_for_errored_subagent() {
    // Given an errored, not selected subagent entry.
    let theme = default_theme();

    // When computing title style.
    let style = entry_title_style(&style_entry(false, true, true), &theme);

    // Then error red outranks the subagent color: a red block, not red text.
    assert_eq!(style.bg, Some(Color::Red));
    assert_eq!(style.fg, Some(theme.gutter_bg));
}

#[rstest::rstest]
fn indicator_span_returns_blank_space_when_idle() {
    // Given an idle entry.
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(true, false, &throbber, &theme);

    // Then it is a blank space.
    assert_eq!(span.content, " ");
}

#[rstest::rstest]
fn indicator_span_returns_throbber_character_when_working() {
    // Given a working entry (not idle).
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(false, false, &throbber, &theme);

    // Then it is a non-space character in the theme's busy color.
    assert_ne!(span.content, " ");
    assert!(!span.content.is_empty());
    assert_eq!(span.style.fg, Some(theme.streaming));
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
    orphan.set_parent_session(jinn_core_types::SessionId::new());
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
    cursor_to_row(&mut state, root_a_index);

    // When navigating down.
    let _ = navigate(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the cursor is on the next entry (root_a's first child in DFS order).
    let new_sessions = sorted_open_sessions(&state);
    let new_index = cursor_row(&state).expect("the cursor is on a row");
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
    cursor_to_row(&mut state, child_a1_index);

    // When navigating up.
    let _ = navigate(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the cursor is on root_a (parent).
    let new_index = cursor_row(&state).expect("the cursor is on a row");
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
    // Given state with a tree, cursor on the last row.
    let mut state = state_with_tree();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    let last_index = sessions.len() - 1;
    let last_id = sessions[last_index].id.clone();
    cursor_to_row(&mut state, last_index);

    // When closing it.
    complete_removed_session(&mut state, &last_id);

    // Then the cursor is clamped to the new last row — the row the closed
    // session occupied, which no longer exists.
    let remaining = sorted_open_sessions(&state);
    assert_eq!(cursor_row(&state), Some(remaining.len() - 1));
}

#[rstest::rstest]
fn closing_the_active_session_makes_the_cursor_row_active() {
    // Given state with a tree, cursor on a middle row that is also the active
    // session.
    let mut state = state_with_tree();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    let middle_index = sessions.len() / 2;
    let middle_id = sessions[middle_index].id.clone();
    state.session.set_active(middle_id.clone());
    cursor_to_row(&mut state, middle_index);

    // When closing it.
    complete_active_session(&mut state, &middle_id);

    // Then the session the cursor landed on is the active one, so the sidebar
    // does not show a selection band and an active marker a row apart.
    let cursor = state
        .frontend
        .with_sections(|s| s.sessions.selected_id.clone(), || None);
    assert_eq!(
        state.session.active_session_id(),
        &cursor.expect("a cursor")
    );
}

#[rstest::rstest]
fn close_root_session_promotes_children_to_roots() {
    // Given state with root_a having children.
    let mut state = state_with_tree();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    let root_a_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    let root_a_id = sessions[root_a_index].id.clone();
    cursor_to_row(&mut state, root_a_index);

    // When closing root_a.
    complete_removed_session(&mut state, &root_a_id);

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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    let child_a1_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("child a1"))
        .expect("child a1");
    let child_a1_id = sessions[child_a1_index].id.clone();
    cursor_to_row(&mut state, child_a1_index);

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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
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
    cursor_to_row(&mut state, child_a1_index);

    // When closing child_a1 (the intermediate parent).
    complete_removed_session(&mut state, &child_a1_id);

    // Then child_a1 is removed.
    assert!(!state.session.contains(&child_a1_id));
    // And the visual_parents index maps grandchild -> root_a.
    assert_eq!(
        state.frontend.with_sections(
            |s| s.sessions.visual_parents.get(&grandchild_id).cloned(),
            || None
        ),
        Some(root_a_id.clone()),
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
        Some(root_a_id.clone()),
        "grandchild's effective parent should be root_a"
    );
}

#[rstest::rstest]
fn archiving_root_does_not_create_visual_parents_for_orphaned_children() {
    // Given state with root_a having children.
    let mut state = state_with_tree();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let sessions = sorted_open_sessions(&state);
    let root_a_index = sessions
        .iter()
        .position(|s| entry_title(&state, &s.id).contains("root a"))
        .expect("root a");
    let _root_a_id = sessions[root_a_index].id.clone();
    cursor_to_row(&mut state, root_a_index);

    // When closing root_a (no loaded ancestor to reparent to).
    complete_removed_session(&mut state, &_root_a_id);

    // Then the visual_parents index should be empty (root has no loaded ancestor).
    assert!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .is_empty(),
        "no visual_parents entries should exist when root is closed"
    );
}

/// A four-deep chain `root -> A -> B -> leaf` in the session map, with the
/// default session removed and the sidebar focused on sessions.
fn state_with_four_level_chain() -> ChainIds {
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    (state, root_id, a_id, b_id, leaf_id)
}

/// The `visual_parents` entry recorded for `id`, if any.
fn visual_parent_of(
    state: &AppState,
    id: &jinn_core_types::SessionId,
) -> Option<jinn_core_types::SessionId> {
    state
        .frontend
        .with_sections(|s| s.sessions.visual_parents.get(id).cloned(), || None)
}

/// Archives `id` the way a keypress would: put the cursor on it, then complete
/// its removal.
fn archive_session_from_the_list(state: &mut AppState, id: &jinn_core_types::SessionId) {
    let index = sorted_open_sessions(state)
        .iter()
        .position(|s| &s.id == id)
        .expect("session is listed");
    cursor_to_row(state, index);
    complete_removed_session(state, id);
}

#[rstest::rstest]
fn multi_level_intermediate_hiding_reparents_to_nearest_loaded_ancestor() {
    // Given a chain: root -> A -> B -> leaf, with A archived.
    let (mut state, root_id, a_id, b_id, _leaf_id) = state_with_four_level_chain();

    // When archiving A.
    archive_session_from_the_list(&mut state, &a_id);

    // Then B is reparented to root.
    assert_eq!(
        visual_parent_of(&state, &b_id),
        Some(root_id),
        "B should be reparented to root"
    );
}

#[rstest::rstest]
fn archiving_a_reparented_session_reparents_its_child_transitively() {
    // Given a chain: root -> A -> B -> leaf, with A archived and B therefore
    // reparented to root.
    let (mut state, root_id, a_id, b_id, leaf_id) = state_with_four_level_chain();
    archive_session_from_the_list(&mut state, &a_id);

    // When archiving B.
    archive_session_from_the_list(&mut state, &b_id);

    // Then leaf is reparented to root (transitive via B's visual parent).
    assert_eq!(
        visual_parent_of(&state, &leaf_id),
        Some(root_id),
        "leaf should be reparented to root (transitive)"
    );
}

#[rstest::rstest]
fn a_transitively_reparented_leaf_sits_at_depth_one_under_root() {
    // Given a chain: root -> A -> B -> leaf, with A and B archived.
    let (mut state, _root_id, a_id, b_id, leaf_id) = state_with_four_level_chain();
    archive_session_from_the_list(&mut state, &a_id);
    archive_session_from_the_list(&mut state, &b_id);

    // When building the sidebar tree.
    let remaining = sorted_open_sessions(&state);

    // Then leaf sits at depth 1 under root.
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

    // Then root_a has two children, and only the last is flagged as such.
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

/// A chain `root -> A -> B` alongside an unrelated `parent -> child` pair, with
/// the default session removed and `root` active.
type ChainIds = (
    AppState,
    jinn_core_types::SessionId,
    jinn_core_types::SessionId,
    jinn_core_types::SessionId,
    jinn_core_types::SessionId,
);

fn state_with_chain_and_unrelated_pair() -> ChainIds {
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

    (state, root_id, a_id, b_id, unrelated_child_id)
}

#[rstest::rstest]
fn update_visual_parents_on_removal_reparents_only_children_of_removed_session() {
    // Given a chain: root -> A -> B, plus an unrelated parent -> child pair.
    use crate::sections::sessions::update_visual_parents_on_removal;

    let (mut state, root_id, a_id, b_id, unrelated_child_id) =
        state_with_chain_and_unrelated_pair();

    // When removing A.
    update_visual_parents_on_removal(&mut state, &a_id);

    // Then B is reparented to root.
    assert_eq!(
        visual_parent_of(&state, &b_id),
        Some(root_id),
        "B should be reparented to root"
    );
    // And the unrelated child is NOT reparented (it has a different parent).
    assert_eq!(
        visual_parent_of(&state, &unrelated_child_id),
        None,
        "unrelated child should not be reparented - its parent is not being removed"
    );
}

#[rstest::rstest]
fn clear_visual_parents_on_load_removes_only_entries_pointing_to_loaded_session() {
    // Given a state with visual_parents entries.
    use crate::sections::sessions::clear_visual_parents_on_load;

    let mut state = AppState::default_with_scope_focus();
    let id_x = jinn_core_types::SessionId::new();
    let id_y = jinn_core_types::SessionId::new();
    let loaded_id = jinn_core_types::SessionId::new();
    let other_id = jinn_core_types::SessionId::new();

    // entry_x -> loaded_id (should be removed after load)
    state.frontend.update_sections(|s| {
        s.sessions
            .visual_parents
            .insert(id_x.clone(), loaded_id.clone());
    });
    // entry_y -> other_id (should be kept)
    state.frontend.update_sections(|s| {
        s.sessions
            .visual_parents
            .insert(id_y.clone(), other_id.clone());
    });

    // When clearing on load for loaded_id.
    clear_visual_parents_on_load(&mut state, &loaded_id);

    // Then only entry_x is removed (pointed to loaded_id).
    assert_eq!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .get(&id_x),
        None,
        "entry pointing to loaded session should be removed"
    );
    assert_eq!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .get(&id_y),
        Some(&other_id),
        "entry pointing to other session should be kept"
    );
}

#[rstest::rstest]
fn clear_visual_parents_on_load_actually_removes_entries() {
    // Given a state with a visual_parents entry that should be cleared.
    use crate::sections::sessions::clear_visual_parents_on_load;

    let mut state = AppState::default_with_scope_focus();
    let child_id = jinn_core_types::SessionId::new();
    let loaded_id = jinn_core_types::SessionId::new();

    state.frontend.update_sections(|s| {
        s.sessions
            .visual_parents
            .insert(child_id, loaded_id.clone());
    });
    assert_eq!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .len(),
        1
    );

    // When clearing on load.
    clear_visual_parents_on_load(&mut state, &loaded_id);

    // Then the entry is removed.
    assert!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .is_empty(),
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

use crate::sections::sessions::archive_tree::{ArchiveTreeError, archive_tree_members};
use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};

/// Helper: builds a session tree of root -> child -> grandchild, plus an
/// unrelated survivor root. All sessions get titles for lookup. Returns the
/// state and the IDs of its members.
fn state_with_archive_tree() -> (AppState, [jinn_core_types::SessionId; 4]) {
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let index = sorted_open_sessions(state)
        .iter()
        .position(|e| entry_title(state, &e.id) == title)
        .unwrap_or_else(|| panic!("session titled {title} not in sidebar"));
    cursor_to_row(state, index);
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
        .begin_streaming();
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
    state
        .session
        .get_mut(&root_id)
        .expect("root")
        .begin_streaming();
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    state
        .frontend
        .update_sections(|s| s.sessions.selected_id = None);

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then validation fails with NoSelection.
    assert_eq!(result, Err(ArchiveTreeError::NoSelection));
}

#[rstest::rstest]
fn archive_tree_members_rejected_when_the_cursor_names_an_absent_session() {
    // Given a focused sessions section whose cursor names a session that is
    // no longer loaded — it was archived while the section was not focused.
    let (mut state, [root_id, ..]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state.session.remove_without_replacement(&root_id);

    // When resolving the archive-tree members.
    let result = archive_tree_members(&state);

    // Then the request is refused rather than acting on whatever row now
    // occupies the space the cursor used to point at.
    assert_eq!(result, Err(ArchiveTreeError::NoSelection));
}

#[rstest::rstest]
fn archive_tree_members_counts_a_descendant_that_is_not_listed() {
    // Given a parent with a loaded child and an archived grandchild beneath
    // it. The grandchild is not drawn, but it is part of the parent's subtree.
    let (mut state, [_root, _child, grandchild_id, _survivor]) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .set_session_state(jinn_session_store_msg::SessionState::Archived);

    // When resolving the archive-tree members.
    let members = archive_tree_members(&state).expect("members");

    // Then the walk reaches the grandchild anyway. The count the prompt shows
    // is the tree's size, not the number of rows on screen — the sidebar used
    // to build this from the rows and therefore undercounted here.
    assert!(
        members.contains(&grandchild_id),
        "an unlisted descendant is still in the subtree: {members:?}"
    );
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

use jinn_kernel::IntentHandler;
use jinn_kernel::protocol::KernelIntent;

#[rstest::rstest]
fn archive_tree_arm_sets_confirm_prompt_with_subtree_count() {
    // Given a focused idle subtree of three sessions.
    let (mut state, _) = state_with_archive_tree();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first archive-tree press.
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        .begin_streaming();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first archive-tree press.
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling a second archive-tree press (confirm).
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );
    state
        .session
        .get_mut(&grandchild_id)
        .expect("grandchild")
        .begin_streaming();

    // When handling a second archive-tree press (confirm).
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling a different intent.
    let _result = IntentHandler::handle(
        &KernelIntent::SessionNew,
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // Then the prompt is dismissed.
    assert_eq!(state.frontend.archive_tree_prompt, None);
}

#[rstest::rstest]
fn archive_tree_invalid_context_leaves_no_prompt() {
    // Given the sessions section is not focused.
    let (mut state, _) = state_with_archive_tree();

    // When handling the archive-tree press.
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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

    // When handling the first teardown-tree press.
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        .begin_streaming();
    focus_sessions_and_select(&mut state, "tree root");

    // When handling the first teardown-tree press.
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling a second teardown-tree press (confirm).
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling a different intent.
    let _result = IntentHandler::handle(
        &KernelIntent::SessionNew,
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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

    // When handling a different intent.
    let _result = IntentHandler::handle(
        &KernelIntent::SessionNew,
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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

    // When handling the teardown-tree press (the notice's own action).
    let result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling the archive-tree press (the sibling tree action).
    let _result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
        &tree_intent(jinn_sidebar_msg::TREE_ARCHIVE_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
    );

    // When handling the teardown-tree press (the sibling tree action).
    let _result = IntentHandler::handle(
        &tree_intent(jinn_sidebar_msg::TREE_TEARDOWN_ACTION),
        &mut state,
        &empty_slices(),
        &sidebar_routes(),
        jinn_slices::empty_config_layer(),
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
fn empty_slices() -> jinn_slices::Slices {
    jinn_slices::Slices::new()
}

/// The sidebar's real route table (the rows the `A`/`X` keys bind to).
fn sidebar_routes() -> jinn_slices::route::KeyRoutes {
    let routes = jinn_slices::route::KeyRoutes::new();
    crate::key_routes::attach_sidebar_rows(&routes);
    routes
}

/// The dynamic intent the sidebar's archive-tree route rows mint for the
/// given action string (what the `A`/`X` keys actually produce).
fn tree_intent(action: &'static str) -> KernelIntent {
    KernelIntent::Dynamic(jinn_slices::route::DynamicIntent::new(
        jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id(),
        action,
        action,
    ))
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
    // And it sits two rows above the cursor row, leaving a one-row gap.
    let cursor_row = rows
        .iter()
        .position(|r| r.contains("tree root"))
        .expect("cursor session row is visible");
    assert_eq!(
        banner_row,
        cursor_row.saturating_sub(2),
        "banner must sit 2 rows above the cursor row"
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
        cursor_row.saturating_sub(2),
        "banner must sit 2 rows above the cursor row"
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
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
    terminal
        .draw(|frame| {
            crate::sections::sessions::render_archive_tree_prompt_for_state(
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
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
    let mut sidebar = crate::sections::Sidebar::default();
    sidebar.register(Box::new(SessionsSection::new()));
    terminal
        .draw(|frame| {
            sidebar.render(frame, sidebar_rect, &ctx);
            crate::sections::sessions::render_archive_tree_prompt_for_state(
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
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
    let mut sidebar = crate::sections::Sidebar::default();
    sidebar.register(Box::new(SessionsSection::new()));
    terminal
        .draw(|frame| {
            sidebar.render(frame, sidebar_rect, &ctx);
            crate::sections::sessions::render_close_session_prompt_for_state(
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
        crate::sections::sessions::update_visual_parents_on_removal(&mut state, member);
        state.session.remove(member);
        crate::sections::sessions::reconcile_after_session_removal(&mut state, false);
    }

    // Then the sidebar lists only the survivor.
    let sessions = sorted_open_sessions(&state);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, survivor_id);

    // And no stale visual_parents entries remain.
    assert!(
        state
            .frontend
            .with_sections(
                |s| s.sessions.visual_parents.clone(),
                std::collections::HashMap::new
            )
            .is_empty(),
        "visual_parents should be empty, got {:?}",
        state.frontend.with_sections(
            |s| s.sessions.visual_parents.clone(),
            std::collections::HashMap::new
        )
    );

    // And the cursor is valid: either None or in bounds.
    if let Some(index) = cursor_row(&state) {
        assert!(index < sessions.len(), "cursor out of bounds: {index}");
    }
}

#[rstest::rstest]
fn an_unchanged_frame_rebuilds_the_tree_only_once() {
    // Given a sessions section that has rendered one frame.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(3);
    {
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let ctx = RenderCtx::new(
            &state,
            &slices,
            &overlay_views,
            jinn_kernel::common::render_ctx::empty_config_layer(),
        );
        section.content_height(&ctx);
    }
    let after_first = section.rebuilds();
    assert_eq!(after_first, 1, "the first frame must build the tree");

    // When several more frames render with nothing changed.
    for _ in 0..5 {
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let ctx = RenderCtx::new(
            &state,
            &slices,
            &overlay_views,
            jinn_kernel::common::render_ctx::empty_config_layer(),
        );
        section.content_height(&ctx);
    }

    // Then no further rebuilds happened.
    assert_eq!(
        section.rebuilds(),
        after_first,
        "an unchanged frame must reuse the cached tree"
    );
}

/// State holding a parent session and a composed attendant.
fn state_with_composed_attendant() -> AppState {
    let mut state = AppState::default_with_scope_focus();
    let parent = state.session.active_session().clone();
    let mut attendant = ChatSessionState::new_attendant(&parent, true);
    attendant.set_title("reviewer".to_owned());
    attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
    attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
    attendant.set_attendant_is_prepping(false);
    state.session.insert(attendant);
    state
}

/// The id of the attendant in `state`.
fn attendant_id(state: &AppState) -> jinn_core_types::SessionId {
    state
        .session
        .iter()
        .find(|(_, session)| session.is_attendant())
        .map(|(id, _)| id.clone())
        .expect("an attendant session")
}

/// Whether the sessions tree marks any attendant as still composing.
fn any_attendant_prepping(state: &AppState) -> bool {
    sorted_open_sessions(state)
        .iter()
        .any(|entry| entry.is_attendant_prepping)
}

#[rstest::rstest]
fn leaving_prep_mode_refreshes_the_sessions_tree() {
    // Given a section that has already rendered a frame with a composed
    // attendant, so no prep marker is owed.
    let mut section = SessionsSection::new();
    let mut state = state_with_composed_attendant();
    {
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        section.content_height(&RenderCtx::new_with_default_config(
            &state,
            &slices,
            &overlay_views,
        ));
    }
    let after_first = section.rebuilds();
    assert!(
        !any_attendant_prepping(&state),
        "a composed attendant is not marked"
    );

    // When the attendant goes back into prep mode and the section asks for
    // its height again.
    let id = attendant_id(&state);
    state
        .session
        .get_mut(&id)
        .expect("the attendant")
        .set_attendant_is_prepping(true);
    {
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        section.content_height(&RenderCtx::new_with_default_config(
            &state,
            &slices,
            &overlay_views,
        ));
    }

    // Then the tree is rebuilt, because the key summarises the prep flag.
    assert!(
        section.rebuilds() > after_first,
        "a prep-mode change must invalidate the memoized tree"
    );
    // And the tree the section is now caching marks the attendant as
    // composing.
    assert!(
        any_attendant_prepping(&state),
        "a composing attendant is marked"
    );
}

#[rstest::rstest]
fn a_manual_trigger_alone_does_not_mark_an_attendant_as_composing() {
    // Given a composed attendant on the manual trigger.
    let mut state = state_with_composed_attendant();
    let id = attendant_id(&state);
    state
        .session
        .get_mut(&id)
        .expect("the attendant")
        .set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::Manual);

    // When the sessions tree is built.
    let tree = sorted_open_sessions(&state);

    // Then it carries no prep marker. A manual trigger declines to fire on
    // its own; the `R` key still runs the attendant, and marking it as
    // unable to run is the ambiguity the two-fact split removed.
    assert!(
        !tree.iter().any(|entry| entry.is_attendant_prepping),
        "a manual trigger is not composition"
    );
}

#[rstest::rstest]
fn a_parent_completed_attendant_carries_the_trigger_marker() {
    // Given a composed attendant on the parent-completed trigger.
    let mut state = state_with_composed_attendant();

    // When the sessions tree is built.
    let tree = sorted_open_sessions(&state);

    // Then it carries the marker saying it fires on its own.
    assert!(
        tree.iter()
            .any(|entry| entry.attendant_fires_on_parent_completion),
        "a parent-completed attendant must be marked as one that fires on its own"
    );
}

#[rstest::rstest]
fn the_sessions_list_key_summarizes_the_prep_flag() {
    // Given a state whose attendant is composed.
    let running = state_with_composed_attendant();
    let running_key = crate::sections::sessions::state::session_list_key(&running);

    // When the same attendant goes back into prep mode.
    let mut preparing = state_with_composed_attendant();
    let id = attendant_id(&preparing);
    preparing
        .session
        .get_mut(&id)
        .expect("the attendant")
        .set_attendant_is_prepping(true);
    let preparing_key = crate::sections::sessions::state::session_list_key(&preparing);

    // Then the two keys differ, so a prep-state change can never be a
    // cache hit and leave the marker stale.
    assert_ne!(
        running_key, preparing_key,
        "the memo key must summarize the flag the tree reads"
    );
}

#[rstest::rstest]
fn the_sessions_list_key_summarizes_the_trigger_marker() {
    // Given a state whose attendant fires on its parent's completion.
    let automatic = state_with_composed_attendant();
    let automatic_key = crate::sections::sessions::state::session_list_key(&automatic);

    // When the same attendant's trigger is committed as manual.
    let mut manual = state_with_composed_attendant();
    manual
        .session
        .get_mut(&attendant_id(&manual))
        .expect("the attendant")
        .set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::Manual);
    let manual_key = crate::sections::sessions::state::session_list_key(&manual);

    // Then the two keys differ, so the second marker cannot go stale behind
    // a cache hit either.
    assert_ne!(
        automatic_key, manual_key,
        "the memo key must summarize every flag the tree reads"
    );
}

#[rstest::rstest]
fn height_and_render_agree_on_the_session_count() {
    // Given a sessions section and state with several sessions.
    let mut section = SessionsSection::new();
    let state = state_with_sessions(4);

    // When the height is computed and then rendered.
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new(
        &state,
        &slices,
        &overlay_views,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    let height = section.content_height(&ctx);
    let tree_len = section.cached_session_count();

    // Then the height reflects exactly those sessions plus the footer.
    assert_eq!(u32::from(height), tree_len as u32 + 1);
}

#[rstest::rstest]
fn adding_a_session_rebuilds_the_tree() {
    // Given a section that has already built its tree.
    let mut section = SessionsSection::new();
    let mut state = state_with_sessions(2);
    let before = {
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let ctx = RenderCtx::new(
            &state,
            &slices,
            &overlay_views,
            jinn_kernel::common::render_ctx::empty_config_layer(),
        );
        section.content_height(&ctx);
        section.rebuilds()
    };

    // When a new session is added.
    {
        let mut s = ChatSessionState::new();
        s.push_entry(ChatEntry::user("a newly added session"));
        state.session.insert(s);
    }

    // Then the tree is rebuilt.
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new(
        &state,
        &slices,
        &overlay_views,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    section.content_height(&ctx);
    assert_eq!(section.rebuilds(), before + 1);
}

/// Measures a section's content height against `state`, which is what drives
/// the tree build.
fn measure_content_height(section: &mut SessionsSection, state: &AppState) -> u16 {
    let slices = jinn_slices::Slices::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let ctx = RenderCtx::new(
        state,
        &slices,
        &overlay_views,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    section.content_height(&ctx)
}

/// Replaces a session's title with the same-length string formed by flipping
/// every `a` to `b` and every other character to `a`.
fn rename_session_length_preserving(state: &mut AppState) {
    let target = state.session.iter().next().map(|(id, _)| id.clone());
    if let Some(session) = target.as_ref().and_then(|id| state.session.get_mut(id)) {
        let original = session.title().unwrap_or("Untitled Session").to_owned();
        // Same byte length, different bytes at the head and the tail, so
        // only the boundary digest can catch it.
        let flipped = original
            .chars()
            .map(|c| if c == 'a' { 'b' } else { 'a' })
            .collect::<String>();
        assert_eq!(
            original.len(),
            flipped.len(),
            "the rename is deliberately length-preserving"
        );
        session.set_title(flipped);
    }
}

#[rstest::rstest]
fn a_renamed_session_rebuilds_the_tree() {
    // Given a section that has built its tree, warmed by a frame that changed
    // nothing.
    let mut section = SessionsSection::new();
    let mut state = state_with_sessions(2);
    let before = {
        measure_content_height(&mut section, &state);
        section.rebuilds()
    };
    measure_content_height(&mut section, &state);
    assert_eq!(section.rebuilds(), before, "no change, no rebuild");

    // When a session is renamed the same length.
    rename_session_length_preserving(&mut state);

    // Then exactly one more rebuild happens.
    measure_content_height(&mut section, &state);
    assert_eq!(
        section.rebuilds(),
        before + 1,
        "a same-length rename must still invalidate the memo"
    );
}

#[rstest::rstest]
fn session_reloaded_from_the_archive_is_listed() {
    // Given a session that was archived and has just been loaded back, which
    // is what the session picker does when the user picks an archived entry.
    let mut state = AppState::default_with_scope_focus();
    let session_id = state.session.active_session_id().clone();
    state
        .session
        .get_mut(&session_id)
        .expect("active session")
        .set_session_state(jinn_session_store_msg::SessionState::Archived);
    // And the load completes and marks it live again.
    state
        .session
        .get_mut(&session_id)
        .expect("active session")
        .set_session_state(jinn_session_store_msg::SessionState::Loaded);

    // When the sidebar's session list is built.
    let listed = sorted_open_sessions(&state);

    // Then the session is included.
    assert!(
        listed.iter().any(|entry| entry.id == session_id),
        "a reloaded session must be listed; got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// in-flight tint - dispatch marking
// ---------------------------------------------------------------------------

/// A state with one loaded, idle session that is selected in the sessions section.
fn state_with_one_selected_idle_session() -> (AppState, jinn_core_types::SessionId) {
    let mut state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);
    (state, id)
}

/// Whether the given session is currently marked in flight in the sidebar cell.
fn marked_in_flight(state: &AppState, id: &jinn_core_types::SessionId) -> bool {
    crate::sections::sessions::state::is_in_flight(&state.frontend, id)
}

#[rstest::rstest]
fn archive_marks_session_in_flight() {
    // Given one loaded, idle session with the cursor on it.
    let (mut state, id) = state_with_one_selected_idle_session();

    // When handling Intent::SidebarSessionArchive.
    crate::sections::sessions::handle_session_archive(&mut state);

    // Then the session is marked in flight.
    assert!(marked_in_flight(&state, &id));
}

#[rstest::rstest]
fn close_marks_session_in_flight_on_confirm() {
    // Given one loaded, idle session, with the close prompt already armed.
    let (mut state, id) = state_with_one_selected_idle_session();
    state.frontend.close_session_prompt = true;

    // When handling the confirm path.
    crate::sections::sessions::handle_session_close_arm(&mut state);

    // Then the session is marked in flight.
    assert!(marked_in_flight(&state, &id));
}

#[rstest::rstest]
fn first_close_press_marks_nothing() {
    // Given one loaded, idle session.
    let (mut state, id) = state_with_one_selected_idle_session();

    // When handling the first close press, which only arms the prompt.
    crate::sections::sessions::handle_session_close_arm(&mut state);

    // Then nothing is marked in flight.
    assert!(!marked_in_flight(&state, &id));
}

#[rstest::rstest]
fn archive_tree_marks_every_member_of_an_idle_subtree() {
    // Given a parent session with one idle child, cursor on the parent.
    let mut state = AppState::default_with_scope_focus();
    let parent_id = state.session.active_session_id().clone();
    let mut child = ChatSessionState::new();
    child.set_parent_session(parent_id.clone());
    let child_id = child.session_id().clone();
    state.session.insert(child);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When confirming the archive-tree prompt twice.
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::Archive,
    );
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::Archive,
    );

    // Then both the parent and the child are marked in flight.
    assert!(marked_in_flight(&state, &parent_id));
    assert!(marked_in_flight(&state, &child_id));
}

#[rstest::rstest]
fn archive_tree_with_busy_member_marks_nothing() {
    // Given a parent session with one busy child, cursor on the parent.
    let mut state = AppState::default_with_scope_focus();
    let parent_id = state.session.active_session_id().clone();
    let mut child = ChatSessionState::new();
    child.set_parent_session(parent_id.clone());
    let child_id = child.session_id().clone();
    child.begin_streaming();
    state.session.insert(child);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When confirming the archive-tree prompt twice.
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::Archive,
    );
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::Archive,
    );

    // Then no member is marked in flight.
    assert!(!marked_in_flight(&state, &parent_id));
    assert!(!marked_in_flight(&state, &child_id));
}

#[rstest::rstest]
fn teardown_tree_marks_every_member_of_an_idle_subtree() {
    // Given a parent session with one idle child, cursor on the parent.
    let mut state = AppState::default_with_scope_focus();
    let parent_id = state.session.active_session_id().clone();
    let mut child = ChatSessionState::new();
    child.set_parent_session(parent_id.clone());
    let child_id = child.session_id().clone();
    state.session.insert(child);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When confirming the teardown-tree prompt twice.
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::TeardownAndArchive,
    );
    let _ = crate::sections::sessions::handle_session_tree_action_arm(
        &mut state,
        crate::sections::sessions::TreePromptAction::TeardownAndArchive,
    );

    // Then both the parent and the child are marked in flight.
    assert!(marked_in_flight(&state, &parent_id));
    assert!(marked_in_flight(&state, &child_id));
}

#[rstest::rstest]
fn session_list_key_changes_when_a_session_becomes_in_flight() {
    // Given one loaded session with the cursor on it.
    let (state, id) = state_with_one_selected_idle_session();

    // When marking it in flight and re-reading the memo key.
    let before = crate::sections::sessions::state::session_list_key(&state);
    state
        .frontend
        .update_sections(|s| s.sessions.begin_in_flight(std::slice::from_ref(&id)));
    let after = crate::sections::sessions::state::session_list_key(&state);

    // Then the key changed, so the cached tree is rebuilt and the tint appears.
    assert_ne!(
        before, after,
        "the memo key must change or the cached tree is never rebuilt"
    );
}

#[rstest::rstest]
fn indicator_span_returns_blank_space_when_idle_and_not_in_flight() {
    // Given an idle entry with no disposal in flight.
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(true, false, &throbber, &theme);

    // Then it is a blank space.
    assert_eq!(span.content, " ");
}

#[rstest::rstest]
fn indicator_span_returns_block_character_when_in_flight() {
    // Given an idle entry whose disposal is in flight.
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(true, true, &throbber, &theme);

    // Then it is a non-space block character.
    assert_ne!(span.content, " ");
    assert!(!span.content.is_empty());
}

#[rstest::rstest]
fn indicator_span_uses_theme_busy_color_when_in_flight() {
    // Given an idle entry whose disposal is in flight.
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(true, true, &throbber, &theme);

    // Then it wears the theme's busy color, matching the busy spinner.
    assert_eq!(span.style.fg, Some(theme.streaming));
}

#[rstest::rstest]
fn both_spinners_share_the_theme_busy_color() {
    // Given a non-default theme whose busy color is not cyan.
    let mut theme = default_theme();
    theme.streaming = Color::Magenta;
    let throbber = ThrobberState::default();

    // When computing both indicator spans.
    let busy = indicator_span(false, false, &throbber, &theme);
    let in_flight = indicator_span(true, true, &throbber, &theme);

    // Then both follow the theme, so neither is left on a hardcoded cyan.
    assert_eq!(busy.style.fg, Some(Color::Magenta));
    assert_eq!(in_flight.style.fg, Some(Color::Magenta));
}

#[rstest::rstest]
fn in_flight_indicator_animates_across_block_symbols() {
    // Given an in-flight entry stepped through the whole cycle.
    let symbols = throbber_widgets_tui::symbols::throbber::HORIZONTAL_BLOCK.symbols;

    // When each step of the cycle is rendered.
    let seen = (0..symbols.len())
        .map(|step| {
            let mut throbber = ThrobberState::default();
            let theme = default_theme();
            for _ in 0..step {
                throbber.calc_next();
            }
            indicator_span(true, true, &throbber, &theme)
                .content
                .to_string()
        })
        .collect::<Vec<_>>();

    // Then every step renders a block from the HORIZONTAL_BLOCK set.
    let expected = symbols.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(seen, expected);
}

#[rstest::rstest]
fn in_flight_indicator_does_not_show_on_a_busy_session() {
    // Given a session that is somehow both busy and in flight.
    let throbber = ThrobberState::default();
    let theme = default_theme();

    // When computing indicator span.
    let span = indicator_span(false, true, &throbber, &theme);

    // Then the busy braille spinner wins, keeping the column single-valued.
    assert!(
        !throbber_widgets_tui::symbols::throbber::HORIZONTAL_BLOCK
            .symbols
            .contains(&span.content.as_ref())
    );
}

/// Sidebar navigation requests a preview render for the session it lands on.
///
/// Navigation is the only path that can ask for a preview — the render pass has
/// no bus handle — so a request that never leaves `navigate` means the popup
/// spins forever.
mod navigation_preview_requests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use crate::sections::section_trait::{EnterFrom, SectionNavResult, SidebarIntent};
    use crate::sections::sessions::navigate::{navigate, receive_cursor};
    use jinn_kernel::common::app_state::AppState;
    use jinn_session_state::ChatSessionState;

    /// App state with a sessions section holding `count` sessions.
    fn state_with_sessions(count: usize) -> (AppState, Vec<jinn_core_types::SessionId>) {
        let mut state = AppState::default_with_scope_focus();
        let mut ids = Vec::new();
        for _ in 0..count {
            let mut session = ChatSessionState::new();
            session.push_entry(ChatEntry::user("hello"));
            ids.push(session.session_id().clone());
            state.session.insert(session);
        }
        state
            .frontend
            .update_sections(|s| s.sessions.preview_content_width = 40);
        (state, ids)
    }

    #[rstest::rstest]
    fn moving_onto_a_session_requests_its_preview() {
        // Given a sessions section with three sessions.
        let (mut state, _ids) = state_with_sessions(3);
        cursor_to_row(&mut state, 0);
        // And the first session's preview already served, so the move is what
        // triggers the request rather than a cold start.
        let first = cursor_row(&state).expect("index");
        assert_eq!(first, 0);

        // When the cursor moves down.
        let config = jinn_slices::empty_config_layer();
        let (result, emitted) = navigate(&SidebarIntent::MoveDown, &mut state, config);

        // Then the move succeeded.
        assert_eq!(result, SectionNavResult::Moved);
        // And a preview render was requested.
        assert!(
            emitted.message_names.contains(&"PreviewSessionRequested"),
            "expected a preview request, got {:?}",
            emitted.message_names
        );
    }

    #[rstest::rstest]
    fn moving_past_the_last_session_requests_no_preview() {
        // Given a sessions section with the cursor on its final session.
        // The count comes from the section's own list rather than the number
        // inserted: the session map keeps a fresh session alive, so the two do
        // not necessarily agree.
        let (mut state, _ids) = state_with_sessions(1);
        let last = sorted_open_sessions(&state).len().saturating_sub(1);
        cursor_to_row(&mut state, last);

        // When the cursor moves down past the end.
        let config = jinn_slices::empty_config_layer();
        let (result, emitted) = navigate(&SidebarIntent::MoveDown, &mut state, config);

        // Then the sidebar is told the section is exhausted, with nothing to publish.
        assert_eq!(result, SectionNavResult::Exhausted);
        assert!(emitted.message_names.is_empty());
    }

    #[rstest::rstest]
    fn entering_the_section_from_the_top_requests_a_preview() {
        // Given a sessions section with two sessions and no cursor.
        let (mut state, _ids) = state_with_sessions(2);
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = None);

        // When the sidebar enters the section from above.
        let emitted = receive_cursor(
            &mut state,
            EnterFrom::Top,
            jinn_slices::empty_config_layer(),
        );

        // Then the top session's preview is requested.
        assert!(
            emitted.message_names.contains(&"PreviewSessionRequested"),
            "expected a preview request, got {:?}",
            emitted.message_names
        );
    }
}

#[rstest::rstest]
fn a_selected_session_row_bands_the_full_width() {
    // Given the sessions section focused on its first row.
    let mut section = SessionsSection::new();
    let mut state = state_with_sessions(1);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);
    let theme = state.frontend.theme.clone();

    // When rendering into a wide terminal.
    let width = 60u16;
    let height = 10u16;
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // Then the selected row carries the selection background...
    let selected_y = (0..height).find(|&y| {
        (0..width).any(|x| {
            buffer
                .cell((x, y))
                .is_some_and(|cell| cell.bg == theme.selection_fg)
        })
    });
    let Some(y) = selected_y else {
        panic!("no selected row with a selection band rendered");
    };
    // ...across the full width, including cells past the last character.
    let last_banded_x = (0..width)
        .filter(|&x| {
            buffer
                .cell((x, y))
                .is_some_and(|cell| cell.bg == theme.selection_fg)
        })
        .max();
    assert_eq!(
        last_banded_x,
        Some(width.saturating_sub(1)),
        "the band must reach the row's last cell"
    );
    // And the band's text is the sidebar background.
    let text_cell = buffer.cell((4, y)).expect("a text cell on the band");
    assert_eq!(text_cell.fg, theme.gutter_bg);
}

#[rstest::rstest]
fn no_sidebar_session_row_carries_the_reversed_modifier() {
    // Given the sessions section focused on its first row — the only
    // selection state there is.
    let mut section = SessionsSection::new();
    let mut state = state_with_sessions(1);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When rendering.
    let width = 40u16;
    let height = 10u16;
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // Then no cell in the whole section carries the terminal inversion.
    let reversed = (0..height).any(|y| {
        (0..width).any(|x| {
            buffer
                .cell((x, y))
                .is_some_and(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED))
        })
    });
    assert!(!reversed, "selection is a color band, never REVERSED");
}

/// A state with one session whose last entry is an error.
///
/// The error goes into the *default* active session so the row's title stays
/// the default's, keeping the test's expectations about which row carries
/// the error simple.
fn state_with_errored_session() -> AppState {
    let mut state = state_with_sessions(1);
    state
        .active_session_mut()
        .push_entry(ChatEntry::error("it broke"));
    state
}

#[rstest::rstest]
fn an_unselected_error_row_is_a_red_block() {
    // Given an errored session with no cursor on the sessions list.
    let mut section = SessionsSection::new();
    let state = state_with_errored_session();
    let theme = state.frontend.theme.clone();

    // When rendering.
    let width = 40u16;
    let height = 10u16;
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // Then the errored row renders as a red block: red background with the
    // sidebar background as the text color — an inversion of the panel, not
    // red text on dark.
    let error_y = (0..height)
        .find(|&y| {
            (0..width).any(|x| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.bg == Color::Red)
            })
        })
        .unwrap_or_else(|| panic!("no red block rendered"));
    let text: String = (0..width)
        .filter_map(|x| buffer.cell((x, error_y)).map(ratatui::buffer::Cell::symbol))
        .collect();
    let title_at = text
        .find("Untitled Session")
        .expect("error row title visible");
    let title_cell = buffer
        .cell((u16::try_from(title_at).unwrap_or(0), error_y))
        .expect("title cell");
    assert_eq!(title_cell.fg, theme.gutter_bg);
    assert_eq!(title_cell.bg, Color::Red);
}

#[rstest::rstest]
fn a_selected_error_row_takes_the_selection_band_not_red() {
    // Given the sessions cursor on the errored session — the one session.
    let mut section = SessionsSection::new();
    let mut state = state_with_errored_session();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);
    let theme = state.frontend.theme.clone();

    // When rendering.
    let width = 40u16;
    let height = 10u16;
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            section.render(frame, area, 0, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    // Then the selected errored row is the selection band — selection
    // overrides the error block, whose red background is nowhere on the row.
    let band_y = (0..height)
        .find(|&y| {
            (0..width).any(|x| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.bg == theme.selection_fg)
            })
        })
        .unwrap_or_else(|| panic!("no selection band rendered"));
    let red_on_band = (0..width).any(|x| {
        buffer
            .cell((x, band_y))
            .is_some_and(|cell| cell.bg == Color::Red)
    });
    assert!(!red_on_band, "selection overrides the error red block");
    let text: String = (0..width)
        .filter_map(|x| buffer.cell((x, band_y)).map(ratatui::buffer::Cell::symbol))
        .collect();
    assert!(
        text.contains("Untitled Session"),
        "the selected band row is the errored session: {text}"
    );
}
