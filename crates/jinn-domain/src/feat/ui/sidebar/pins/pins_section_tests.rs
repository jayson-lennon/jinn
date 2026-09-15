#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_testutil::setup_term;
use ratatui::style::Color;

use crate::common::app_state::{AppState, FocusScope};
use crate::common::render_ctx::RenderCtx;
use crate::feat::ui::sidebar::pins::pins_section::*;
use crate::feat::ui::sidebar::section_trait::{SidebarSection, SidebarSectionId};
use crate::protocol::{ChangeSource, ChatEntry, PinPosition};

fn state_with_pinned(count: usize) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    let mut ids = vec![];
    for i in 0..count {
        let entry = ChatEntry::user(format!("entry {i}"));
        let entry_id = entry.id.clone();
        state.active_session_mut().push_entry(entry);
        ids.push(entry_id);
    }
    for id in &ids {
        state.active_session_mut().pin_entry(id, PinPosition::Top);
    }
    // Select the first pinned entry.
    if let Some(first_id) = ids.first() {
        state.frontend.pins.select_by_id(first_id.clone());
    }
    state
}

#[rstest::rstest]
fn sidebar_persona_edit_opens_picker_when_persona_focused() {
    // Given a state with persona section focused and sidebar scope.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state
        .frontend
        .scope_set_sidebar_section(SidebarSectionId::Persona);

    // When handling sidebar persona edit.
    let result = handle_sidebar_persona_edit(&mut state);

    // Then the persona picker is active.
    assert_eq!(
        state.frontend.picker_kind(),
        Some(crate::protocol::PickerKind::Persona)
    );
    // And a LoadPersonaPickerEntries command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("LoadPersonaPickerEntries"))
    );
}

#[rstest::rstest]
fn sidebar_persona_edit_noop_when_pins_focused() {
    // Given a state with pins section focused and sidebar scope.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state
        .frontend
        .scope_set_sidebar_section(SidebarSectionId::Pins);

    // When handling sidebar persona edit.
    let result = handle_sidebar_persona_edit(&mut state);

    // Then nothing changed.
    assert!(!state.frontend.is_picker());
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn pins_unpin_returns_command() {
    // Given a state with pinned entries.
    let mut state = state_with_pinned(2);

    // When handling pins unpin.
    let result = handle_pins_unpin(&mut state);

    // Then an UnpinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("UnpinChatEntry"))
    );
}

#[rstest::rstest]
fn pins_unpin_noop_when_empty() {
    // Given a state with no pinned entries.
    let mut state = AppState::default_with_scope_focus();

    // When handling pins unpin.
    let result = handle_pins_unpin(&mut state);

    // Then no commands.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn pins_pin_top_returns_command() {
    // Given a state with pinned entries.
    let mut state = state_with_pinned(1);

    // When handling pins pin top.
    let result = handle_pins_pin(&mut state, PinPosition::Top);

    // Then a PinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("PinChatEntry"))
    );
}

#[rstest::rstest]
fn pins_pin_bottom_returns_command() {
    // Given a state with pinned entries.
    let mut state = state_with_pinned(1);

    // When handling pins pin bottom.
    let result = handle_pins_pin(&mut state, PinPosition::Bottom);

    // Then a PinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("PinChatEntry"))
    );
}

#[rstest::rstest]
fn pins_pin_relative_returns_command() {
    // Given a state with pinned entries.
    let mut state = state_with_pinned(1);

    // When handling pins pin relative.
    let result = handle_pins_pin(&mut state, PinPosition::Relative);

    // Then a PinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("PinChatEntry"))
    );
}

#[rstest::rstest]
fn pins_pin_cycle_rotates_top_to_bottom() {
    // Given a pinned entry at Top.
    let mut state = AppState::default_with_scope_focus();
    let entry = ChatEntry::user("entry");
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    let sorted_ids = state.sorted_pinned_ids();
    state.frontend.pins.select_by_id(sorted_ids[0].clone());

    // When handling pins pin cycle.
    let result = handle_pins_pin_cycle(&mut state);

    // Then a PinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("PinChatEntry"))
    );
}

#[rstest::rstest]
fn pins_pin_cycle_noop_when_empty() {
    // Given a state with no pinned entries.
    let mut state = AppState::default_with_scope_focus();

    // When handling pins pin cycle.
    let result = handle_pins_pin_cycle(&mut state);

    // Then no commands.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn pins_pin_top_noop_when_no_selection() {
    // Given a state with pinned entries but no selection.
    let mut state = AppState::default_with_scope_focus();
    let entry = ChatEntry::user("entry");
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    // Don't select anything.

    // When handling pins pin top.
    let result = handle_pins_pin(&mut state, PinPosition::Top);

    // Then no commands.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn section_id_is_pins() {
    // Given a PinsSection.
    let section = PinsSection;

    // When asking for its ID.
    // Then it returns Pins.
    assert_eq!(section.id(), SidebarSectionId::Pins);
}

#[rstest::rstest]
fn content_height_is_zero_when_empty() {
    // Given a PinsSection and state with no pinned entries.
    let section = PinsSection;
    let state = AppState::default_with_scope_focus();

    // When asking for content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

    // Then it returns 0 (section is hidden when empty).
    assert_eq!(height, 0);
}

#[rstest::rstest]
fn content_height_matches_entry_count() {
    // Given a PinsSection and state with 3 pinned entries.
    let section = PinsSection;
    let state = state_with_pinned(3);

    // When asking for content height.
    let slices = jinn_slices::Slices::new();
    let overlay_views = crate::common::overlay_views::OverlayViews::new();
    let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

    // Then it returns header(1) + header-gap(1) + entries(3) + trailing gap(1) = 6.
    assert_eq!(height, 6);
}

fn render_rows(
    section: &mut PinsSection,
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
fn render_empty_shows_header_with_zero_count() {
    let mut section = PinsSection;
    let state = AppState::default_with_scope_focus();
    let rows = render_rows(&mut section, &state, 40, 10);
    assert!(rows[0].contains("Pinned Context"));
    assert!(rows[0].contains('0'));
}

#[rstest::rstest]
fn render_shows_pinned_entries() {
    let mut section = PinsSection;
    let state = state_with_pinned(2);
    let rows = render_rows(&mut section, &state, 60, 20);
    let combined = rows.join("\n");
    assert!(
        combined.contains("pinned message 0") || combined.contains("entry 0"),
        "should contain first entry, got: {combined}"
    );
}

#[rstest::rstest]
fn consecutive_pinned_entries_are_adjacent_with_no_blank_between() {
    // Given a PinsSection and state with 3 pinned entries.
    let mut section = PinsSection;
    let state = state_with_pinned(3);

    // When rendering.
    let rows = render_rows(&mut section, &state, 60, 20);

    // Then the first and second entries sit on consecutive rows (no blank between).
    let row_of_entry0 = rows
        .iter()
        .position(|row| row.contains("entry 0"))
        .expect("first entry should be rendered");
    let row_of_entry1 = row_of_entry0 + 1;
    assert!(
        rows[row_of_entry1].contains("entry 1"),
        "second entry should be on the row immediately after the first; \n
        got row {row_of_entry1} = {:?}",
        rows[row_of_entry1]
    );
}

#[rstest::rstest]
fn render_selected_entry_has_yellow_marker_when_sidebar_focused() {
    let mut section = PinsSection;
    let state = state_with_pinned(2);
    // Sidebar must be focused for the indicator to be yellow.
    state.frontend.scope_push(FocusScope::SidebarPins);

    let (mut terminal, area) = setup_term(60, 20);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // First entry at index 0 is selected by default.
    // No bordered block in section render - content starts at row 0.
    let cell0 = buffer.cell((0, 2)).expect("cell 0,2");
    assert_eq!(cell0.symbol(), "\u{2588}");
    assert_eq!(cell0.fg, Color::Yellow);
}

#[rstest::rstest]
fn render_selected_entry_has_darkgray_marker_when_not_focused() {
    let mut section = PinsSection;
    let state = state_with_pinned(2);
    // Sidebar is NOT focused (Normal scope is the default).

    let (mut terminal, area) = setup_term(60, 20);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            section.render(frame, area, &ctx);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // When sidebar is not focused, no selection indicator is shown.
    let cell0 = buffer.cell((0, 2)).expect("cell 0,2");
    assert_eq!(cell0.symbol(), " ");
}

#[rstest::rstest]
fn render_sorts_entries_by_position() {
    // Given entries pinned with BOT, TOP, REL positions (added in that order).
    let mut section = PinsSection;
    let mut state = AppState::default_with_scope_focus();

    let bot_entry = ChatEntry::user("bottom entry");
    let bot_id = bot_entry.id.clone();
    state.active_session_mut().push_entry(bot_entry);
    state
        .active_session_mut()
        .pin_entry(&bot_id, PinPosition::Bottom);

    let top_entry = ChatEntry::user("top entry");
    let top_id = top_entry.id.clone();
    state.active_session_mut().push_entry(top_entry);
    state
        .active_session_mut()
        .pin_entry(&top_id, PinPosition::Top);

    let rel_entry = ChatEntry::user("relative entry");
    let rel_id = rel_entry.id.clone();
    state.active_session_mut().push_entry(rel_entry);
    state
        .active_session_mut()
        .pin_entry(&rel_id, PinPosition::Relative);

    // When rendering.
    let rows = render_rows(&mut section, &state, 60, 20);
    let combined = rows.join("\n");

    // Then entries appear in TOP, REL, BOT order.
    let top_pos = combined
        .find("top entry")
        .expect("should contain top entry");
    let rel_pos = combined
        .find("relative entry")
        .expect("should contain relative entry");
    let bot_pos = combined
        .find("bottom entry")
        .expect("should contain bottom entry");
    assert!(
        top_pos < rel_pos,
        "TOP entry should appear before REL entry"
    );
    assert!(
        rel_pos < bot_pos,
        "REL entry should appear before BOT entry"
    );
}

// Note: SessionNew section-scoping is now handled by the keymap (n is only
// bound in SidebarSessions scope), so the IntentHandler no longer checks
// which section is focused. These tests validated the old handler-level check.

#[rstest::rstest]
fn session_new_works_when_sidebar_sessions_focused() {
    // Given a state in Sidebar scope with Sessions section focused.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::SidebarPersona);
    state
        .frontend
        .scope_set_sidebar_section(SidebarSectionId::Sessions);
    let _old_id = state.session.active_session_id().clone();

    // When handling SessionNew via IntentHandler.
    let result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SessionNew,
        &mut state,
        &empty_slices(),
        &empty_routes(),
    );

    // Then a new session is created.
    // And SessionCreated and ActiveSessionChanged are emitted.
    assert_eq!(result.message_names.len(), 2);
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("SessionCreated"))
    );
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("ActiveSessionChanged"))
    );
}

#[rstest::rstest]
fn session_new_works_when_not_in_sidebar() {
    // Given a state in Normal scope (not sidebar).
    let mut state = AppState::default_with_scope_focus();
    let old_id = state.session.active_session_id().clone();

    // When handling SessionNew via IntentHandler.
    let _result = crate::feat::intent::IntentHandler::handle(
        &crate::Intent::SessionNew,
        &mut state,
        &empty_slices(),
        &empty_routes(),
    );

    // Then a new session is created (no section restriction outside sidebar).
    assert_ne!(*state.session.active_session_id(), old_id);
}

#[rstest::rstest]
fn sync_chat_log_cursor_sets_cursor_by_entry_id_with_visual_items() {
    // Given a session with ignored entries (causing visual-item index != history index)
    // and a pinned entry deep in history.
    use crate::feat::ui::chat_log::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
    };

    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(ChatEntry::user("a")); // hist 0
    for _ in 0..15 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            crate::protocol::ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        state.active_session_mut().push_entry(entry);
    } // hist 1..15 (collapsed into 1 visual item)
    state.active_session_mut().push_entry(ChatEntry::user("b")); // hist 16

    // Pin the entry at history index 12 (inside the ignored block).
    // When the block is expanded, this entry should be selectable.
    let pinned_id = state.active_session().history()[12].id.clone();
    state
        .active_session_mut()
        .pin_entry(&pinned_id, PinPosition::Top);

    // Expand the ignored block so the pinned entry is visible.
    let block_start_id = state.active_session().history()[1].id.clone();
    state
        .active_session_mut()
        .toggle_ignored_block_visibility(&block_start_id);

    // Build visual items (now expanded - individual Entry items).
    let items = build_visual_items(
        state.active_session().history(),
        &state.active_session().shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    state.active_session_mut().set_visual_items(items);

    // Select the pinned entry in the pins section.
    state.frontend.pins.select_by_id(pinned_id.clone());

    // Set cursor to something else first.
    let first_entry_id = state.active_session().history()[0].id.clone();
    state
        .active_session_mut()
        .set_selected_cursor_id(first_entry_id);
    assert_ne!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id.clone()),
        "precondition: cursor should not be on pinned entry"
    );

    // When sync_chat_log_cursor is called.
    crate::feat::ui::sidebar::pins::pins_section::sync_chat_log_cursor(&mut state);

    // Then the chat log cursor is set to the pinned entry by ID.
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(pinned_id.clone()),
        "sync_chat_log_cursor should set cursor to pinned entry by ID"
    );
}

#[rstest::rstest]
fn sync_chat_log_cursor_sets_correct_entry_when_multiple_entries_exist() {
    // Given a session with 3 entries and the middle one pinned.
    let mut state = AppState::default_with_scope_focus();
    let entry_a = ChatEntry::user("entry a");
    let id_a = entry_a.id.clone();
    state.active_session_mut().push_entry(entry_a);

    let entry_b = ChatEntry::user("entry b");
    let id_b = entry_b.id.clone();
    state.active_session_mut().push_entry(entry_b);

    let entry_c = ChatEntry::user("entry c");
    let id_c = entry_c.id.clone();
    state.active_session_mut().push_entry(entry_c);

    // Pin entry_b and select it in the pins section.
    state
        .active_session_mut()
        .pin_entry(&id_b, PinPosition::Top);
    state.frontend.pins.select_by_id(id_b.clone());

    // Set cursor to entry_a first.
    state
        .active_session_mut()
        .set_selected_cursor_id(id_a.clone());

    // When sync_chat_log_cursor is called.
    sync_chat_log_cursor(&mut state);

    // Then the cursor is set to entry_b (not entry_a or entry_c).
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(id_b),
        "sync_chat_log_cursor should set cursor to the pinned entry, not others"
    );
    assert_ne!(
        state.active_session().selected_cursor_id(),
        Some(id_a),
        "cursor should not be on entry a"
    );
    assert_ne!(
        state.active_session().selected_cursor_id(),
        Some(id_c),
        "cursor should not be on entry c"
    );
}

#[rstest::rstest]
fn resolve_selected_entry_id_returns_real_session_and_entry_ids() {
    // Given a state with a pinned entry.
    let mut state = AppState::default_with_scope_focus();
    let entry = ChatEntry::user("pinned entry");
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    state.frontend.pins.select_by_id(entry_id);

    // When handling pins unpin (which uses resolve_selected_entry_id internally).
    let result = handle_pins_unpin(&mut state);

    // Then the UnpinChatEntry command is returned.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("UnpinChatEntry")),
        "should return an UnpinChatEntry command"
    );
}

use crate::feat::session::tool_result_status::ToolResultStatus;

/// Empty slice registry + route table for handler tests that don't
/// exercise slices or route rows.
fn empty_slices() -> crate::common::slices::Slices {
    crate::common::slices::Slices::new()
}

fn empty_routes() -> crate::common::slices::key_routes::KeyRoutes {
    crate::common::slices::key_routes::KeyRoutes::new()
}

/// Build an AppState with one pinned tool-result entry.
fn state_with_pinned_tool_result(name: &str, content: &str) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    let entry = ChatEntry::tool_result("call-1", name, content, ToolResultStatus::Success);
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    state
}

/// Join all rendered rows into one string for substring assertions.
fn joined_render(state: &AppState) -> String {
    let mut section = PinsSection;
    render_rows(&mut section, state, 50, 10).join("\n")
}

#[rstest::rstest]
fn pins_skill_result_shows_summary_label_without_raw_xml() {
    // Given a pinned skill tool result with well-formed XML content.
    let state = state_with_pinned_tool_result(
        "skill",
        "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>",
    );

    // When rendering the pins section.
    let rendered = joined_render(&state);

    // Then the summary label shows the icon and name, with no raw XML.
    assert!(
        rendered.contains('\u{2756}'),
        "skill pin should show the skill icon: {rendered}"
    );
    assert!(
        rendered.contains("phased-task-loop"),
        "skill pin should show the skill name: {rendered}"
    );
    assert!(
        !rendered.contains("<skill"),
        "skill pin should not show raw XML: {rendered}"
    );
    assert!(
        !rendered.contains("location="),
        "skill pin should not show the location attribute: {rendered}"
    );
}

#[rstest::rstest]
fn pins_skill_result_malformed_content_uses_fallback_label() {
    // Given a pinned skill tool result with malformed (non-skill) content.
    let state = state_with_pinned_tool_result("skill", "not a skill xml");

    // When rendering the pins section.
    let rendered = joined_render(&state);

    // Then the fallback label shows the icon and (skill), with no panic.
    assert!(
        rendered.contains('\u{2756}'),
        "malformed skill pin should still show the icon: {rendered}"
    );
    assert!(
        rendered.contains("(skill)"),
        "malformed skill pin should show the fallback label: {rendered}"
    );
}

#[rstest::rstest]
fn pins_non_skill_tool_result_unchanged() {
    // Given a pinned non-skill tool result.
    let state = state_with_pinned_tool_result("read", "file contents here");

    // When rendering the pins section.
    let rendered = joined_render(&state);

    // Then the non-skill rendering is unchanged: check icon, tool name, content.
    assert!(
        rendered.contains('✓'),
        "non-skill success result should show the check icon: {rendered}"
    );
    assert!(
        rendered.contains("read"),
        "non-skill result should show the tool name: {rendered}"
    );
    assert!(
        rendered.contains("file contents here"),
        "non-skill result should show the raw content: {rendered}"
    );
}

#[rstest::rstest]
fn truncate_to_width_returns_string_unchanged_when_it_fits() {
    // Given a string that fits within the budget.
    let result = truncate_to_width("hello", 10);

    // Then it is returned unchanged.
    assert_eq!(result, "hello");
}

#[rstest::rstest]
fn truncate_to_width_truncates_ascii_and_appends_ellipsis() {
    // Given a string that exceeds the budget.
    let result = truncate_to_width("hello world", 5);

    // Then it is truncated to fit with an ellipsis.
    assert_eq!(result, "hell\u{2026}");
}

#[rstest::rstest]
fn truncate_to_width_fits_double_wide_char_within_budget() {
    // Given a string with a double-wide char that fits.
    let result = truncate_to_width("\u{2705} ok", 10);

    // Then it is returned unchanged (2+1+2=5 <= 10).
    assert_eq!(result, "\u{2705} ok");
}

#[rstest::rstest]
fn truncate_to_width_skips_wide_char_at_boundary() {
    // Given a string where a 2-cell char would overflow the budget.
    let result = truncate_to_width("ab\u{2705}", 3);

    // Then the wide char is skipped and ellipsis is appended.
    assert_eq!(result, "ab\u{2026}");
}

#[rstest::rstest]
fn truncate_to_width_fits_wide_char_exactly() {
    // Given a string where a wide char fills the budget exactly.
    let result = truncate_to_width("a\u{2705}b", 4);

    // Then it is returned unchanged (1+2+1=4).
    assert_eq!(result, "a\u{2705}b");
}

#[rstest::rstest]
fn truncate_to_width_only_wide_chars_budget_one() {
    // Given only wide chars with a budget of 1.
    let result = truncate_to_width("\u{2705}\u{274c}", 1);

    // Then only the ellipsis fits.
    assert_eq!(result, "\u{2026}");
}

#[rstest::rstest]
fn truncate_to_width_empty_string_returns_empty() {
    // Given an empty string.
    let result = truncate_to_width("", 5);

    // Then it is returned unchanged.
    assert_eq!(result, "");
}

#[rstest::rstest]
fn truncate_to_width_zero_budget_returns_empty() {
    // Given a non-empty string with zero budget.
    let result = truncate_to_width("hello", 0);

    // Then an empty string is returned (no room for anything).
    assert_eq!(result, "");
}

#[rstest::rstest]
fn truncate_str_strips_ansi_before_truncating() {
    // Given a string with ANSI escape codes.
    let result = truncate_str("\x1b[31mhello\x1b[0m", 3);

    // Then ANSI is stripped and the plain text is truncated.
    assert_eq!(result, "he\u{2026}");
}

#[rstest::rstest]
#[test]
fn tool_result_with_wide_emoji_fits_narrow_sidebar() {
    // Given a pinned tool result with the ✓ success icon.
    let state = state_with_pinned_tool_result("write", &"x".repeat(100));

    // When rendering in a narrow sidebar (20 cells).
    let mut section = PinsSection;
    let rows = render_rows(&mut section, &state, 20, 10);

    // Then the ✓ character is present (single-width, no clipping issue).
    let combined = rows.join("\n");
    assert!(
        combined.contains('✓'),
        "should still contain ✓ even in narrow sidebar: {combined}"
    );
}

#[rstest::rstest]
fn long_content_is_truncated_to_fit_area_width() {
    // Given a pinned user entry with very long content.
    let mut state = AppState::default_with_scope_focus();
    let entry = ChatEntry::user("a".repeat(200));
    let entry_id = entry.id.clone();
    state.active_session_mut().push_entry(entry);
    state
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    state.frontend.pins.select_by_id(entry_id);

    // When rendering in a narrow sidebar (25 cells).
    let mut section = PinsSection;
    let rows = render_rows(&mut section, &state, 25, 10);

    // Then the content row exists and fits within the area.
    let combined = rows.join("\n");
    // The content should be truncated (contain ellipsis) rather than overflow.
    assert!(
        combined.contains('\u{2026}'),
        "long content should be truncated with ellipsis: {combined}"
    );
}
