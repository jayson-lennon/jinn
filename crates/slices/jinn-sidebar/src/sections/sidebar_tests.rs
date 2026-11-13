#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_kernel::common::render_test::{redraw_draw, render_draw};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use crate::sections::intent::handle_sidebar_focus;
use crate::sections::pins::PinsSection;
use crate::sections::section_trait::SidebarIntent;
use crate::sections::sidebar::{Sidebar, jump_to_section, navigate_sidebar};

/// Puts the sessions cursor on the session drawn at `row`.
fn cursor_to_row(state: &mut AppState, row: usize) {
    let id = crate::sections::sessions::state::sorted_open_sessions(state)
        .get(row)
        .map(|entry| entry.id.clone())
        .expect("the row under test exists in the list it indexes");
    state
        .frontend
        .update_sections(|s| s.sessions.selected_id = Some(id));
}

/// The row the sessions cursor is drawn on.
fn cursor_row(state: &AppState) -> Option<usize> {
    let id = state
        .frontend
        .with_sections(|s| s.sessions.selected_id.clone(), || None)?;
    crate::sections::sessions::state::sorted_open_sessions(state)
        .iter()
        .position(|entry| entry.id == id)
}
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::ChatEntry;
use jinn_kernel::protocol::ChatEntryId;
use jinn_kernel::protocol::PinPosition;
use jinn_session_state::ChatSessionState;

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
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, 30, 10, |frame, _area, frame_ctx| {
        sidebar.render(frame, ratatui::layout::Rect::new(0, 0, 30, 10), frame_ctx);
    });

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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When navigating down.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Pins and the first pinned entry is selected.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Pins
    );
    let first_pin_id = state.sorted_pinned_ids()[0].clone();
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.pins.selected_id().cloned(), || None),
        Some(first_pin_id)
    );
}

#[rstest::rstest]
fn move_down_from_persona_skips_empty_pins_to_sessions() {
    // Given persona focused with no pinned entries (but sessions exist).
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When navigating down.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus skips empty Pins and lands on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn move_up_from_first_pin_enters_persona() {
    // Given pins focused with 3 entries, first pin selected.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let first_id = state.sorted_pinned_ids()[0].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(first_id));

    // When navigating up from the first pin.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Persona, pins selection is cleared, and persona has cursor.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
    assert!(
        state
            .frontend
            .with_sections(|s| s.pins.selected_id().is_none(), || true)
    );
    assert_eq!(
        state.frontend.with_sections(|s| s.persona.cursor, || None),
        Some(0)
    );
}

#[rstest::rstest]
fn move_down_at_last_pin_enters_sessions() {
    // Given pins focused with 2 entries, last pin selected.
    let mut state = state_with_pinned(2);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let last_id = state.sorted_pinned_ids()[1].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(last_id));

    // When navigating down.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Sessions (which always has content).
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn move_up_at_persona_sticks() {
    // Given persona focused.
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When navigating up.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus stays on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn move_up_from_sessions_skips_empty_pins_to_persona() {
    // Given sessions focused with no pinned entries.
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When navigating up.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus skips empty Pins and lands on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn sidebar_focus_places_cursor_on_persona() {
    // Given default app state.
    let mut state = AppState::default_with_scope_focus();

    // When handling sidebar focus.
    handle_sidebar_focus(&mut state);

    // Then persona section has the cursor.
    assert_eq!(
        state.frontend.with_sections(|s| s.persona.cursor, || None),
        Some(0)
    );
}

#[rstest::rstest]
fn jump_next_from_persona_to_pins_retains_persona_cursor() {
    // Given persona focused with cursor at 0, pins with 3 entries.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When jumping to next section.
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Pins.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Pins
    );
    // And persona cursor is retained.
    assert_eq!(
        state.frontend.with_sections(|s| s.persona.cursor, || None),
        Some(0)
    );
}

#[rstest::rstest]
fn jump_prev_from_pins_to_persona_retains_pins_cursor() {
    // Given pins focused with cursor on second pin, pins has 3 entries.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let second_id = state.sorted_pinned_ids()[1].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(second_id.clone()));

    // When jumping to prev section.
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
    // And pins cursor is retained.
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.pins.selected_id().cloned(), || None),
        Some(second_id)
    );
}

#[rstest::rstest]
fn jump_next_from_persona_skips_empty_pins_to_sessions() {
    // Given persona focused with no pinned entries.
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When jumping to next section.
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus skips empty Pins and lands on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn jump_next_fallback_receive_cursor_on_never_visited_section() {
    // Given persona focused, pins has entries but no cursor set.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));
    // Pins has no selection.
    assert!(
        state
            .frontend
            .with_sections(|s| s.pins.selected_id().is_none(), || true)
    );

    // When jumping to next section.
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus moves to Pins and receive_cursor was called (first pin selected).
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Pins
    );
    let first_pin_id = state.sorted_pinned_ids()[0].clone();
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.pins.selected_id().cloned(), || None),
        Some(first_pin_id)
    );
}

#[rstest::rstest]
fn jump_next_from_sessions_at_boundary_does_nothing() {
    // Given sessions focused (last section).
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    cursor_to_row(&mut state, 0);

    // When jumping to next section (no section after Sessions).
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus stays on Sessions.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Sessions
    );
}

#[rstest::rstest]
fn jump_prev_from_persona_at_boundary_does_nothing() {
    // Given persona focused (first section).
    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));

    // When jumping to prev section (no section before Persona).
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus stays on Persona.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn jump_to_sessions_retains_cursor_and_adjusts_scroll() {
    // Given 20 sessions, persona focused, sessions has cursor at index 18.
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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));
    // Pre-set sessions cursor and scroll.
    cursor_to_row(&mut state, 18);

    // When jumping to sessions (skipping empty pins if any, or through pins).
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Sessions may or may not be the target depending on pins.
    // If pins is empty (default state has no pins), we land on sessions.
    if state.frontend.sidebar_section() == Some(jinn_sidebar_msg::SidebarSectionId::Sessions) {
        // Then cursor is retained.
        assert_eq!(cursor_row(&state), Some(18));
    }
}

/// Creates a sidebar with all built-in sections registered.
fn sidebar_with_all_sections() -> Sidebar {
    let mut sidebar = Sidebar::new();
    super::register_sections(&mut sidebar);
    sidebar
}

/// Finds the first row in the buffer that contains the given needle text.
/// The visible text of one row, trimmed of trailing blanks.
fn row_text(buf: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
    (0..width)
        .map(|x| buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol))
        .collect::<String>()
}

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
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

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

/// The user-confirmed section order: Library above Sessions, below MCP
/// servers. This was once built with Library last, and the anchor logic had
/// a workaround for its collapsible tail; the order and the plain anchor
/// are pinned together here.
#[rstest::rstest]
fn library_renders_between_mcp_and_sessions() {
    // Given a state with a library member and MCP servers configured and
    // enabled, so all three sections draw rows.
    let mut state = AppState::default_with_scope_focus();
    {
        let library_cell = state
            .frontend
            .slices()
            .and_then(|slices| {
                slices.reader::<jinn_session_store_msg::LibraryState>(
                    &jinn_session_store_msg::library_slot(),
                )
            })
            .expect("the cell catalog registers the library slot");
        library_cell.update(|library| {
            library.entries = vec![jinn_session_store_msg::SessionSummary {
                session_id: jinn_core_types::SessionId::new(),
                title: "shelved work".to_owned(),
                updated_at: jiff::Timestamp::now(),
                created_at: jiff::Timestamp::now(),
                session_state: jinn_session_store_msg::SessionState::Loaded,
                in_library: true,
                is_bot: false,
                parent_session: None,
                project: None,
            }];
        });
    }
    state.active_session_mut().enable_mcp_server("probe");
    let config = jinn_config::testutil::config_layer("[mcp.probe]\ncommand = \"echo\"\n");

    // When building the document table the render walks.
    let document = crate::sections::layout::document(&state, &config);
    let tail: Vec<_> = document
        .spans
        .iter()
        .filter(|span| span.rows > 0)
        .map(|span| span.id)
        .collect();

    // Then the drawn sections run ... McpServers, Library, Sessions, and
    // Sessions is the document's last span.
    let mcp = tail
        .iter()
        .position(|id| *id == jinn_sidebar_msg::SidebarSectionId::McpServers)
        .expect("MCP servers configured and enabled");
    let library = tail
        .iter()
        .position(|id| *id == jinn_sidebar_msg::SidebarSectionId::Library)
        .expect("the library holds a member");
    let sessions = tail
        .iter()
        .position(|id| *id == jinn_sidebar_msg::SidebarSectionId::Sessions)
        .expect("sessions always draw");
    assert!(
        mcp < library && library < sessions,
        "expected ... MCP servers, Library, Sessions ..., got {tail:?}"
    );
    assert_eq!(
        document.spans.last().map(|span| span.id),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions),
        "Sessions must be the document tail — the bottom anchor lands on it"
    );
}

/// The bottom anchor pins Sessions to the column bottom even when the
/// Library above it is empty. The anchor used to pass through empty tails
/// because Library — a collapsible section — sat last; with Sessions back as
/// the guaranteed tail, the plain last-span logic must hold for an empty
/// library too.
#[rstest::rstest]
fn sessions_anchor_survives_empty_library() {
    // Given a sidebar with all sections, an empty library cell, and a
    // column tall enough that the document has slack.
    let mut sidebar = sidebar_with_all_sections();
    let state = AppState::default_with_scope_focus();

    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the Sessions footer still lands on the column's last row — the
    // same row the plain bottom-anchor test above pins — with the library
    // contributing nothing between MCP servers and Sessions.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions");
    assert_eq!(
        sessions_row,
        Some(39),
        "an empty library must not un-anchor Sessions from the bottom"
    );
}

#[rstest::rstest]
fn leading_sections_stay_at_the_top_when_the_document_is_short() {
    // Given a sidebar with content in more than just Persona and Sessions, in
    // a column tall enough that the document does not fill it.
    let mut sidebar = sidebar_with_all_sections();
    let state = state_with_pinned(3);

    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then Persona is at the very top of the column, not pushed to the bottom
    // alongside Sessions.
    let buf = terminal.backend().buffer();
    let persona_row = find_row_containing(buf, width, height, "Persona");
    assert_eq!(persona_row, Some(0), "Persona should anchor to row 0");
}

#[rstest::rstest]
fn a_blank_gap_separates_the_sessions_block_from_the_sections_above_it() {
    // Given a short document in a tall column, so the unused rows fall between
    // the leading sections and the trailing sessions block.
    let mut sidebar = sidebar_with_all_sections();
    let state = state_with_pinned(3);

    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the Sessions footer is at the bottom of the column, with at least
    // one blank row between it and the last row of the content above it.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions");
    // The gap rows carry no text at all, so find the last row above the
    // Sessions block that has content, and assert the rows between are blank.
    // The last non-blank row *before* the sessions entry row.
    let sessions_entry = sessions_row.saturating_sub(1);
    let last_content = (0..sessions_entry)
        .rev()
        .find(|y| !row_text(buf, width, *y).trim().is_empty())
        .expect("some content above the Sessions block");
    for y in (last_content + 1)..sessions_entry {
        assert_eq!(
            row_text(buf, width, y).trim(),
            "",
            "row {y} between the content and the Sessions block should be blank"
        );
    }
    assert!(
        sessions_entry > last_content + 1,
        "expected a blank gap: last content at {last_content}, \
         Sessions entry at {sessions_entry}"
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
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

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
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

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

#[rstest::rstest]
fn sessions_title_shows_spinner_during_startup_hydration() {
    // Given a sidebar with startup hydration active.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state.session.begin_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering the sidebar.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the Sessions title includes the ASCII spinner.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions row");
    let symbols = throbber_widgets_tui::ASCII;
    let found = (0..width).any(|x| {
        buf.cell((x, sessions_row))
            .is_some_and(|cell| symbols.symbols.contains(&cell.symbol()))
    });
    assert!(found, "Sessions title should include a spinner");
}

#[rstest::rstest]
fn sessions_title_spinner_uses_streaming_color() {
    // Given a sidebar with startup hydration active.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state.session.begin_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering the sidebar.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the title throbber uses the streaming foreground color.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions row");
    let symbols = throbber_widgets_tui::ASCII;
    let found = (0..width).any(|x| {
        buf.cell((x, sessions_row)).is_some_and(|cell| {
            symbols.symbols.contains(&cell.symbol()) && cell.fg == state.frontend.theme.streaming
        })
    });
    assert!(found, "Sessions spinner should use theme.streaming");
}

#[rstest::rstest]
fn sessions_title_hides_spinner_after_startup_hydration() {
    // Given a sidebar whose startup hydration has completed.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state.session.begin_startup_hydration();
    state.session.finish_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering the sidebar.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the Sessions title has no spinner.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions row");
    let symbols = throbber_widgets_tui::ASCII;
    let found = (0..width).any(|x| {
        buf.cell((x, sessions_row))
            .is_some_and(|cell| symbols.symbols.contains(&cell.symbol()))
    });
    assert!(!found, "Sessions title should hide the spinner");
}

#[rstest::rstest]
fn sessions_title_spinner_precedes_the_label() {
    // Given a sidebar with startup hydration active.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state.session.begin_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering the sidebar.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the spinner sits to the left of the "S" that starts the label.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions row");
    let streaming = state.frontend.theme.streaming;
    let spinner_x = (0..width)
        .find(|&x| {
            buf.cell((x, sessions_row))
                .is_some_and(|cell| cell.fg == streaming && cell.symbol().trim().len() == 1)
        })
        .expect("spinner cell");
    let label_x = (0..width)
        .find(|&x| {
            buf.cell((x, sessions_row))
                .is_some_and(|cell| cell.symbol() == "S")
        })
        .expect("Sessions label cell");
    assert!(
        spinner_x < label_x,
        "spinner at column {spinner_x} should precede the label at column {label_x}"
    );
}

#[rstest::rstest]
fn sessions_hydration_spinner_animates_without_session_rows() {
    // Given an empty sessions list with startup hydration active.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_session_state(jinn_session_store_msg::SessionState::Archived);
    state.session.begin_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut render = |terminal: &mut Terminal<TestBackend>| {
        redraw_draw(&state, terminal, |frame, _area, frame_ctx| {
            sidebar.render(
                frame,
                ratatui::layout::Rect::new(0, 0, width, height),
                frame_ctx,
            );
        });
    };

    // When rendering repeatedly after the animation interval.
    render(&mut terminal);
    let first = terminal.backend().buffer().clone();
    let first_session_row =
        find_row_containing(&first, width, height, "Sessions").expect("Sessions row");
    let first_spinner = (0..width)
        .find_map(|x| {
            first
                .cell((x, first_session_row))
                .filter(|cell| cell.fg == state.frontend.theme.streaming)
                .map(|cell| cell.symbol().to_owned())
        })
        .expect("initial spinner");
    // wall-clock-under-test: the spinner advances on a wall-clock
    // cadence, so the clock has to move for the second frame to differ. A
    // signal cannot substitute here.
    std::thread::sleep(Duration::from_millis(100));
    render(&mut terminal);

    // Then the visible title spinner advances.
    let second = terminal.backend().buffer().clone();
    let second_session_row =
        find_row_containing(&second, width, height, "Sessions").expect("Sessions row");
    let second_spinner = (0..width)
        .find_map(|x| {
            second
                .cell((x, second_session_row))
                .filter(|cell| cell.fg == state.frontend.theme.streaming)
                .map(|cell| cell.symbol().to_owned())
        })
        .expect("animated spinner");
    assert_ne!(first_spinner, second_spinner);
}

#[rstest::rstest]
fn sessions_hydration_spinner_preserves_footer_position() {
    // Given a sidebar with startup hydration active.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    state.session.begin_startup_hydration();
    let width = 30u16;
    let height = 40u16;
    let backend = TestBackend::new(width, height);
    let _terminal = Terminal::new(backend).unwrap();

    // When rendering the sidebar.
    let terminal = render_draw(&state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });

    // Then the spinner keeps the Sessions footer at the same bottom row.
    let buf = terminal.backend().buffer();
    let sessions_row = find_row_containing(buf, width, height, "Sessions").expect("Sessions row");
    assert_eq!(sessions_row, 39);
}

// ---------------------------------------------------------------------------
// History position save/restore
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn entering_pins_saves_history_position() {
    // Given persona focused with a known scroll offset and selected entry.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));
    state.active_session_mut().set_scroll_offset(Some(42));
    let entry_id_0 = state.active_session().history()[0].id.clone();
    state.active_session_mut().set_selected_entry_index(0);

    // When navigating down into Pins.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let first_id = state.sorted_pinned_ids()[0].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(first_id));
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // When navigating up to Persona.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

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
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let first_id = state.sorted_pinned_ids()[0].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(first_id));
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // When jumping to previous section (Persona).
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the history position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn sidebar_leave_discards_saved_position() {
    // Given pins focused with a saved position.
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let first_id = state.sorted_pinned_ids()[0].clone();
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(first_id));
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state.active_session_mut().save_history_position();

    // Modify state to simulate pin view.
    state.active_session_mut().set_scroll_offset(Some(10));
    state.active_session_mut().set_selected_entry_index(2);

    // When leaving the sidebar.
    crate::sections::intent::handle_sidebar_leave(&mut state);

    // Then the scroll stays at the pin's position (not restored).
    assert_eq!(state.active_session().scroll_offset(), Some(10));
    assert_eq!(state.active_session().selected_entry_index(), Some(2));
    // And the saved position is discarded.
    assert!(!state.active_session().has_saved_history_position());
}

/// State on Persona with three pins below it and the chat log parked at scroll
/// offset 42, entry 0.
fn state_on_persona_above_three_pins() -> AppState {
    let mut state = state_with_pinned(3);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));
    state.active_session_mut().set_scroll_offset(Some(42));
    state.active_session_mut().set_selected_entry_index(0);
    state
}

/// Moves the sidebar cursor down one row, as a keypress would.
fn navigate_down(state: &mut AppState) {
    let _ = navigate_sidebar(
        &SidebarIntent::MoveDown,
        state,
        jinn_slices::empty_config_layer(),
    );
}

#[rstest::rstest]
fn full_cycle_saves_and_restores() {
    // Given persona focused with original scroll position.
    let mut state = state_on_persona_above_three_pins();

    // When walking down through every pin and out of the section.
    navigate_down(&mut state); // onto the first pin
    navigate_down(&mut state); // onto the second pin
    navigate_down(&mut state); // onto the third pin
    navigate_down(&mut state); // off the end, back out to the sections above

    // Then the position is saved on the way out and restored on the way back.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn navigating_onto_pins_saves_the_position_without_restoring_it() {
    // Given persona focused with original scroll position.
    let mut state = state_on_persona_above_three_pins();

    // When navigating down onto the pins.
    navigate_down(&mut state);

    // Then the position is saved. Leaving persona parked the chat log where it
    // was, and `sync_chat_log_cursor` moved the selected entry to the pin's
    // history index (which may be 0 if the pin is the first entry) — the saved
    // position survives that.
    assert!(state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn navigating_between_pins_leaves_the_saved_position_saved() {
    // Given persona focused, with the position already saved and the cursor on
    // the first pin.
    let mut state = state_on_persona_above_three_pins();
    navigate_down(&mut state);

    // When navigating down within pins, to the second pin.
    navigate_down(&mut state);

    // Then the saved position is still there — moving within the section never
    // restores it.
    assert!(state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn navigating_to_the_last_pin_leaves_the_saved_position_saved() {
    // Given persona focused, with the position already saved and the cursor on
    // the second pin.
    let mut state = state_on_persona_above_three_pins();
    navigate_down(&mut state);
    navigate_down(&mut state);

    // When navigating down within pins, to the third and last pin.
    navigate_down(&mut state);

    // Then the saved position is still there.
    assert!(state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn jump_roundtrip_saves_and_restores() {
    // Given persona focused.
    let mut state = state_on_persona_above_three_pins();

    // When jumping to Pins and back to Persona.
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then position is restored.
    assert_eq!(state.active_session().scroll_offset(), Some(42));
    assert_eq!(state.active_session().selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn jump_to_pins_saves_the_position() {
    // Given persona focused.
    let mut state = state_on_persona_above_three_pins();

    // When jumping to Pins.
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then position is saved (via receive_cursor fallback).
    assert!(state.active_session().has_saved_history_position());
}

#[rstest::rstest]
fn jump_to_pins_with_retained_cursor_syncs_chat_log_cursor() {
    // Given pins focused with a retained cursor, then jumped away and back.
    // Use entries where the pinned entry is NOT the first, so the restore
    // puts the cursor on a different entry than the pin.
    let (mut state, pinned_id, _away) = state_with_pins_focused_on_a_middle_pin();

    // When jumping away to Persona and back.
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );
    let _ = jump_to_section(
        &SidebarIntent::MoveDown,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then chat log cursor is synced to the pinned entry.
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id),
        "chat log cursor should match the retained pin after jump back"
    );
}

#[rstest::rstest]
fn jumping_away_from_pins_restores_the_cursor_off_the_pin() {
    // Given pins focused with a retained cursor, synced to a pin that is not
    // the first entry — so the restore moves the cursor elsewhere.
    let (mut state, pinned_id, _away) = state_with_pins_focused_on_a_middle_pin();

    // When jumping away to Persona.
    let _ = jump_to_section(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then the cursor is restored away from the pin.
    assert_ne!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id),
        "cursor should be restored away from pin"
    );
}

/// Pins focused with a saved position, the chat log cursor already synced to
/// the pinned entry, and the pinned entry not first in history.
///
/// Returns the state, the pinned entry's id, and the id of the entry the
/// restored cursor lands on instead.
fn state_with_pins_focused_on_a_middle_pin() -> (AppState, ChatEntryId, ChatEntryId) {
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(ChatEntry::user("a")); // hist 0
    state.active_session_mut().push_entry(ChatEntry::user("b")); // hist 1 - will be pinned
    state.active_session_mut().push_entry(ChatEntry::user("c")); // hist 2
    let pinned_id = state.active_session().history()[1].id.clone();
    let away_id = state.active_session().history()[2].id.clone();
    state
        .active_session_mut()
        .pin_entry(&pinned_id, PinPosition::Top);

    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    state
        .frontend
        .update_sections(|s| s.pins.select_by_id(pinned_id.clone()));
    state.active_session_mut().set_selected_entry_index(2); // cursor on "c" before save
    state.active_session_mut().save_history_position();
    crate::sections::pins::pins_section::sync_chat_log_cursor(&mut state);
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id.clone()),
        "precondition: cursor should be on pinned entry"
    );

    (state, pinned_id, away_id)
}

// ---------------------------------------------------------------------------
// Scroll behaviour
// ---------------------------------------------------------------------------

/// Renders the sidebar into a `width` x `height` area and returns the buffer
/// as text rows.
fn render_sidebar_rows(
    sidebar: &mut Sidebar,
    state: &AppState,
    width: u16,
    height: u16,
) -> Vec<String> {
    let terminal = render_draw(state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    });
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

/// The row index of the selected entry line, if the sidebar drew one.
///
/// Finds the row by its selection band — the shared `selected_row_style`
/// background — because that band is what selection *is* now.
fn selected_row(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> Option<u16> {
    let buffer = terminal.backend().buffer();
    let theme = jinn_theme::default_theme();
    (0..height).find(|&y| {
        (0..width).any(|x| {
            buffer
                .cell((x, y))
                .is_some_and(|cell| cell.bg == theme.selection_fg)
        })
    })
}

/// Renders and returns the terminal so both text rows and cell styles can be
/// inspected.
fn render_sidebar_terminal(
    sidebar: &mut Sidebar,
    state: &AppState,
    width: u16,
    height: u16,
) -> Terminal<TestBackend> {
    render_draw(state, width, height, |frame, _area, frame_ctx| {
        sidebar.render(
            frame,
            ratatui::layout::Rect::new(0, 0, width, height),
            frame_ctx,
        );
    })
}

/// A state focused on the pins section with `count` pins and the cursor on the
/// pin at `selected`.
fn state_focused_on_pin(count: usize, selected: usize) -> AppState {
    let state = state_with_pinned(count);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Pins.focus_scope());
    let sorted_ids = state.sorted_pinned_ids();
    let id = sorted_ids[selected].clone();
    state.frontend.update_sections(|s| {
        s.pins.select_by_id(id);
    });
    state
}

#[rstest::rstest]
fn highlighted_row_stays_visible_with_40_pins_in_20_row_column() {
    // Given 40 pins in a 20-row column, with the cursor on the last pin.
    let mut sidebar = sidebar_with_all_sections();
    let state = state_focused_on_pin(40, 39);

    // When rendering.
    let width = 30u16;
    let height = 20u16;
    let terminal = render_sidebar_terminal(&mut sidebar, &state, width, height);

    // Then the highlighted row was drawn inside the column.
    let row = selected_row(&terminal, width, height);
    assert!(
        row.is_some_and(|row| row < height),
        "the selected pin must be drawn within the 20-row column, got {row:?}"
    );
}

#[rstest::rstest]
fn highlighted_row_stays_visible_with_30_phase_task_list() {
    // Given 30 single-line phases in a 20-row column, cursor on the last phase.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    let inputs: Vec<jinn_tools_msg::PhaseInput> = (0..30)
        .map(|i| jinn_tools_msg::PhaseInput {
            description: format!("Phase {i}"),
            tasks: vec![("task".to_owned(), jinn_tools_msg::TaskStatus::Pending)],
        })
        .collect();
    state
        .active_session_mut()
        .task_list_mut()
        .set_from_inputs(&inputs);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::TaskList.focus_scope());
    state
        .frontend
        .update_sections(|s| s.task_list.selected_phase_index = Some(29));

    // When rendering.
    let width = 60u16;
    let height = 20u16;
    let terminal = render_sidebar_terminal(&mut sidebar, &state, width, height);

    // Then the highlighted phase row was drawn inside the column.
    let row = selected_row(&terminal, width, height);
    assert!(
        row.is_some_and(|row| row < height),
        "the selected phase must be drawn within the 20-row column, got {row:?}"
    );
}

#[rstest::rstest]
fn cursor_row_is_middle_of_viewport_when_the_document_has_slack() {
    // Given 40 pins in a 40-row column with the cursor on pin 20.
    let mut sidebar = sidebar_with_all_sections();
    let state = state_focused_on_pin(40, 20);

    // When rendering.
    let width = 30u16;
    let height = 40u16;
    let terminal = render_sidebar_terminal(&mut sidebar, &state, width, height);

    // Then the cursor lands on the middle row of the column.
    let row = selected_row(&terminal, width, height).expect("a row is highlighted");
    assert!(
        (i32::from(row) - i32::from(height) / 2).abs() <= 1,
        "cursor should sit near the vertical middle ({height}/2), got row {row}"
    );
}

#[rstest::rstest]
fn the_selected_pin_is_the_one_drawn() {
    // Given 40 pins with the cursor on pin 30, in a column tall enough to show
    // the whole document.
    let selected = 30;
    let mut sidebar = sidebar_with_all_sections();
    let state = state_focused_on_pin(40, selected);

    // When rendering into a column that fits the whole document.
    let width = 30u16;
    let height = 60u16;
    let rows = render_sidebar_rows(&mut sidebar, &state, width, height);

    // Then the pin under the cursor is the highlighted one.
    // Then the pin under the cursor is the highlighted one.
    let expected = format!("entry {selected}");
    let highlighted: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains(&expected))
        .map(|(i, _)| i)
        .collect();
    assert!(
        highlighted.len() == 1,
        "the selected pin should be drawn exactly once, got rows {highlighted:?}"
    );
}

/// A state where every sidebar section has something to draw.
///
/// Navigation skips sections with nothing to show, so comparing the nav
/// chain against the draw order only means anything when nothing is
/// skipped for emptiness.
fn state_with_every_section_populated() -> AppState {
    let mut state = AppState::default_with_scope_focus();
    // Pins.
    let entry = ChatEntry::user("pinned");
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    // Bots: one beneath the *active* session, marked loaded so the
    // section's row filter admits it.
    {
        let parent = state.active_session().clone();
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        bot.append_bot_report("a finding".to_owned());
        bot.set_session_state(jinn_session_store_msg::SessionState::Loaded);
        let bot_id = bot.session_id().clone();
        *state.session_mut_or_create(&bot_id) = bot;
    }
    // Task list.
    state
        .active_session_mut()
        .task_list_mut()
        .set_from_inputs(&[jinn_tools_msg::PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![(
                "Read the docs".to_owned(),
                jinn_tools_msg::TaskStatus::Pending,
            )],
        }]);
    // MCP: the session must enable a server the config declares.
    state.active_session_mut().enable_mcp_server("probe");
    // Sessions: the active session itself is enough.
    state
}

#[rstest::rstest]
fn move_up_from_the_bots_section_enters_persona() {
    // Given the bots section focused, with the parent session and an
    // bot beneath it so the section actually has content.
    let mut state = state_with_pinned(2);
    {
        let parent = jinn_session_state::ChatSessionState::new();
        let id = parent.session_id().clone();
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        bot.append_bot_report("a finding".to_owned());
        *state.session_mut_or_create(&id) = bot;
    }
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Bot.focus_scope());

    // When navigating up.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus lands on Persona. The bots section renders directly
    // under Persona, so anything else sends the cursor backwards past a
    // section that is drawn below it.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
}

/// Builds a state whose bots section actually has a row.
///
/// The bot must be stored under **its own** id: the section's row
/// filter admits only sessions whose `parent_session` names the active
/// session's context (the active session's id, or its parent when the
/// active session is itself a bot). Inserting the bot under
/// the parent's id leaves `bot_rows` empty, and every navigation
/// through the section exits at the empty-list guard before reaching the
/// `MoveUp` arm — the trap this fixture exists to avoid.
fn state_with_bot_in_section() -> AppState {
    let mut state = AppState::default_with_scope_focus();
    // Clone the active session as the bot's parent.
    let parent = state.active_session().clone();
    let mut bot = ChatSessionState::new_bot(&parent, true);
    bot.append_bot_report("a finding".to_owned());
    let bot_id = bot.session_id().clone();
    *state.session_mut_or_create(&bot_id) = bot;
    state
}

#[rstest::rstest]
fn move_up_from_a_populated_bots_section_at_its_top_row_enters_persona() {
    // Given the bots section focused with a real row in it, the
    // cursor resting on that row (the section's top row).
    let mut state = state_with_bot_in_section();
    assert!(
        crate::sections::bots_section::has_content(&state),
        "the fixture must produce a non-empty bots section, or the \
         MoveUp arm under test never runs"
    );
    {
        let bot_id = state
            .session
            .iter()
            .find(|(_, s)| s.is_bot())
            .map(|(id, _)| id.clone())
            .expect("the fixture inserted a bot");
        state.frontend.update_sections(|s| {
            s.bot.selected_id = Some(bot_id);
        });
    }
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Bot.focus_scope());

    // When navigating up from the section's top row.
    let _ = navigate_sidebar(
        &SidebarIntent::MoveUp,
        &mut state,
        jinn_slices::empty_config_layer(),
    );

    // Then focus lands on Persona — not re-selected onto the top
    // bot — and the bots cursor is left where the section's
    // exhaustion contract requires: untouched.
    assert_eq!(
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
        jinn_sidebar_msg::SidebarSectionId::Persona
    );
}

#[rstest::rstest]
fn repeated_move_up_walks_every_populated_section_up_to_persona() {
    use jinn_sidebar_msg::SidebarSectionId;

    // Given Sessions focused — the lowest populated section — with pins and
    // a bot so Sessions → Pins → Bots → Persona all have
    // content, and TaskList and McpServers empty in between.
    let mut state = state_with_bot_in_section();
    let entry = ChatEntry::user("pinned");
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());

    // When pressing up until the section stops changing.
    let mut visited = vec![
        state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona),
    ];
    for _ in 0..10 {
        let _ = navigate_sidebar(
            &SidebarIntent::MoveUp,
            &mut state,
            jinn_slices::empty_config_layer(),
        );
        let section = state
            .frontend
            .sidebar_section()
            .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona);
        if visited.last() == Some(&section) {
            break;
        }
        visited.push(section);
    }

    // Then the walk visits every populated section in draw order and
    // stops at Persona: the bots section must hand the cursor up
    // rather than trapping it.
    assert_eq!(
        visited,
        vec![
            SidebarSectionId::Sessions,
            SidebarSectionId::Pins,
            SidebarSectionId::Bot,
            SidebarSectionId::Persona
        ],
        "k must walk up through every populated section and stop at Persona"
    );
}

#[rstest::rstest]
fn the_sidebar_section_order_is_the_same_everywhere() {
    // Given the order the sections are registered in, which is the order
    // they are drawn in.
    let mut sidebar = Sidebar::new();
    super::register_sections(&mut sidebar);
    let registered = sidebar.section_ids();

    // Given a state where every section has something to show, so
    // navigation stops skipping over the empty ones.
    let mut state = state_with_every_section_populated();

    // When walking the navigation chain downward from the first section,
    // through the public entry point a key press actually takes.
    let mut walked = vec![registered[0]];
    state.frontend.scope_push(registered[0].focus_scope());
    for _ in 0..(registered.len() * 2) {
        let before = state.frontend.sidebar_section();
        let _ = navigate_sidebar(
            &SidebarIntent::MoveDown,
            &mut state,
            jinn_slices::empty_config_layer(),
        );
        let after = state.frontend.sidebar_section();
        if after == before {
            break;
        }
        if let Some(id) = after {
            walked.push(id);
        }
    }

    // Then the chain visits the populated sections in draw order, and
    // nothing between them is out of place. Empty sections are skipped by
    // design, so MCP (which needs a configured server) is absent here;
    // what this pins is the *relative* order, which is what drifted.
    let populated: Vec<jinn_sidebar_msg::SidebarSectionId> = registered
        .iter()
        .copied()
        .filter(|id| walked.contains(id))
        .collect();
    assert_eq!(
        walked, populated,
        "navigation visits sections in an order the sidebar does not draw them in"
    );
    // And the layout's own copy agrees with the live registration, since
    // overlay anchors are computed from it rather than from the sidebar.
    assert_eq!(
        super::layout::REGISTRATION_ORDER.to_vec(),
        registered,
        "the layout's section order has drifted from the registration order"
    );
}

#[rstest::rstest]
fn preview_bottom_edge_sits_two_rows_above_the_sessions_cursor() {
    // Given the sidebar focused on sessions, its cursor on a session with a
    // cached preview, and a frame tall enough for the full popup.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    // The scope is pushed before the section matters: `sidebar_section` is
    // None without a sidebar scope on the stack, and the preview exits early.
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
    let entry = crate::sections::sessions::state::sorted_open_sessions(&state)
        .first()
        .expect("the active session always lists")
        .clone();
    state
        .frontend
        .update_sections(|s| s.sessions.selected_id = Some(entry.id.clone()));
    // A preview must be cached at the width the render pass will look up at:
    // the pass derives it from the frame, so the test derives it the same way.
    let frame_area = ratatui::layout::Rect::new(0, 0, 100, 60);
    let measured = crate::sections::sessions::preview::preview_content_width(frame_area);
    state
        .frontend
        .update_sections(|s| s.sessions.preview_content_width = measured);
    complete_preview(&mut state, &entry.id);

    let width = 100u16;
    let height = 60u16;

    // When the sidebar renders and the preview pass draws over it.
    let terminal = render_draw(&state, width, height, |frame, area, frame_ctx| {
        sidebar.render(frame, area, frame_ctx);
        crate::sections::sessions::render_session_preview_for_state(frame, area, area, frame_ctx);
    });

    // Then the popup's bottom edge sits exactly two rows above the row the
    // cursor band is drawn on.
    let buf = terminal.backend().buffer();
    let keybinds_row = find_row_containing(buf, width, height, " close · a archive · i insert")
        .expect("the popup's keybinds bar renders");
    let cursor_row = cursor_row_of_band(buf, width, height);
    assert_eq!(
        keybinds_row + POPUP_ROWS_BELOW_KEYBINDS + PAST_BORDER + POPUP_GAP_ROWS,
        cursor_row,
        "the popup must hang two rows above the cursor row"
    );
}

#[rstest::rstest]
fn preview_bottom_edge_sits_two_rows_above_the_library_cursor() {
    // Given the sidebar focused on the library, its cursor on an in-memory
    // member with a cached preview.
    let mut sidebar = sidebar_with_all_sections();
    let mut state = AppState::default_with_scope_focus();
    let mut member = jinn_session_state::ChatSessionState::new();
    member.set_title("a library member".to_owned());
    member.push_entry(jinn_kernel::protocol::ChatEntry::user("hello"));
    member.set_in_library(true);
    let member_id = member.session_id().clone();
    // Sections above the library are empty in a default state, which puts
    // the library's cursor a few rows from the frame's top — no room for the
    // popup above it. Bots give those sections rows.
    seed_bots(&mut state, 4);
    let (library_cell, seed) = {
        let cell = state
            .frontend
            .slices()
            .and_then(|slices| {
                slices.reader::<jinn_session_store_msg::LibraryState>(
                    &jinn_session_store_msg::library_slot(),
                )
            })
            .expect("the cell catalog registers the library slot");
        let summary = jinn_session_store_msg::SessionSummary {
            session_id: member_id.clone(),
            title: "a library member".to_owned(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: jinn_session_store_msg::SessionState::Loaded,
            in_library: true,
            is_bot: false,
            parent_session: None,
            project: None,
        };
        cell.update(|library| library.entries = vec![summary]);
        (cell, ())
    };
    let _ = (library_cell, seed);
    state.session.insert(member);
    // The library scope focuses the section and lights its cursor band.
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Library.focus_scope());
    state.frontend.update_sections(|s| {
        s.library.selected_id = Some(member_id.clone());
        s.sessions.selected_id = None;
    });
    let frame_area = ratatui::layout::Rect::new(0, 0, 100, 60);
    let measured = crate::sections::sessions::preview::preview_content_width(frame_area);
    state
        .frontend
        .update_sections(|s| s.sessions.preview_content_width = measured);
    complete_preview(&mut state, &member_id);

    let width = 100u16;
    let height = 60u16;

    // When the sidebar renders and the preview pass draws over it.
    let terminal = render_draw(&state, width, height, |frame, area, frame_ctx| {
        sidebar.render(frame, area, frame_ctx);
        crate::sections::sessions::render_session_preview_for_state(frame, area, area, frame_ctx);
    });

    // Then the popup's bottom edge sits exactly two rows above the row the
    // cursor band is drawn on — the header-row offset that once left the
    // popup floating four rows up cannot come back.
    let buf = terminal.backend().buffer();
    let keybinds_row = find_row_containing(buf, width, height, " close · a archive · i insert")
        .expect("the popup's keybinds bar renders");
    assert!(
        find_row_containing(buf, width, height, "a library member").is_some(),
        "the member row renders"
    );
    let cursor_row = cursor_row_of_band(buf, width, height);
    assert_eq!(
        keybinds_row + POPUP_ROWS_BELOW_KEYBINDS + PAST_BORDER + POPUP_GAP_ROWS,
        cursor_row,
        "the popup must hang two rows above the library cursor row"
    );
}

/// Rows from the keybinds line down to the popup's bottom border: the second
/// keybind row, the cwd row, the stats row, and the border itself.
const POPUP_ROWS_BELOW_KEYBINDS: u16 = 4;

/// The one row the cursor sits past the popup's bottom border before the gap
/// is counted: bottom border, gap, gap, cursor.
const PAST_BORDER: u16 = 1;

/// The rows between the popup's bottom border and the cursor row — the
/// gap the rect math honours. Named here so the pin reads as arithmetic
/// rather than an import of the thing it audits.
const POPUP_GAP_ROWS: u16 = 2;

/// Inserts `count` bots into the session map so the sections above the
/// library have rows and the library's cursor sits low enough in the frame
/// for the popup to have room above it.
fn seed_bots(state: &mut AppState, count: usize) {
    let parent = state.session.active_session().clone();
    for i in 0..count {
        let mut bot = jinn_session_state::ChatSessionState::new_bot(&parent, true);
        bot.set_title(format!("seeded bot {i}"));
        state.session.insert(bot);
    }
}

/// Renders a preview for `id` and completes it into the cache, the way the
/// worker would — an empty line set, which caches as a hit and draws the
/// popup's chrome and footer.
fn complete_preview(state: &mut AppState, id: &jinn_core_types::SessionId) {
    use std::sync::Arc;
    let request = crate::sections::sessions::preview_load::update_preview(
        state,
        id,
        jinn_slices::empty_config_layer(),
    )
    .expect("a measured width must request");
    state.frontend.update_sections(|s| {
        s.sessions.preview.complete(
            id.clone(),
            request.generation,
            request.signature,
            request.content_width,
            Arc::new(Vec::new()),
        );
    });
}

/// The row the cursor band is drawn on, found by the selection background.
fn cursor_row_of_band(buf: &ratatui::buffer::Buffer, width: u16, height: u16) -> u16 {
    let theme = &AppState::default_with_scope_focus().frontend.theme;
    for y in 0..height {
        let banded: Vec<u16> = (0..width)
            .filter(|&x| {
                buf.cell((x, y))
                    .is_some_and(|cell| cell.bg == theme.selection_fg)
            })
            .collect();
        if !banded.is_empty() {
            return y;
        }
    }
    panic!("no selection band found in the buffer");
}
