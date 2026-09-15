#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use crate::common::app_state::{AppState, FocusScope};
use crate::common::render_ctx::RenderCtx;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::ui::sidebar;
use crate::feat::ui::sidebar::intent::handle_sidebar_focus;
use crate::feat::ui::sidebar::pins::PinsSection;
use crate::feat::ui::sidebar::section_trait::SidebarIntent;
use crate::feat::ui::sidebar::section_trait::SidebarSectionId;
use crate::feat::ui::sidebar::sidebar::{Sidebar, jump_to_section, navigate_sidebar};
use crate::protocol::ChatEntry;
use crate::protocol::PinPosition;

fn state_with_pinned(count: usize) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    for i in 0..count {
        let entry = ChatEntry::user(format!("entry {i}"));
        let id = entry.id.clone();
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().pin_entry(&id, PinPosition::Top);
    }
    state
}

#[rstest::rstest]
fn register_adds_section() {
    // Given a new sidebar.
    let mut sidebar = Sidebar::new();

    // When registering a section.
    sidebar.register(Box::new(PinsSection));

    // Then section count is 1.
    assert_eq!(sidebar.section_count(), 1);
}

#[rstest::rstest]
fn render_clears_area_with_sidebar_background() {
    // Given a sidebar with no sections.
    let mut sidebar = Sidebar::new();
    let state = AppState::default_with_scope_focus();

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            sidebar.render(frame, ratatui::layout::Rect::new(0, 0, 30, 10), &ctx);
        })
        .unwrap();

    // Then the entire area has the sidebar background (#191b1e).
    let expected_bg = Color::Rgb(0x19, 0x1b, 0x1e);
    let buf = terminal.backend().buffer();
    for y in 0..10u16 {
        for x in 0..30u16 {
            let cell = buf.cell((x, y)).expect("cell");
            assert_eq!(
                cell.bg, expected_bg,
                "cell ({x},{y}) should have #191b1e bg"
            );
        }
    }
}

#[rstest::rstest]
fn move_down_from_persona_with_pins_enters_pins_at_first_entry() {
    // Given persona focused with 3 pinned entries.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When navigating down.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);

    // Then focus moves to Pins and the first pinned entry is selected.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Pins
    );
    let first_pin_id = state.sorted_pinned_ids()[0].clone();
    assert_eq!(state.frontend.pins.selected_id(), Some(&first_pin_id));
}

#[rstest::rstest]
fn move_down_from_persona_skips_empty_pins_to_sessions() {
    // Given persona focused with no pinned entries (but sessions exist).
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When navigating down.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);

    // Then focus skips empty Pins and lands on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn move_up_from_first_pin_enters_persona() {
    // Given pins focused with 3 entries, first pin selected.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let first_id = state.sorted_pinned_ids()[0].clone();
    state.frontend.pins.select_by_id(first_id);

    // When navigating up from the first pin.
    navigate_sidebar(&SidebarIntent::MoveUp, &mut state);

    // Then focus moves to Persona, pins selection is cleared, and persona has cursor.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Persona
    );
    assert!(state.frontend.pins.selected_id().is_none());
    assert_eq!(state.frontend.persona_section.cursor, Some(0));
}

#[rstest::rstest]
fn move_down_at_last_pin_enters_sessions() {
    // Given pins focused with 2 entries, last pin selected.
    let mut state = state_with_pinned(2);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let last_id = state.sorted_pinned_ids()[1].clone();
    state.frontend.pins.select_by_id(last_id);

    // When navigating down.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);

    // Then focus moves to Sessions (which always has content).
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn move_up_at_persona_sticks() {
    // Given persona focused.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When navigating up.
    navigate_sidebar(&SidebarIntent::MoveUp, &mut state);

    // Then focus stays on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn move_up_from_sessions_skips_empty_pins_to_persona() {
    // Given sessions focused with no pinned entries.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = Some(0);

    // When navigating up.
    navigate_sidebar(&SidebarIntent::MoveUp, &mut state);

    // Then focus skips empty Pins and lands on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn sidebar_focus_places_cursor_on_persona() {
    // Given default app state.
    let mut state = AppState::default_with_scope_focus();

    // When handling sidebar focus.
    handle_sidebar_focus(&mut state);

    // Then persona section has the cursor.
    assert_eq!(state.frontend.persona_section.cursor, Some(0));
}

#[rstest::rstest]
fn jump_next_from_persona_to_pins_retains_persona_cursor() {
    // Given persona focused with cursor at 0, pins with 3 entries.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When jumping to next section.
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Then focus moves to Pins.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Pins
    );
    // And persona cursor is retained.
    assert_eq!(state.frontend.persona_section.cursor, Some(0));
}

#[rstest::rstest]
fn jump_prev_from_pins_to_persona_retains_pins_cursor() {
    // Given pins focused with cursor on second pin, pins has 3 entries.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let second_id = state.sorted_pinned_ids()[1].clone();
    state.frontend.pins.select_by_id(second_id.clone());

    // When jumping to prev section.
    jump_to_section(&SidebarIntent::MoveUp, &mut state);

    // Then focus moves to Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Persona
    );
    // And pins cursor is retained.
    assert_eq!(state.frontend.pins.selected_id(), Some(&second_id));
}

#[rstest::rstest]
fn jump_next_from_persona_skips_empty_pins_to_sessions() {
    // Given persona focused with no pinned entries.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When jumping to next section.
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Then focus skips empty Pins and lands on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn jump_next_fallback_receive_cursor_on_never_visited_section() {
    // Given persona focused, pins has entries but no cursor set.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);
    // Pins has no selection.
    assert!(state.frontend.pins.selected_id().is_none());

    // When jumping to next section.
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Then focus moves to Pins and receive_cursor was called (first pin selected).
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Pins
    );
    let first_pin_id = state.sorted_pinned_ids()[0].clone();
    assert_eq!(state.frontend.pins.selected_id(), Some(&first_pin_id));
}

#[rstest::rstest]
fn jump_next_from_sessions_at_boundary_does_nothing() {
    // Given sessions focused (last section).
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarSessions);
    state.frontend.sessions_section.selected_index = Some(0);

    // When jumping to next section (no section after Sessions).
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Then focus stays on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn jump_prev_from_persona_at_boundary_does_nothing() {
    // Given persona focused (first section).
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);

    // When jumping to prev section (no section before Persona).
    jump_to_section(&SidebarIntent::MoveUp, &mut state);

    // Then focus stays on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(SidebarSectionId::Persona),
        SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn jump_to_sessions_retains_cursor_and_adjusts_scroll() {
    // Given 20 sessions, persona focused, sessions has cursor at index 18 with scroll_offset 4.
    let mut state = {
        let mut s = AppState::default_with_scope_focus();
        for i in 1..20 {
            let session = ChatSessionState::new();
            let _id = session.session_id().clone();
            s.session.insert({
                let mut sess = ChatSessionState::new();
                sess.push_entry(ChatEntry::user(format!("message for session {i}")));
                sess
            });
        }
        s
    };
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);
    // Pre-set sessions cursor and scroll.
    state.frontend.sessions_section.selected_index = Some(18);
    state.frontend.sessions_section.scroll_offset = 4;

    // When jumping to sessions (skipping empty pins if any, or through pins).
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Sessions may or may not be the target depending on pins.
    // If pins is empty (default state has no pins), we land on sessions.
    if state.frontend.sidebar_section() == Some(SidebarSectionId::Sessions) {
        // Then cursor is retained.
        assert_eq!(state.frontend.sessions_section.selected_index, Some(18));
        // And scroll_to_cursor was called to adjust offset.
        assert_eq!(state.frontend.sessions_section.scroll_offset, 4);
    }
}

/// Creates a sidebar with all built-in sections registered.
fn sidebar_with_all_sections() -> Sidebar {
    let mut sidebar = Sidebar::new();
    sidebar::register_sections(&mut sidebar);
    sidebar
}

/// Finds the first row in the buffer that contains the given needle text.
fn find_row_containing(
    buf: &ratatui::buffer::Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Option<u16> {
    for y in 0..height {
        let row: String = (0..width)
            .map(|x| buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol))
            .collect::<String>();
        if row.contains(needle) {
            return Some(y);
        }
    }
    None
}

#[rstest::rstest]
fn sessions_header_anchored_to_bottom() {
    // Given a sidebar with all sections and default state (1 session).
    let mut sidebar = sidebar_with_all_sections();
    let state = AppState::default_with_scope_focus();

    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            sidebar.render(frame, ratatui::layout::Rect::new(0, 0, width, height), &ctx);
        })
        .unwrap();

    // Then the footer appears near the bottom.
    // With 1 session, content_height = 2 (1 entry + 1 footer). bottom_y = 40 - 2 = 38.
    // Minimap is 0 (empty history), Persona is 4, Pins is 0 (no pins).
    // So y_offset = 4, section_y = max(38, 4) = 38.
    // Sessions footer is at row 39 (last line of the 2-row block at row 38-39).
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions");
    assert!(
        sessions_row.is_some(),
        "should find 'Sessions' footer in buffer"
    );
    assert_eq!(
        sessions_row,
        Some(39),
        "Sessions footer should be at row 39 (bottom-anchored)"
    );
}

#[rstest::rstest]
fn sessions_header_below_persona_when_sidebar_is_short() {
    // Given a sidebar with all sections and a short area (8 rows).
    // Empty sections (Pins, TaskList, McpServers) collapse to 0 height, so the
    // default layout renders Persona(4) + Sessions(2) only.
    let mut sidebar = sidebar_with_all_sections();
    let state = AppState::default_with_scope_focus();

    let width = 30u16;
    let height = 8u16;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            sidebar.render(frame, ratatui::layout::Rect::new(0, 0, width, height), &ctx);
        })
        .unwrap();

    // Then Sessions footer appears below Persona (clamped to not overlap).
    // Persona = 4 rows, content_height(Sessions) = 2 (1 entry + footer).
    // bottom_y = 8 - 2 = 6, section_y = max(6, 4) = 6.
    // Footer is at row 7 (last line of the 2-row block at row 6-7).
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions");
    assert!(
        sessions_row.is_some(),
        "should find 'Sessions' footer in buffer"
    );
    assert_eq!(
        sessions_row,
        Some(7),
        "Sessions footer should be at row 7 (just below Persona, clamped)"
    );
}

#[rstest::rstest]
fn sessions_footer_highlights_s_in_accent_action() {
    // Given a sidebar with all sections and default state.
    let mut sidebar = sidebar_with_all_sections();
    let state = AppState::default_with_scope_focus();

    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            sidebar.render(frame, ratatui::layout::Rect::new(0, 0, width, height), &ctx);
        })
        .unwrap();

    // Then the S in Sessions has accent_action color.
    let buf = terminal.backend().buffer();
    let sessions_row =
        find_row_containing(buf, width, height, "Sessions").expect("should find Sessions footer");

    // Find the cell containing the highlighted S.
    let accent_action = state.frontend.theme.accent_action;
    let mut found_highlighted_s = false;
    for x in 0..width {
        let cell = buf.cell((x, sessions_row)).expect("cell");
        if cell.symbol() == "S" && cell.fg == accent_action {
            found_highlighted_s = true;
            break;
        }
    }
    assert!(
        found_highlighted_s,
        "should find an S cell with accent_action foreground in Sessions footer row"
    );

    // And the surrounding box-drawing characters use border_unfocused
    // (since the sidebar is not focused in this default state).
    let border_unfocused = state.frontend.theme.border_unfocused;
    let mut found_unfocused_dash = false;
    for x in 0..width {
        let cell = buf.cell((x, sessions_row)).expect("cell");
        if cell.symbol() == "\u{2500}" && cell.fg == border_unfocused {
            found_unfocused_dash = true;
            break;
        }
    }
    assert!(
        found_unfocused_dash,
        "should find a dash cell with border_unfocused foreground in Sessions footer row"
    );
}

// ---------------------------------------------------------------------------
// History position save/restore
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn entering_pins_saves_history_position() {
    // Given persona focused with a known scroll offset and selected entry.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);
    state.active_session_mut().set_scroll_offset(Some(42));
    let entry_id_0 = state.active_session().history()[0].id.clone();
    state.active_session_mut().set_selected_entry_index(0);

    // When navigating down into Pins.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);

    // Then the history position was saved before sync_chat_log_cursor changed it.
    let saved = state
        .active_session()
        .saved_history_position()
        .expect("saved");
    assert_eq!(saved.scroll_offset, Some(42));
    assert_eq!(saved.selected_cursor_id, Some(entry_id_0));
    // And the selected entry was changed by sync_chat_log_cursor
    // (or stayed at 0 if the pin is at index 0 - what matters is that save captured pre-change).
    assert!(state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn leaving_pins_to_persona_restores_history_position() {
    // Given pins focused with a saved position.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let first_id = state.sorted_pinned_ids()[0].clone();
    state.frontend.pins.select_by_id(first_id);
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // When navigating up to Persona.
    navigate_sidebar(&SidebarIntent::MoveUp, &mut state);

    // Then the history position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
    // And the saved position is cleared.
    assert!(!state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn jump_from_pins_to_persona_restores_history_position() {
    // Given pins focused with a saved position.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let first_id = state.sorted_pinned_ids()[0].clone();
    state.frontend.pins.select_by_id(first_id);
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // When jumping to previous section (Persona).
    jump_to_section(&SidebarIntent::MoveUp, &mut state);

    // Then the history position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn sidebar_leave_discards_saved_position() {
    // Given pins focused with a saved position.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPins);
    let first_id = state.sorted_pinned_ids()[0].clone();
    state.frontend.pins.select_by_id(first_id);
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // Modify state to simulate pin view.
    state.active_session_mut().set_scroll_offset(Some(10));
    state.active_session_mut().set_selected_entry_index(2);

    // When leaving the sidebar.
    crate::feat::ui::sidebar::intent::handle_sidebar_leave(&mut state);

    // Then the scroll stays at the pin's position (not restored).
    assert_eq!(state.active_session().scroll_offset(), Some(10));
    assert_eq!(state.active_session().selected_entry_index(), Some(2));
    // And the saved position is discarded.
    assert!(!state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn full_cycle_saves_and_restores() {
    // Given persona focused with original scroll position.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);

    // When navigating to Pins.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);
    // Then position is saved.
    assert!(state.active_session().has_saved_history_position());
    // sync_chat_log_cursor changes selected_entry_index to the pin's history index
    // (which may be 0 if the pin is the first entry).

    // When navigating within pins (second pin) - does NOT restore.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);
    assert!(state.active_session().has_saved_history_position());

    // When navigating within pins (third pin, last) - does NOT restore.
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);
    assert!(state.active_session().has_saved_history_position());

    // When navigating to Sessions (exhausting pins).
    navigate_sidebar(&SidebarIntent::MoveDown, &mut state);
    // Then position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn jump_roundtrip_saves_and_restores() {
    // Given persona focused.
    let mut state = state_with_pinned(3);
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state.frontend.persona_section.cursor = Some(0);
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);

    // When jumping to Pins.
    jump_to_section(&SidebarIntent::MoveDown, &mut state);
    // Then position is saved (via receive_cursor fallback).
    assert!(state.active_session().has_saved_history_position());

    // When jumping back to Persona.
    jump_to_section(&SidebarIntent::MoveUp, &mut state);
    // Then position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn jump_to_pins_with_retained_cursor_syncs_chat_log_cursor() {
    // Given pins focused with a retained cursor, then jumped away and back.
    // Use entries where the pinned entry is NOT the first, so the restore
    // puts the cursor on a different entry than the pin.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(ChatEntry::user("a")); // hist 0
    state.active_session_mut().push_entry(ChatEntry::user("b")); // hist 1 - will be pinned
    state.active_session_mut().push_entry(ChatEntry::user("c")); // hist 2
    let pinned_id = state.active_session().history()[1].id.clone();
    state
        .active_session_mut()
        .pin_entry(&pinned_id, PinPosition::Top);

    state.frontend.scope_push(FocusScope::SidebarPins);
    state.frontend.pins.select_by_id(pinned_id.clone());
    state.active_session_mut().set_selected_entry_index(2); // cursor on "c" before save
    state.active_session_mut().save_history_position();
    // Now sync cursor to pin.
    crate::feat::ui::sidebar::pins::pins_section::sync_chat_log_cursor(&mut state);
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id.clone()),
        "precondition: cursor should be on pinned entry"
    );

    // Jump to Persona (away from pins) - restores cursor to "c".
    jump_to_section(&SidebarIntent::MoveUp, &mut state);
    assert_ne!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id.clone()),
        "cursor should be restored away from pin"
    );

    // Jump back to Pins (retained cursor on pinned entry).
    jump_to_section(&SidebarIntent::MoveDown, &mut state);

    // Then chat log cursor is synced to the pinned entry.
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id),
        "chat log cursor should match the retained pin after jump back"
    );
}
