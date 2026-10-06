//! The chat input box — the kernel's unit tests, now exercising the
//! slice through its public surface.
//!
//! These were the the chat input slice's and
//! file_lister unit tests. They live here because the
//! implementation is a slice now: the box's handlers, element, validator,
//! autocomplete render, and directory-lister actor are reached through
//! `jinn_chat_input`'s public API rather than through kernel-internal
//! modules.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::uninlined_format_args,
    reason = "test code"
)]

use jinn_chat_input::element::ChatInputBoxElement;
use jinn_kernel::AppState;
use jinn_kernel::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_kernel::common::ui_registry::UiRegistry;
use jinn_kernel::protocol::ChatEntry;
use jinn_slices::FocusScope;
use jinn_theme::default_theme;
use jinn_turn_dispatch_msg::QueueItem;

use jinn_kernel::IntentHandler;
use jinn_kernel::protocol::KernelIntent;
use jinn_kernel::state::frontend_state::PendingSessionCreation;
use jinn_testutil::setup_term;
use ratatui::layout::Position;

#[rstest::rstest]
fn name_returns_chat_input_box() {
    // Given a ChatInputBoxElement.
    let element = ChatInputBoxElement;

    // When querying the name.
    let name = element.name();

    // Then it is "chat-input-box".
    assert_eq!(name, "chat-input-box");
}

#[rstest::rstest]
fn render_draws_input_buffer() {
    // Given a ChatInputBoxElement with "hello" in state (Normal mode).
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("hello"));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the buffer contains the ">" prompt character.
    let buffer = terminal.backend().buffer().clone();
    let cell = buffer.cell((0, 0)).expect("cell should exist");
    assert_eq!(cell.symbol(), ">");
}

#[rstest::rstest]
fn render_input_mode_yellow_prompt() {
    // Given a ChatInputBoxElement in Input mode with "hi" in buffer.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("hi"));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the ">" prompt is yellow.
    let buffer = terminal.backend().buffer().clone();
    let cell = buffer.cell((0, 0)).expect("cell should exist");
    assert_eq!(cell.symbol(), ">");
    assert_eq!(cell.style().fg, Some(default_theme().focus_accent));
}

#[rstest::rstest]
fn render_input_mode_yellow_border() {
    // Given a ChatInputBoxElement in Input mode.
    let mut element = ChatInputBoxElement;
    let state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the bottom border is yellow (sample a cell away from the badge at x=1).
    let buffer = terminal.backend().buffer().clone();
    let cell = buffer.cell((20, 2)).expect("cell should exist");
    assert_eq!(cell.style().fg, Some(default_theme().focus_accent));
}

#[rstest::rstest]
fn render_input_mode_cursor_at_end_of_text() {
    // Given a ChatInputBoxElement in Input mode with "abc" in buffer.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("abc"));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (5, 0): inner.x=0 + "> "=2 + "abc"=3.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 5, y: 0 });
}

#[rstest::rstest]
fn render_cursor_at_mid_buffer() {
    // Given a ChatInputBoxElement in Input mode with "abc" and cursor at position 1.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("abc"));
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_right); // cursor at 1 (between 'a' and 'b')
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (3, 0): inner.x=0 + "> "=2 + cursor_pos=1.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 3, y: 0 });
}

#[rstest::rstest]
fn render_cursor_at_home() {
    // Given a ChatInputBoxElement in Input mode with "hi" and cursor moved to start.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("hi"));
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (2, 0): inner.x=0 + "> "=2 + cursor_pos=0.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 2, y: 0 });
}

#[rstest::rstest]
fn multiline_first_line_has_prefix() {
    // Given a ChatInputBoxElement with "hello\nworld" in buffer (Normal mode).
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("hello\nworld"));
        s
    };

    let (mut terminal, area) = setup_term(40, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the first row is prefixed with "> " and reads "hello".
    let buffer = terminal.backend().buffer().clone();
    let cell = buffer.cell((0, 0)).expect("cell should exist");
    assert_eq!(cell.symbol(), ">");
    let h_cell = buffer.cell((2, 0)).expect("cell should exist");
    assert_eq!(h_cell.symbol(), "h");
}

#[rstest::rstest]
fn multiline_second_line_has_indent() {
    // Given a ChatInputBoxElement with "hello\nworld" in buffer (Normal mode).
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("hello\nworld"));
        s
    };

    let (mut terminal, area) = setup_term(40, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the second row is indented and reads "world".
    let buffer = terminal.backend().buffer().clone();
    let indent_cell = buffer.cell((0, 1)).expect("cell should exist");
    assert_eq!(indent_cell.symbol(), " ");
    let w_cell = buffer.cell((2, 1)).expect("cell should exist");
    assert_eq!(w_cell.symbol(), "w");
}

#[rstest::rstest]
fn render_multiline_cursor_on_second_line() {
    // Given a ChatInputBoxElement in Input mode with "ab\ncd" and cursor at end.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("ab\ncd"));
        s
    };

    let (mut terminal, area) = setup_term(40, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (4, 1): row 1, col 2.
    // inner.x=0, indent=2, col=2 → x=4, y=inner.y + 1 = 0 + 1 = 1.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 4, y: 1 });
}

#[rstest::rstest]
fn render_multiline_cursor_between_newlines() {
    // Given a ChatInputBoxElement in Input mode with "a\n\nb" and cursor on the empty middle line.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("a\n\nb"));
        // Cursor is at end (pos 4). Move back 1 to be on the empty middle line.
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // now at pos 3, which is after the second \n, before 'b'
        // Actually: "a\n\nb" → graphemes: a(0) \n(1) \n(2) b(3). cursor at 3 = before 'b'.
        // Move left once more to be at pos 2 = after first \n, on empty line.
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left);
        s
    };

    let (mut terminal, area) = setup_term(40, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is on row 1 (empty middle line), col 0.
    // inner.y=0, row=1 → y=1, indent=2, col=0 → x=2.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 2, y: 1 });
}

#[rstest::rstest]
fn render_wraps_long_text() {
    // Given "hello world" in a narrow terminal (width 10) so it wraps.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("hello world"));
        // Set wrap width to simulate narrow terminal: 10 - 2 prefix = 8
        s.update_active_input(|i| i.set_wrap_width(8));
        s
    };

    let (mut terminal, area) = setup_term(10, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the text is rendered across multiple visual lines.
    let buffer = terminal.backend().buffer().clone();
    // Row 0 should have "> hello " (prefix + first part of wrapped text).
    let cell = buffer.cell((2, 0)).expect("cell should exist");
    assert_eq!(cell.symbol(), "h");
    // Row 1 should have continuation with "world".
    let w_cell = buffer.cell((2, 1)).expect("cell should exist");
    assert_eq!(w_cell.symbol(), "w");
}

#[rstest::rstest]
fn render_cursor_on_wrapped_continuation() {
    // Given "hello world" in a narrow terminal with cursor on wrapped line.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("hello world"));
        s.update_active_input(|i| i.set_wrap_width(8));
        s
    };

    let (mut terminal, area) = setup_term(10, 5);

    // When rendering (cursor is at end, which is on the wrapped continuation line).
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is on row 1 (the continuation line).
    let _buffer = terminal.backend().buffer().clone();
    // The cursor should be visible on the second visual line.
    // Cursor at pos 11 (end). Wrapped lines: "> hello " and "  world".
    // Row 0 = "> hello " (8 graphemes), Row 1 = "  world" (5 graphemes).
    // cursor_row_col returns (1, 5) - row 1, col 5.
    // visual_row = 1, cursor_y = inner.y + 1 = 1.
    // cursor_x = inner.x + 2 + 5 = 7.
    terminal
        .backend_mut()
        .assert_cursor_position(Position { x: 7, y: 1 });
}

#[rstest::rstest]
fn indicator_shows_up_arrow_when_lines_hidden_above() {
    // Given a narrow terminal with 3 visible rows and 5 total lines, scrolled to offset 2.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        // 5 logical lines, narrow width so each wraps to 1 visual line.
        s.update_active_input(|i| i.insert_text("line1\nline2\nline3\nline4\nline5"));
        s.update_active_input(|i| i.set_wrap_width(38));
        s.update_active_input(|i| i.set_scroll_offset(2));
        s
    };

    let (mut terminal, area) = setup_term(40, 4);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the up arrow indicator appears on the top-right.
    // "↑ 2" = 3 display cols, right-aligned on row 0 at x = 40 - 3 = 37.
    let buffer = terminal.backend().buffer().clone();
    let arrow_cell = buffer.cell((37, 0)).expect("cell should exist");
    assert_eq!(arrow_cell.symbol(), "↑");
    assert_eq!(arrow_cell.style().fg, Some(default_theme().age_fresh));
    assert_eq!(
        arrow_cell.style().bg,
        Some(default_theme().scroll_indicator_bg)
    );
    let num_cell = buffer.cell((39, 0)).expect("cell should exist");
    assert_eq!(num_cell.symbol(), "2");
}

#[rstest::rstest]
fn indicator_shows_down_arrow_when_lines_hidden_below() {
    // Given a narrow terminal with 3 visible rows and 5 total lines, scrolled to offset 0.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("line1\nline2\nline3\nline4\nline5"));
        s.update_active_input(|i| i.set_wrap_width(38));
        // scroll_offset = 0, so lines_above = 0, lines_below = 5 - 0 - 3 = 2.
        s
    };

    let (mut terminal, area) = setup_term(40, 4);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the down arrow indicator appears on the bottom-right of the inner area.
    // inner area: rows 0..2 (3 rows), bottom row is y=2.
    // "↓ 2" = 3 display cols, right-aligned at x = 40 - 3 = 37, y = 2.
    let buffer = terminal.backend().buffer().clone();
    let arrow_cell = buffer.cell((37, 2)).expect("cell should exist");
    assert_eq!(arrow_cell.symbol(), "↓");
    assert_eq!(arrow_cell.style().fg, Some(default_theme().age_fresh));
    assert_eq!(
        arrow_cell.style().bg,
        Some(default_theme().scroll_indicator_bg)
    );
    let num_cell = buffer.cell((39, 2)).expect("cell should exist");
    assert_eq!(num_cell.symbol(), "2");
}

#[rstest::rstest]
fn indicator_shows_both_arrows_when_viewport_in_middle() {
    // Given a narrow terminal with 3 visible rows and 7 total lines, scrolled to offset 2.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("line1\nline2\nline3\nline4\nline5\nline6\nline7"));
        s.update_active_input(|i| i.set_wrap_width(38));
        s.update_active_input(|i| i.set_scroll_offset(2));
        // lines_above = 2, lines_below = 7 - 2 - 3 = 2.
        s
    };

    let (mut terminal, area) = setup_term(40, 4);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then both indicators appear.
    let buffer = terminal.backend().buffer().clone();

    // Up arrow on top-right (row 0). "↑ 2" = 3 display cols → x = 37.
    let up_cell = buffer.cell((37, 0)).expect("cell should exist");
    assert_eq!(up_cell.symbol(), "↑");
    assert_eq!(up_cell.style().fg, Some(default_theme().age_fresh));

    // Down arrow on bottom-right of inner area (row 2). "↓ 2" = 3 display cols → x = 37.
    let down_cell = buffer.cell((37, 2)).expect("cell should exist");
    assert_eq!(down_cell.symbol(), "↓");
    assert_eq!(down_cell.style().fg, Some(default_theme().age_fresh));
}

#[rstest::rstest]
fn no_indicators_when_content_fits() {
    // Given a terminal where all content fits without scrolling.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(|i| i.insert_text("hello"));
        s.update_active_input(|i| i.set_wrap_width(38));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no arrow indicators appear on the right edge.
    let buffer = terminal.backend().buffer().clone();
    // Check that the rightmost cell on row 0 is NOT an arrow.
    let right_cell = buffer.cell((39, 0)).expect("cell should exist");
    assert_ne!(right_cell.symbol(), "↑");
    assert_ne!(right_cell.symbol(), "↓");
}

#[rstest::rstest]
fn render_cursor_after_cjk() {
    // Given a ChatInputBoxElement in Input mode with CJK text "中文".
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("中文"));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (6, 0): inner.x=0 + "> "=2 + "中文"=4 display cols.
    terminal
        .backend_mut()
        .assert_cursor_position(ratatui::layout::Position { x: 6, y: 0 });
}

#[rstest::rstest]
fn render_cursor_after_emoji() {
    // Given a ChatInputBoxElement in Input mode with emoji "🎉🎉".
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("🎉🎉"));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (6, 0): inner.x=0 + "> "=2 + "🎉🎉"=4 display cols.
    terminal
        .backend_mut()
        .assert_cursor_position(ratatui::layout::Position { x: 6, y: 0 });
}

#[rstest::rstest]
fn render_cursor_mixed_ascii_cjk() {
    // Given a ChatInputBoxElement in Input mode with mixed "a中b" and cursor at pos 2 (after "中").
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.frontend.scope_push(FocusScope::Input);
        s.update_active_input(|i| i.insert_text("a中b"));
        // Cursor at end (pos 3). Move left once to pos 2 (after "中").
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left);
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then cursor is at position (5, 0): inner.x=0 + "> "=2 + "a"=1 + "中"=2 = 5.
    terminal
        .backend_mut()
        .assert_cursor_position(ratatui::layout::Position { x: 5, y: 0 });
}

// ===== Mode badge rendering tests =====

#[rstest::rstest]
fn render_queue_badge_in_queue_mode() {
    // Given a ChatInputBoxElement toggled to Queue mode with empty buffer.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::toggle_input_mode); // Steer → Queue
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the bottom border shows [Q:QUEUE] starting at x=2 (aligned with the cursor
    // column), exposing the `─` border line in columns 0 and 1. The leading `Q` is the
    // orange accent_action; everything else is input_mode_queue.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let border0_cell = buffer.cell((0, 2)).expect("cell should exist");
    assert_eq!(border0_cell.symbol(), "─");
    let border1_cell = buffer.cell((1, 2)).expect("cell should exist");
    assert_eq!(border1_cell.symbol(), "─");
    // [ at x=2 — input_mode_queue
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    assert_eq!(bracket_cell.style().fg, Some(theme.input_mode_queue));
    // Q at x=3 — accent_action (orange hotkey)
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.symbol(), "Q");
    assert_eq!(q_cell.style().fg, Some(theme.accent_action));
    // : at x=4 — input_mode_queue
    let colon_cell = buffer.cell((4, 2)).expect("cell should exist");
    assert_eq!(colon_cell.symbol(), ":");
    assert_eq!(colon_cell.style().fg, Some(theme.input_mode_queue));
    // ] at x=10 — input_mode_queue
    let close_cell = buffer.cell((10, 2)).expect("cell should exist");
    assert_eq!(close_cell.symbol(), "]");
    assert_eq!(close_cell.style().fg, Some(theme.input_mode_queue));
}

#[rstest::rstest]
fn render_steer_badge_in_steer_mode() {
    // Given a ChatInputBoxElement in default (Steer) mode.
    let mut element = ChatInputBoxElement;
    let state = AppState::default_with_scope_focus();

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the bottom border shows [Q:STEER] starting at x=2 (aligned with the cursor
    // column), exposing the `─` border line in columns 0 and 1. The leading `Q` is the
    // orange accent_action; everything else is input_mode_steer (magenta).
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let border0_cell = buffer.cell((0, 2)).expect("cell should exist");
    assert_eq!(border0_cell.symbol(), "─");
    let border1_cell = buffer.cell((1, 2)).expect("cell should exist");
    assert_eq!(border1_cell.symbol(), "─");
    // [ at x=2 — input_mode_steer (magenta)
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    assert_eq!(bracket_cell.style().fg, Some(theme.input_mode_steer));
    // Q at x=3 — accent_action (orange hotkey)
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.symbol(), "Q");
    assert_eq!(q_cell.style().fg, Some(theme.accent_action));
    // : at x=4 — input_mode_steer (magenta)
    let colon_cell = buffer.cell((4, 2)).expect("cell should exist");
    assert_eq!(colon_cell.symbol(), ":");
    assert_eq!(colon_cell.style().fg, Some(theme.input_mode_steer));
    // ] at x=10 — input_mode_steer (magenta)
    let close_cell = buffer.cell((10, 2)).expect("cell should exist");
    assert_eq!(close_cell.symbol(), "]");
    assert_eq!(close_cell.style().fg, Some(theme.input_mode_steer));
}

#[rstest::rstest]
fn render_steer_badge_shows_buffer_count_when_nonzero() {
    // Given Steer mode with 2 fragments buffered.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .steering_buffer_mut()
            .push_fragment("first".to_owned());
        s.active_session_mut()
            .steering_buffer_mut()
            .push_fragment("second".to_owned());
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the badge shows [Q:STEER · 2] (13 display cells), starting at x=2
    // (aligned with the cursor column), exposing the `─` border line in columns 0 and 1.
    // The `Q` is accent_action (orange); everything else is input_mode_steer (magenta).
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let border0_cell = buffer.cell((0, 2)).expect("cell should exist");
    assert_eq!(border0_cell.symbol(), "─");
    let border1_cell = buffer.cell((1, 2)).expect("cell should exist");
    assert_eq!(border1_cell.symbol(), "─");
    // [ at x=2 — input_mode_steer
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    assert_eq!(bracket_cell.style().fg, Some(theme.input_mode_steer));
    // Q at x=3 — accent_action
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.symbol(), "Q");
    assert_eq!(q_cell.style().fg, Some(theme.accent_action));
    // · at x=11 — input_mode_steer
    let dot_cell = buffer.cell((11, 2)).expect("cell should exist");
    assert_eq!(dot_cell.symbol(), "·");
    assert_eq!(dot_cell.style().fg, Some(theme.input_mode_steer));
    // 2 at x=13 — input_mode_steer
    let count_cell = buffer.cell((13, 2)).expect("cell should exist");
    assert_eq!(count_cell.symbol(), "2");
    assert_eq!(count_cell.style().fg, Some(theme.input_mode_steer));
    // ] at x=14 — input_mode_steer
    let close_cell = buffer.cell((14, 2)).expect("cell should exist");
    assert_eq!(close_cell.symbol(), "]");
    assert_eq!(close_cell.style().fg, Some(theme.input_mode_steer));
}

#[rstest::rstest]
fn render_queue_badge_shows_queue_count_when_nonzero() {
    // Given QUEUE mode with 2 queued user messages.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::toggle_input_mode); // Steer -> Queue
        s.active_session_mut()
            .enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("first"))));
        s.active_session_mut()
            .enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("second"))));
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the badge shows [Q:QUEUE · 2] (13 display cells), starting at x=2
    // (aligned with the cursor column). The `Q` is accent_action (orange);
    // everything else is input_mode_queue.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    // [ at x=2 — input_mode_queue
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    assert_eq!(bracket_cell.style().fg, Some(theme.input_mode_queue));
    // Q at x=3 — accent_action
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.symbol(), "Q");
    assert_eq!(q_cell.style().fg, Some(theme.accent_action));
    // · at x=11 — input_mode_queue
    let dot_cell = buffer.cell((11, 2)).expect("cell should exist");
    assert_eq!(dot_cell.symbol(), "·");
    assert_eq!(dot_cell.style().fg, Some(theme.input_mode_queue));
    // 2 at x=13 — input_mode_queue
    let count_cell = buffer.cell((13, 2)).expect("cell should exist");
    assert_eq!(count_cell.symbol(), "2");
    assert_eq!(count_cell.style().fg, Some(theme.input_mode_queue));
    // ] at x=14 — input_mode_queue
    let close_bracket_cell = buffer.cell((14, 2)).expect("cell should exist");
    assert_eq!(close_bracket_cell.symbol(), "]");
    assert_eq!(close_bracket_cell.style().fg, Some(theme.input_mode_queue));
}

#[rstest::rstest]
fn render_queue_badge_no_count_when_buffer_empty() {
    // Given Queue mode (default) - even if buffer had fragments, badge width is just [QUEUE].
    let mut element = ChatInputBoxElement;
    let state = AppState::default_with_scope_focus();

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the badge shows [Q:QUEUE] (9 display cells), starting at x=2
    // (aligned with the cursor column), exposing the `─` border line in columns 0, 1,
    // and resuming at x=11 (right after the 9-cell badge at x=2..11).
    let buffer = terminal.backend().buffer().clone();
    let border0_cell = buffer.cell((0, 2)).expect("cell should exist");
    assert_eq!(border0_cell.symbol(), "─");
    let border1_cell = buffer.cell((1, 2)).expect("cell should exist");
    assert_eq!(border1_cell.symbol(), "─");
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    let close_cell = buffer.cell((10, 2)).expect("cell should exist");
    assert_eq!(close_cell.symbol(), "]");
    // x = 11 (right after the 9-cell badge at x=2..11) is the bottom-border line character.
    let right_cell = buffer.cell((11, 2)).expect("cell should exist");
    assert_eq!(right_cell.symbol(), "─");
}

// ===== Muted badge rendering tests (Normal scope) =====

#[rstest::rstest]
fn steer_badge_is_muted_in_normal_mode() {
    // Given a ChatInputBoxElement in Steer mode with Normal (browsing) scope.
    let mut element = ChatInputBoxElement;
    let state = {
        let s = AppState::default_with_scope_focus();
        s.frontend.scope_pop(); // pop Input → back to Normal
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then all badge spans use muted_text — the badge is purely informational
    // when the input box is unfocused.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    // [ at x=2
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.symbol(), "[");
    assert_eq!(bracket_cell.style().fg, Some(theme.muted_text));
    // Q at x=3
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.symbol(), "Q");
    assert_eq!(q_cell.style().fg, Some(theme.muted_text));
    // : at x=4
    let colon_cell = buffer.cell((4, 2)).expect("cell should exist");
    assert_eq!(colon_cell.symbol(), ":");
    assert_eq!(colon_cell.style().fg, Some(theme.muted_text));
    // ] at x=10
    let close_cell = buffer.cell((10, 2)).expect("cell should exist");
    assert_eq!(close_cell.symbol(), "]");
    assert_eq!(close_cell.style().fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn queue_badge_is_muted_in_normal_mode() {
    // Given a ChatInputBoxElement toggled to Queue mode with Normal (browsing) scope.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::toggle_input_mode); // Steer → Queue
        s.frontend.scope_pop(); // pop Input → back to Normal
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then all badge spans use muted_text.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.style().fg, Some(theme.muted_text));
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.style().fg, Some(theme.muted_text));
    let colon_cell = buffer.cell((4, 2)).expect("cell should exist");
    assert_eq!(colon_cell.style().fg, Some(theme.muted_text));
    let close_cell = buffer.cell((10, 2)).expect("cell should exist");
    assert_eq!(close_cell.style().fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn steer_badge_count_is_muted_in_normal_mode() {
    // Given Steer mode with 2 fragments buffered, Normal scope.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .steering_buffer_mut()
            .push_fragment("first".to_owned());
        s.active_session_mut()
            .steering_buffer_mut()
            .push_fragment("second".to_owned());
        s.frontend.scope_pop(); // pop Input → back to Normal
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then all badge spans including the count use muted_text.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.style().fg, Some(theme.muted_text));
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.style().fg, Some(theme.muted_text));
    let dot_cell = buffer.cell((11, 2)).expect("cell should exist");
    assert_eq!(dot_cell.style().fg, Some(theme.muted_text));
    let count_cell = buffer.cell((13, 2)).expect("cell should exist");
    assert_eq!(count_cell.style().fg, Some(theme.muted_text));
    let close_cell = buffer.cell((14, 2)).expect("cell should exist");
    assert_eq!(close_cell.style().fg, Some(theme.muted_text));
}

#[rstest::rstest]
fn queue_badge_count_is_muted_in_normal_mode() {
    // Given Queue mode with 2 queued items, Normal scope.
    let mut element = ChatInputBoxElement;
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.update_active_input(jinn_chat_input_msg::ChatInputBoxState::toggle_input_mode); // Steer → Queue
        s.active_session_mut()
            .enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("first"))));
        s.active_session_mut()
            .enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("second"))));
        s.frontend.scope_pop(); // pop Input → back to Normal
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then all badge spans including the count use muted_text.
    let buffer = terminal.backend().buffer().clone();
    let theme = default_theme();
    let bracket_cell = buffer.cell((2, 2)).expect("cell should exist");
    assert_eq!(bracket_cell.style().fg, Some(theme.muted_text));
    let q_cell = buffer.cell((3, 2)).expect("cell should exist");
    assert_eq!(q_cell.style().fg, Some(theme.muted_text));
    let dot_cell = buffer.cell((11, 2)).expect("cell should exist");
    assert_eq!(dot_cell.style().fg, Some(theme.muted_text));
    let count_cell = buffer.cell((13, 2)).expect("cell should exist");
    assert_eq!(count_cell.style().fg, Some(theme.muted_text));
    let close_cell = buffer.cell((14, 2)).expect("cell should exist");
    assert_eq!(close_cell.style().fg, Some(theme.muted_text));
}

use jinn_chat_input::{AutocompleteMatch, AutocompleteTrigger, InputMode};
use jinn_chat_input_msg::{FileEntry, FilePickerState};
use jinn_session_msg::PhaseKind;

/// Empty slice registry + route table for handler tests that don't
/// exercise slices or route rows.
fn empty_slices() -> jinn_slices::Slices {
    jinn_slices::Slices::new()
}

fn empty_routes() -> jinn_slices::route::KeyRoutes {
    jinn_slices::route::KeyRoutes::new()
}

#[rstest::rstest]
fn insert_char_appends_to_buffer() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('x').
    let _ = jinn_chat_input::intent::handle_insert_char('x', &mut state);

    // Then the character is in the input buffer.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "x"
    );
}

#[rstest::rstest]
fn insert_char_emits_no_commands() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('x').
    let result = jinn_chat_input::intent::handle_insert_char('x', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn delete_grapheme_removes_last_char() {
    // Given a state with "ab" in the input buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));

    // When handling DeleteGrapheme.
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then the buffer is "a".
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "a"
    );
}

#[rstest::rstest]
fn delete_grapheme_emits_no_commands() {
    // Given a state with "ab" in the input buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));

    // When handling DeleteGrapheme.
    let result = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn delete_grapheme_forward_removes_next_char() {
    // Given a state with "ab" and cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling DeleteGraphemeForward.
    let _ = jinn_chat_input::intent::handle_delete_grapheme_forward(&mut state);

    // Then the buffer is "b".
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "b"
    );
}

#[rstest::rstest]
fn delete_grapheme_forward_emits_no_commands() {
    // Given a state with "ab" and cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling DeleteGraphemeForward.
    let result = jinn_chat_input::intent::handle_delete_grapheme_forward(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn submit_message_returns_enqueue_command() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then a MarkSessionInteracted and an EnqueueUserMessage command are returned.
    assert_eq!(result.message_names.len(), 2);
    assert!(result.message_names[0].contains("MarkSessionInteracted"));
    assert!(result.message_names[1].contains("EnqueueUserMessage"));
}

#[rstest::rstest]
fn submit_message_clears_input_buffer() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the input buffer is reset.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn submit_message_noop_with_empty_buffer() {
    // Given a state with an empty buffer.
    let mut state = AppState::default_with_scope_focus();

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no commands are returned.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn submit_message_completes_and_submits_when_hash_autocomplete_active() {
    // Given a state with text and hash autocomplete active.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#cod"));
    let matches = vec![AutocompleteMatch {
        name: "code-review".to_owned(),
        description: "Perform code review".to_owned(),
    }];
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, matches));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the autocomplete is completed and the message is submitted.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage")),
        "Enter should complete autocomplete and submit the message"
    );
}

#[rstest::rstest]
fn submit_message_with_hash_autocomplete_clears_buffer() {
    // Given a state with text and hash autocomplete active.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#cod"));
    let matches = vec![AutocompleteMatch {
        name: "code-review".to_owned(),
        description: "Perform code review".to_owned(),
    }];
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, matches));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the input buffer is cleared.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

// ─── Input mode & routing (steering) ──────────────────────────────────

#[rstest::rstest]
fn toggle_input_mode_flips_steer_to_queue() {
    // Given default state (mode = Steer).
    let mut state = AppState::default_with_scope_focus();
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::input_mode,
            Default::default
        ),
        InputMode::Steer,
        "default mode is Steer"
    );

    // When toggling.
    jinn_chat_input::intent::handle_toggle_input_mode(&mut state);

    // Then mode is Queue.
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::input_mode,
            Default::default
        ),
        InputMode::Queue
    );
    // And no commands emitted.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn toggle_input_mode_is_sticky_across_submissions() {
    // Given default (Steer) mode with text typed.
    let mut state = AppState::default_with_scope_focus();

    state.update_active_input(|i| i.insert_text("h"));

    // When submitting while Idle (falls back to enqueue).
    let _ = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then mode remains Steer (sticky).
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::input_mode,
            Default::default
        ),
        InputMode::Steer,
        "mode sticky across submissions"
    );
}

#[rstest::rstest]
fn queue_submit_always_enqueues() {
    // Given Queue mode (toggled from default Steer) with text typed, mid-stream.
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_toggle_input_mode(&mut state); // Steer → Queue
    state.session.active_session_mut().begin_streaming();
    state.update_active_input(|i| i.insert_text("h"));

    // When submitting.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then an EnqueueUserMessage command is emitted.
    assert_eq!(result.message_names.len(), 2);
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage"))
    );
}

#[rstest::rstest]
fn steer_submit_while_busy_routes_to_steer(
    #[values(PhaseKind::Streaming, PhaseKind::Sending)] phase: PhaseKind,
) {
    // Given default (Steer) mode + a non-Idle phase with text typed.
    let mut state = AppState::default_with_scope_focus();

    match phase {
        PhaseKind::Streaming => state.session.active_session_mut().begin_streaming(),
        PhaseKind::Sending => state.session.active_session_mut().begin_sending(),
        PhaseKind::Idle => unreachable!("Idle is the fall-through case, tested separately"),
    }
    // Sanity: phase is not Idle.
    assert_ne!(
        state.session.active_session().phase(),
        PhaseKind::Idle,
        "test setup: phase must not be Idle"
    );
    state.update_active_input(|i| i.insert_text("h"));

    // When submitting.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then SubmitSteeringMessage command emitted (not EnqueueUserMessage).
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("SubmitSteeringMessage")),
        "phase {:?}: expected SubmitSteeringMessage",
        phase
    );
    // And no new history entry was created.
    assert!(
        state.session.active_session().history().is_empty(),
        "phase {:?}: no entry should appear in history yet",
        phase
    );
}

#[rstest::rstest]
#[test]
fn steer_submit_while_idle_falls_back_to_enqueue() {
    // Given default (Steer) mode + Idle phase with text typed.
    let mut state = AppState::default_with_scope_focus();

    state.update_active_input(|i| i.insert_text("h"));

    // Sanity check phase is Idle.
    assert_eq!(state.session.active_session().phase(), PhaseKind::Idle);

    // When submitting.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then EnqueueUserMessage command emitted (fall-through).
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage"))
    );
    // And mode display remains Steer.
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::input_mode,
            Default::default
        ),
        InputMode::Steer,
        "mode display unaffected by Idle fall-through"
    );
}

#[rstest::rstest]
fn autocomplete_confirm_no_op_when_no_autocomplete() {
    // Given a state with no autocomplete active.
    let mut state = AppState::default_with_scope_focus();

    // When handling AutocompleteConfirm.
    let result = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then nothing changes and no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_left_moves_cursor() {
    // Given a state with "ab" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        2
    );

    // When handling MoveCursorLeft.
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then the cursor has moved.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        1
    );
}

#[rstest::rstest]
fn move_cursor_left_emits_no_commands() {
    // Given a state with "ab" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        2
    );

    // When handling MoveCursorLeft.
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_right_moves_cursor() {
    // Given a state with cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorRight.
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);

    // Then the cursor has moved.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        1
    );
}

#[rstest::rstest]
fn move_cursor_right_emits_no_commands() {
    // Given a state with cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("ab"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorRight.
    let result = jinn_chat_input::intent::handle_move_cursor_right(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_to_start_moves_cursor() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling MoveCursorToStart.
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);

    // Then cursor is at position 0.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        0
    );
}

#[rstest::rstest]
fn move_cursor_to_start_emits_no_commands() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling MoveCursorToStart.
    let result = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_to_end_moves_cursor() {
    // Given a state with cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_grapheme_at_cursor('a'));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorToEnd.
    let _ = jinn_chat_input::intent::handle_move_cursor_to_end(&mut state);

    // Then cursor is at the end.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        1
    );
}

#[rstest::rstest]
fn move_cursor_to_end_emits_no_commands() {
    // Given a state with cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_grapheme_at_cursor('a'));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorToEnd.
    let result = jinn_chat_input::intent::handle_move_cursor_to_end(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_word_left_moves_cursor() {
    // Given a state with "hello world".
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling MoveCursorWordLeft.
    let _ = jinn_chat_input::intent::handle_move_cursor_word_left(&mut state);

    // Then cursor moves.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        0
    );
}

#[rstest::rstest]
fn move_cursor_word_left_emits_no_commands() {
    // Given a state with "hello world".
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));

    // When handling MoveCursorWordLeft.
    let result = jinn_chat_input::intent::handle_move_cursor_word_left(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_word_right_moves_cursor() {
    // Given a state with "hi" and cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorWordRight.
    let _ = jinn_chat_input::intent::handle_move_cursor_word_right(&mut state);

    // Then cursor moves to end.
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        2
    );
}

#[rstest::rstest]
fn move_cursor_word_right_emits_no_commands() {
    // Given a state with "hi" and cursor at start.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hi"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);

    // When handling MoveCursorWordRight.
    let result = jinn_chat_input::intent::handle_move_cursor_word_right(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn cursor_left_reactivates_autocomplete_when_re_entering_token() {
    // Given a state with "#code " in the buffer (autocomplete was dismissed by space).
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code "));
    // Cursor is at end (after space). Move left twice to get back into "code".
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // cursor on space
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // cursor on 'e'

    // When handling MoveCursorLeft.
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then autocomplete is re-activated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "autocomplete should be re-activated when cursor re-enters token"
    );
}

#[rstest::rstest]
fn cursor_left_reactivates_autocomplete_emits_no_commands() {
    // Given a state with "#code " in the buffer (autocomplete was dismissed by space).
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code "));
    // Cursor is at end (after space). Move left twice to get back into "code".
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // cursor on space
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // cursor on 'e'

    // When handling MoveCursorLeft.
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn backspace_reactivates_autocomplete_when_re_entering_token() {
    // Given a state with "#code " and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code "));

    // When handling DeleteGrapheme (removes the space).
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then autocomplete is re-activated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "autocomplete should be re-activated when backspace re-enters token"
    );
}

#[rstest::rstest]
fn backspace_reactivates_autocomplete_emits_no_commands() {
    // Given a state with "#code " and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code "));

    // When handling DeleteGrapheme (removes the space).
    let result = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn cursor_move_away_from_token_does_not_reactivate() {
    // Given a state with "hello #code" and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello #code"));

    // When moving cursor left past the token boundary (into "hello ").
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'e'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'd'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'o'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'c'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // '#'
    // Now cursor is on '#'. Move left one more to space.
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then autocomplete is NOT active (cursor left the token region).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "autocomplete should NOT activate when cursor moves away from token"
    );
}

#[rstest::rstest]
fn cursor_move_away_emits_no_commands() {
    // Given a state with "hello #code" and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello #code"));

    // When moving cursor left past the token boundary (into "hello ").
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'e'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'd'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'o'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // 'c'
    jinn_chat_input::intent::handle_move_cursor_left(&mut state); // '#'
    // Now cursor is on '#'. Move left one more to space.
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_up_delegates_to_state() {
    // Given a default state.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_grapheme_at_cursor('a'));

    // When handling MoveCursorUp.
    let result = jinn_chat_input::intent::handle_move_cursor_up(&mut state);

    // Then no crash and no commands.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn move_cursor_down_delegates_to_state() {
    // Given a default state.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_grapheme_at_cursor('a'));

    // When handling MoveCursorDown.
    let result = jinn_chat_input::intent::handle_move_cursor_down(&mut state);

    // Then no crash and no commands.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_insert_mode_sets_mode_to_input() {
    // Given a state in Normal mode.
    let mut state = AppState::default_with_scope_focus();

    // When handling EnterInsertMode.
    let _ = jinn_chat_input::intent::handle_enter_insert_mode(&mut state);

    // Then the scope stack has Input on top.
    assert_eq!(
        state.frontend.scope().mode(),
        jinn_kernel::protocol::Mode::Input
    );
}

#[rstest::rstest]
fn enter_insert_mode_emits_no_commands() {
    // Given a state in Normal mode.
    let mut state = AppState::default_with_scope_focus();

    // When handling EnterInsertMode.
    let result = jinn_chat_input::intent::handle_enter_insert_mode(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_normal_mode_returns_to_normal_scope() {
    // Given a state in Input mode.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the scope is back to Normal.
    assert_eq!(state.frontend.scope(), FocusScope::Normal);
}

#[rstest::rstest]
fn enter_normal_mode_clears_pending_creation() {
    // Given a state with a stale pending session creation stash.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.pending_creation =
        Some(jinn_kernel::state::frontend_state::PendingSessionCreation {
            project_dir: std::path::PathBuf::from("/tmp/stale"),
            starting_cwd: std::path::PathBuf::from("/tmp/stale"),
        });

    // When handling EnterNormalMode (ESC from the project/lifecycle chain).
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the stash is cleared so it never leaks into a future session
    // creation.
    assert!(state.frontend.pending_creation.is_none());
}

#[rstest::rstest]
fn enter_normal_mode_from_input_emits_no_commands() {
    // Given a state in Input mode.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_normal_mode_clears_picker_kind_when_leaving_picker() {
    // Given a state in Picker mode with active picker kind.

    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(FocusScope::Dynamic(jinn_project_msg::project_picker_scope()));

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the scope is back to Normal (no picker).
    assert!(!state.frontend.is_picker());
    assert_eq!(state.frontend.scope(), FocusScope::Normal);
}

#[rstest::rstest]
fn enter_normal_mode_from_picker_emits_no_commands() {
    // Given a state in Picker mode with active picker kind.

    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(FocusScope::Dynamic(jinn_project_msg::project_picker_scope()));

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_normal_mode_from_input_with_sidebar_returns_to_normal() {
    // Given a state with sidebar and input on the scope stack.

    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state.frontend.scope_push(FocusScope::Input);

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the scope is back to Normal (not the sidebar persona section).
    assert_eq!(state.frontend.scope(), FocusScope::Normal);
    assert!(!state.frontend.is_sidebar());
}

#[rstest::rstest]
fn enter_normal_mode_from_sidebar_input_emits_no_commands() {
    // Given a state with sidebar and input on the scope stack.

    let mut state = AppState::default_with_scope_focus();
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
    state.frontend.scope_push(FocusScope::Input);

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_normal_mode_does_not_cancel_stream() {
    // Given a state in Input mode with active stream.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().begin_streaming();

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no CancelTurn command is emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("CancelTurn"))
    );
}

#[rstest::rstest]
fn enter_normal_mode_preserves_streaming_phase() {
    // Given a state in Input mode with active stream.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().begin_streaming();

    // When handling EnterNormalMode.
    let _result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the session is still streaming (not cancelled).
    assert!(matches!(
        state.active_session().phase(),
        PhaseKind::Streaming
    ));
}

#[rstest::rstest]
fn enter_normal_mode_does_not_drain_queue() {
    // Given a state in Input mode with active stream and queued messages.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("msg1"),
        )));
    state
        .active_session_mut()
        .enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("msg2"),
        )));

    // When handling EnterNormalMode.
    let _result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the queued messages are NOT drained.
    assert_eq!(state.active_session().queue_len(), 2);
    // And the input buffer is empty.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn enter_normal_mode_with_queue_emits_no_cancel_stream() {
    // Given a state in Input mode with active stream and queued messages.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("msg1"),
        )));
    state
        .active_session_mut()
        .enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("msg2"),
        )));

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no CancelTurn command is emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("CancelTurn"))
    );
}

#[rstest::rstest]
fn slash_at_position_0_triggers_autocomplete() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('/').
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then autocomplete is active with Slash trigger.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_some(),
        "autocomplete should be active after '/' at position 0"
    );
    assert_eq!(ac.as_ref().unwrap().trigger(), AutocompleteTrigger::Slash);
}

#[rstest::rstest]
fn slash_at_position_0_emits_no_commands() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('/').
    let result = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn slash_does_not_trigger_with_content() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));

    // When handling InsertChar('/').
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then autocomplete is NOT active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "autocomplete should NOT trigger when buffer has content"
    );
}

#[rstest::rstest]
fn slash_with_content_emits_no_commands() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));

    // When handling InsertChar('/').
    let result = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn slash_autocomplete_shows_new_command() {
    // Given a state where '/' was typed at position 0.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('/').
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then the autocomplete popup has the /new command.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default)
        .expect("autocomplete active");
    let names: Vec<String> = ac.matches().iter().map(|m| m.name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "new"),
        "expected 'new' in matches, got: {names:?}"
    );
}

#[rstest::rstest]
fn slash_autocomplete_filters_on_typing() {
    // Given a state with '/n' typed.
    let mut state = AppState::default_with_scope_focus();

    // When typing "/n".
    jinn_chat_input::intent::handle_insert_char('/', &mut state);
    jinn_chat_input::intent::handle_insert_char('n', &mut state);

    // Then the autocomplete filter matches 'n'.
    let filter = state
        .active_session()
        .with_input(
            jinn_chat_input_msg::ChatInputBoxState::autocomplete_filter,
            Default::default,
        )
        .unwrap_or_default();
    assert_eq!(filter, "n");

    // And the new command is still in the matches.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default)
        .expect("autocomplete active");
    let names: Vec<String> = ac.matches().iter().map(|m| m.name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "new"),
        "'new' should match filter 'n'"
    );
}

#[rstest::rstest]
fn slash_autocomplete_tab_completes_name() {
    // Given a state with slash autocomplete active.
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Navigate to the "new" entry (default selection is the last entry).
    // Entries: compact(0), compact-all(1), new(2). Default = 2 (= new).

    // When confirming autocomplete (Tab).
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the buffer contains "/new".
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "/new"
    );
}

#[rstest::rstest]
fn slash_autocomplete_tab_confirm_emits_no_commands() {
    // Given a state with slash autocomplete active.
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // When confirming autocomplete (Tab).
    let result = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then no commands were emitted (autocomplete confirm doesn't execute).
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn slash_autocomplete_dismisses_on_space() {
    // Given a state with slash autocomplete active.
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // When pressing space.
    jinn_chat_input::intent::handle_insert_char(' ', &mut state);

    // Then autocomplete is dismissed.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "autocomplete should be dismissed on space"
    );
}

#[rstest::rstest]
fn slash_autocomplete_reactivates_on_cursor_reentry() {
    // Given a state with "/ne " (autocomplete dismissed by space).
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);
    jinn_chat_input::intent::handle_insert_char('n', &mut state);
    jinn_chat_input::intent::handle_insert_char('e', &mut state);
    jinn_chat_input::intent::handle_insert_char(' ', &mut state);
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none()
    );

    // When moving cursor left back to 'e'.
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // on space
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // on 'e'
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state); // on 'n'

    // Then autocomplete is re-activated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "autocomplete should reactivate when cursor re-enters /token"
    );
}

#[rstest::rstest]
fn slash_autocomplete_cursor_reentry_emits_no_commands() {
    // Given a state with "/ne " (autocomplete dismissed by space).
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);
    jinn_chat_input::intent::handle_insert_char('n', &mut state);
    jinn_chat_input::intent::handle_insert_char('e', &mut state);
    jinn_chat_input::intent::handle_insert_char(' ', &mut state);
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none()
    );

    // When moving cursor left back to 'e'.
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // on space
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // on 'e'
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state); // on 'n'

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn slash_autocomplete_does_not_reactivate_after_cursor_leaves_token() {
    // Given a state with "a /ne" and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("a /ne"));

    // When moving cursor left to the space before '/'.
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // 'e'
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // 'n'
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // '/'
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state); // space

    // Then autocomplete is NOT active (slash was not at position 0).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "autocomplete should NOT reactivate for / not at position 0"
    );
}

#[rstest::rstest]
fn slash_autocomplete_cursor_leaves_token_emits_no_commands() {
    // Given a state with "a /ne" and cursor at end.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("a /ne"));

    // When moving cursor left to the space before '/'.
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // 'e'
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // 'n'
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_left); // '/'
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state); // space

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn submit_new_command_creates_session() {
    // Given a state with "/new" in the buffer (no autocomplete active).
    let mut state = AppState::default_with_scope_focus();
    let old_id = state.session.active_session_id().clone();
    state.update_active_input(|i| i.insert_text("/new"));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then a new session is created.
    assert_ne!(*state.session.active_session_id(), old_id);
    // And the input buffer is cleared.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn submit_new_command_emits_no_enqueue_command() {
    // Given a state with "/new" in the buffer (no autocomplete active).
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/new"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no EnqueueUserMessage was emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage")),
        "/new should not enqueue a chat message"
    );
}

#[rstest::rstest]
fn submit_unknown_slash_command_sends_as_chat() {
    // Given a state with "/lol" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/lol"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the message is submitted as a normal chat message.
    // MarkSessionInteracted + EnqueueUserMessage.
    assert_eq!(result.message_names.len(), 2);
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("MarkSessionInteracted")),
        "first command should be MarkSessionInteracted"
    );
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage")),
        "unknown /command should be sent as chat"
    );
}

#[rstest::rstest]
fn submit_unknown_slash_command_clears_buffer() {
    // Given a state with "/lol" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/lol"));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the buffer is cleared.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn submit_compact_slash_command_pushes_system_message() {
    // Given a state with "/compact" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/compact"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then a MarkSessionInteracted and TriggerCompaction command are dispatched.
    assert_eq!(result.message_names.len(), 2);
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("MarkSessionInteracted")),
        "first command should be MarkSessionInteracted"
    );
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("TriggerCompaction")),
        "second command should be TriggerCompaction"
    );
}

#[rstest::rstest]
fn submit_compact_slash_command_clears_buffer() {
    // Given a state with "/compact" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/compact"));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the buffer is cleared.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn submit_export_slash_command_dispatches_export_command() {
    // Given a state with "/export" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/export"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the export command is dispatched.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("ExportSessionToFile")),
        "commands were {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn submit_export_slash_command_clears_buffer() {
    // Given a state with "/export" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/export"));

    // When handling SubmitMessage.
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the buffer is cleared.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

/// A [`jinn_slices::PublishSink`] that keeps every payload published
/// through it, so a test can assert on what a command emitted.
#[derive(Default)]
struct RecordingSink {
    published: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
}

impl jinn_slices::PublishSink for RecordingSink {
    fn publish_schema(
        &self,
        schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        _name: &'static str,
    ) {
        self.published
            .lock()
            .expect("sink lock")
            .push((format!("{schema_id}"), payload));
    }
}

/// The export request a submit produced, decoded from the bus payloads.
fn export_request(
    result: jinn_kernel::protocol::IntentResult,
) -> jinn_export_msg::ExportSessionToFile {
    let sink = RecordingSink::default();
    for closure in result.messages {
        closure(&sink);
    }
    let published = sink.published.lock().expect("sink lock");
    let (_, payload) = published
        .iter()
        .find(|(id, _)| id.ends_with("ExportSessionToFile"))
        .expect("an export request is published");
    serde_json::from_value(payload.clone()).expect("decodes")
}

#[rstest::rstest]
fn submit_export_with_argument_carries_that_path() {
    // Given a state with "/export notes/session.md" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/export notes/session.md"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the request carries the typed path, not an empty one.
    assert_eq!(
        export_request(result).path,
        std::path::PathBuf::from("notes/session.md")
    );
}

#[rstest::rstest]
fn submit_export_argument_keeps_spaces_inside_it() {
    // Given a state with "/export my notes/session.md" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/export my notes/session.md"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the whole remainder is the path, spaces intact.
    assert_eq!(
        export_request(result).path,
        std::path::PathBuf::from("my notes/session.md")
    );
}

#[rstest::rstest]
fn submit_export_without_argument_carries_an_empty_path() {
    // Given a state with a bare "/export" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("/export"));

    // When handling SubmitMessage.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the path is empty, which is how the actor picks a default name.
    assert_eq!(export_request(result).path, std::path::PathBuf::new());
}

#[rstest::rstest]
fn tab_completes_name_without_executing() {
    // Given a state with slash autocomplete active ("/" typed, popup showing entries).
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);
    let old_id = state.session.active_session_id().clone();

    // The default selection is "new" (last entry). No navigation needed.
    // Entries: compact(0), compact-all(1), new(2). Default = 2 (= new).

    // When confirming autocomplete (Tab).
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the buffer contains "/new" (completed) but no session was created.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "/new"
    );
    assert_eq!(
        *state.session.active_session_id(),
        old_id,
        "session should not change on Tab confirm"
    );
}

#[rstest::rstest]
fn tab_confirm_slash_emits_no_commands() {
    // Given a state with slash autocomplete active ("/" typed, popup showing "new").
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // When confirming autocomplete (Tab).
    let result = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn enter_completes_and_executes_slash_command() {
    // Given a state with slash autocomplete active ("/" typed, popup showing entries).
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);
    let old_id = state.session.active_session_id().clone();

    // The default selection is the last entry ("new"). No navigation needed.
    // Entries: compact(0), compact-all(1), new(2). Default = 2 (= new).

    // When pressing Enter (SubmitMessage with autocomplete active).
    let _result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the command is completed and executed.
    assert_ne!(
        *state.session.active_session_id(),
        old_id,
        "session should change on Enter"
    );
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
}

#[rstest::rstest]
fn enter_slash_command_emits_no_enqueue() {
    // Given a state with slash autocomplete active ("/" typed, popup showing "new").
    let mut state = AppState::default_with_scope_focus();
    jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // When pressing Enter (SubmitMessage with autocomplete active).
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no EnqueueUserMessage was emitted.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage")),
        "/new should not enqueue a chat message"
    );
}

#[rstest::rstest]
fn paste_text_inserts_into_chat_input() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling PasteText with "hello\nworld".
    let _ = jinn_chat_input::intent::handle_paste_text("hello\nworld", &mut state);

    // Then the buffer contains the pasted text with newlines preserved.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "hello\nworld"
    );
}

#[rstest::rstest]
fn paste_text_emits_no_commands() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling PasteText with "hello\nworld".
    let result = jinn_chat_input::intent::handle_paste_text("hello\nworld", &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn paste_text_inserts_at_cursor_position() {
    // Given a state with "hello" and cursor at position 2.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_right);
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_right); // cursor at 2

    // When handling PasteText with "XY".
    let _ = jinn_chat_input::intent::handle_paste_text("XY", &mut state);

    // Then text is "heXYllo" and cursor is at 4.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "heXYllo"
    );
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        4
    );
}

#[rstest::rstest]
fn paste_text_at_cursor_emits_no_commands() {
    // Given a state with "hello" and cursor at position 2.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_right);
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_right); // cursor at 2

    // When handling PasteText with "XY".
    let result = jinn_chat_input::intent::handle_paste_text("XY", &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn hash_triggers_after_newline() {
    // Given a state with "hello\n" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello\n"));

    // When handling InsertChar('#').
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete is active with Hash trigger.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_some(),
        "autocomplete should be active after '#' on new line"
    );
    assert_eq!(ac.as_ref().unwrap().trigger(), AutocompleteTrigger::Hash);
}

#[rstest::rstest]
fn hash_after_newline_emits_no_commands() {
    // Given a state with "hello\n" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello\n"));

    // When handling InsertChar('#').
    let result = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn hash_triggers_after_newline_at_line_start() {
    // Given a state with "\n" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("\n"));

    // When handling InsertChar('#').
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete is active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "autocomplete should be active after '#' following newline"
    );
}

#[rstest::rstest]
fn hash_after_newline_start_emits_no_commands() {
    // Given a state with "\n" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("\n"));

    // When handling InsertChar('#').
    let result = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn hash_does_not_trigger_after_non_boundary_char() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));

    // When handling InsertChar('#').
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete is NOT active (preceded by 'o', not a boundary).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "autocomplete should NOT trigger when '#' is preceded by a non-boundary char"
    );
}

#[rstest::rstest]
fn hash_after_non_boundary_emits_no_commands() {
    // Given a state with "hello" in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));

    // When handling InsertChar('#').
    let result = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn hash_reactivates_when_cursor_enters_token_after_newline() {
    // Given a state with "hello\n#code" in buffer and no autocomplete.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello\n#code"));

    // When moving cursor left into the "#code" token.
    // Cursor starts at end (position 11). Move left to 'e' (position 10).
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then autocomplete is re-activated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "autocomplete should reactivate when cursor re-enters #token after newline"
    );
}

#[rstest::rstest]
fn hash_cursor_reentry_emits_no_commands() {
    // Given a state with "hello\n#code" in buffer and no autocomplete.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello\n#code"));

    // When moving cursor left into the "#code" token.
    // Cursor starts at end (position 11). Move left to 'e' (position 10).
    let result = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

/// A "#code hello" state with hash autocomplete active at token_start=0 and
/// the cursor parked at the start of the buffer.
fn hash_autocomplete_at_cursor_start() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code hello"));
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, vec![]));
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
    state
}

#[rstest::rstest]
fn cursor_right_to_token_end_keeps_autocomplete_active() {
    // Given "#code hello" with autocomplete active at token_start=0 and the
    // cursor at the start of the buffer.
    let mut state = hash_autocomplete_at_cursor_start();

    // When moving the cursor right through the token to token_end.
    // token_end = 5 (one past 'e'). Cursor at 5 == token_end, still "in" token.
    for _ in 0..5 {
        jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    }

    // Then autocomplete is still active (cursor at token_end).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "popup should stay open when cursor is at token_end"
    );
}

#[rstest::rstest]
fn cursor_right_past_token_deactivates_autocomplete() {
    // Given "#code hello" with autocomplete active at token_start=0 and the
    // cursor at the start of the buffer.
    let mut state = hash_autocomplete_at_cursor_start();

    // When moving the cursor right past the token end to position 6.
    for _ in 0..6 {
        jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    }

    // Then autocomplete is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "popup should close when cursor moves past token_end"
    );
}

#[rstest::rstest]
fn cursor_right_within_token_keeps_autocomplete_active() {
    // Given a state with "#code" and autocomplete active at token_start=0.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("#code"));
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, vec![]));

    // When moving the cursor right from the start to position 2.
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
    jinn_chat_input::intent::handle_move_cursor_right(&mut state); // cursor at 1
    jinn_chat_input::intent::handle_move_cursor_right(&mut state); // cursor at 2

    // Then autocomplete is still active (cursor within token, position 2 < token_end=5).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "popup should stay open when cursor is within token"
    );
}

#[rstest::rstest]
fn enter_normal_mode_deactivates_hash_autocomplete() {
    // Given a state in Input scope with hash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, vec![]));

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then autocomplete is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "ESC should deactivate hash autocomplete"
    );
}

#[rstest::rstest]
fn enter_normal_mode_with_hash_autocomplete_stays_in_input_scope() {
    // Given a state in Input scope with hash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, vec![]));

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then scope is still Input (not Normal).
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Input,
        "scope should stay in Input after dismissing autocomplete"
    );
}

#[rstest::rstest]
fn enter_normal_mode_deactivates_slash_autocomplete() {
    // Given a state in Input scope with slash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Slash, vec![]));

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then autocomplete is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "ESC should deactivate slash autocomplete"
    );
}

#[rstest::rstest]
fn enter_normal_mode_with_slash_autocomplete_stays_in_input_scope() {
    // Given a state in Input scope with slash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Slash, vec![]));

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then scope is still Input (not Normal).
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Input,
        "scope should stay in Input after dismissing slash autocomplete"
    );
}

#[rstest::rstest]
fn enter_normal_mode_without_autocomplete_switches_to_normal() {
    // Given a state in Input scope with no autocomplete.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    // When handling EnterNormalMode.
    let _ = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then scope switches to Normal.
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Normal,
        "ESC should switch to Normal when no autocomplete is active"
    );
}

#[rstest::rstest]
fn enter_normal_mode_dismissing_autocomplete_emits_no_commands() {
    // Given a state in Input scope with hash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Hash, vec![]));

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then no commands are emitted.
    assert!(result.message_names.is_empty());
}

#[rstest::rstest]
fn hash_autocomplete_populates_matches_from_template_store() {
    // Given a state with a template in the store.
    use jinn_context::PromptTemplate;

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![PromptTemplate {
            name: "my_template".to_owned(),
            description: "A test template".to_owned(),
            body: "template body".to_owned(),
        }]),
    );

    // When inserting '#' at position 0.
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete is active with non-empty matches.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_some(),
        "autocomplete should activate on '#' with templates"
    );
    let matches = ac.as_ref().unwrap().matches();
    assert!(
        !matches.is_empty(),
        "compute_matches should return at least one match for a populated store"
    );
    assert!(
        matches.iter().any(|m| m.name == "my_template"),
        "expected 'my_template' in matches: {matches:?}"
    );
}

#[rstest::rstest]
fn slash_autocomplete_populates_matches_from_slash_commands() {
    // Given a state in Input mode.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    // When inserting '/' at position 0.
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then autocomplete is active with non-empty matches.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(ac.is_some(), "autocomplete should activate on '/'");
    let matches = ac.as_ref().unwrap().matches();
    assert!(
        !matches.is_empty(),
        "compute_slash_matches should return at least one slash command"
    );
}

// ---------- CtrlClear on the chat-input scope ----------

#[rstest::rstest]
fn ctrl_clear_input_empties_chat_input_via_handler() {
    // Given a state in Input scope with text in the buffer.
    use jinn_kernel::IntentHandler;
    use jinn_kernel::protocol::KernelIntent;

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    let _ = jinn_chat_input::intent::handle_insert_char('h', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('i', &mut state);
    assert!(
        !state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        2
    );

    // When handling CtrlClear via the IntentHandler.
    let result = IntentHandler::handle(
        &KernelIntent::CtrlClear,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        jinn_slices::empty_config_layer(),
    );

    // Then the chat input is cleared and scope remains Input.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        }),
        "input buffer cleared"
    );
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        0,
        "cursor reset to 0"
    );
    assert!(result.message_names.is_empty(), "no commands emitted");
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Input,
        "scope remains Input (no quit, no escape)"
    );
}

#[rstest::rstest]
fn ctrl_clear_input_empty_is_noop_via_handler() {
    // Given a state in Input scope with empty buffer.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        })
    );

    // When handling CtrlClear via the IntentHandler.
    let result = IntentHandler::handle(
        &KernelIntent::CtrlClear,
        &mut state,
        &empty_slices(),
        &empty_routes(),
        jinn_slices::empty_config_layer(),
    );

    // Then nothing changes: no scope change, no commands, buffer still empty.
    assert!(
        state.with_active_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || {
            true
        }),
        "buffer still empty"
    );
    assert_eq!(
        state.with_active_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        0,
        "cursor still 0"
    );
    assert!(result.message_names.is_empty(), "no commands emitted");
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Input,
        "scope remains Input"
    );
}

// =============================================================================
// `@path` popup state tests.
//
// The user types `@` to open the file popup. The popup's entries live in
// the file-picker cell (populated by `DirectoryListerActor`), and the
// selection lives in `AutocompleteState`. These tests exercise the popup's
// state transitions directly through the intent handlers: trigger activation,
// slash-descent, backspace, cursor moves, and confirm (dir vs file).
//
// Because the popup reads the cell (async actor output), tests that need
// populated entries set them manually via the helper below.
// =============================================================================

/// Seeds the file-picker cell with `entries` and clears the spinner.
fn seed_file_picker(state: &AppState, entries: Vec<FileEntry>) {
    state.frontend.update_file_picker(|picker| {
        *picker = FilePickerState::with_entries(entries);
        picker.loading = false;
    });
}

/// Reads the file-picker cell's `entries`, or an empty list when the cell
/// is unregistered.
fn file_picker_entries(state: &AppState) -> Vec<FileEntry> {
    state
        .frontend
        .with_file_picker(|picker| picker.entries.clone())
        .unwrap_or_default()
}

/// Activates the `@` popup at the cursor and optionally seeds
// the file-picker cell with a listing. Returns the AppState for chaining.
fn at_popup_with_entries(entries: Vec<FileEntry>) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    // Type `@` at the start of the buffer to activate the popup.
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);
    seed_file_picker(&state, entries);
    state
}

/// Activates the `@` popup with a seeded listing and positions the cursor
// at the end of the given `filter` text (typed after the `@`).
fn at_popup_with_filter_and_entries(filter: &str, entries: Vec<FileEntry>) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);
    for ch in filter.chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }
    seed_file_picker(&state, entries);
    state
}

fn dir_entry(name: &str) -> FileEntry {
    FileEntry {
        name: name.to_owned(),
        is_dir: true,
    }
}

fn file_entry(name: &str) -> FileEntry {
    FileEntry {
        name: name.to_owned(),
        is_dir: false,
    }
}

fn emits_list_directory(result: &jinn_kernel::protocol::IntentResult) -> bool {
    result
        .message_names
        .iter()
        .any(|name| name.contains("ListDirectory"))
}

// -----------------------------------------------------------------------------
// Trigger activation.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn at_at_start_of_buffer_activates_popup() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('@') at the start of the buffer.
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then the autocomplete popup is active with the At trigger.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(ac.is_some(), "@ at start should activate the popup");
    assert_eq!(ac.as_ref().unwrap().trigger(), AutocompleteTrigger::At);
}

#[rstest::rstest]
fn at_after_space_activates_popup() {
    // Given a buffer with a space.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello "));

    // When handling InsertChar('@').
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then the popup activates.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "@ after a space should activate the popup"
    );
}

#[rstest::rstest]
fn at_after_newline_activates_popup() {
    // Given a buffer ending in a newline.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("line1\n"));

    // When handling InsertChar('@').
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then the popup activates.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "@ after a newline should activate the popup"
    );
}

#[rstest::rstest]
fn at_mid_word_does_not_activate_popup() {
    // Given a buffer with text but no trailing boundary.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("foo"));

    // When handling InsertChar('@').
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then the popup does NOT activate.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "foo@ (no boundary) should NOT activate the popup"
    );
}

#[rstest::rstest]
fn at_activation_emits_list_directory() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When handling InsertChar('@').
    let result = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then a ListDirectory command was emitted.
    assert!(
        emits_list_directory(&result),
        "@ activation should emit a ListDirectory command"
    );
}

// -----------------------------------------------------------------------------
// `@@` seam: stays literal, no At activation.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn at_at_stays_literal_no_popup() {
    // Given a default AppState.
    let mut state = AppState::default_with_scope_focus();

    // When typing `@@`.
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);

    // Then the buffer is the literal `@@`.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@@"
    );
    // And the popup is NOT active (the seam is reserved, no handler fires).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "@@ should not activate the AtAt popup (seam reserved)"
    );
}

// -----------------------------------------------------------------------------
// Typing `/` in an active popup descends into the directory.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn typing_slash_in_at_popup_inserts_slash() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When typing '/'.
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then the buffer contains `@foo/`.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@foo/"
    );
}

#[rstest::rstest]
fn typing_slash_in_at_popup_emits_list_directory() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When typing '/'.
    let result = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then a ListDirectory command was emitted for the deeper path.
    assert!(
        emits_list_directory(&result),
        "typing '/' should emit a ListDirectory for the deeper directory"
    );
}

#[rstest::rstest]
fn typing_slash_in_at_popup_keeps_popup_active() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When typing '/'.
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then the popup stays active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "popup should remain active after typing '/'"
    );
}

// -----------------------------------------------------------------------------
// Backspace within and across the token boundary.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn backspace_within_token_keeps_popup_active() {
    // Given an active @ popup with filter "foo" (cursor at end).
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When backspacing once (@foo| -> @fo|).
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then the popup stays active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "backspace within token should keep the popup active"
    );
    // And the filter is now "fo".
    assert_eq!(
        state
            .active_session()
            .with_input(
                jinn_chat_input_msg::ChatInputBoxState::autocomplete_filter,
                Default::default
            )
            .unwrap_or_default(),
        "fo"
    );
}

#[rstest::rstest]
fn backspace_within_token_updates_filter() {
    // Given an active @ popup with filter "foo" (cursor at end).
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When backspacing twice (@foo| -> @f|).
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then the filter is now "f".
    assert_eq!(
        state
            .active_session()
            .with_input(
                jinn_chat_input_msg::ChatInputBoxState::autocomplete_filter,
                Default::default
            )
            .unwrap_or_default(),
        "f"
    );
}

#[rstest::rstest]
fn backspace_to_at_keeps_popup_active() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When backspacing back to the `@` (@foo| -> @|).
    for _ in 0..3 {
        let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);
    }

    // Then the popup stays active: the cursor sits right after `@`, which is
    // the same as having just typed `@` (consistent with the `#`/`/` triggers).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "cursor adjacent to @ should keep the popup active"
    );
}

#[rstest::rstest]
fn backspace_to_at_leaves_at_in_buffer() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When backspacing back to the `@`.
    for _ in 0..3 {
        let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);
    }

    // Then the buffer is just `@`.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@"
    );
}

#[rstest::rstest]
fn backspace_deleting_at_removes_it() {
    // Given an active @ popup with filter "foo".
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When backspacing past the `@` (@foo| -> @| -> empty).
    for _ in 0..4 {
        let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);
    }

    // Then the buffer is empty.
    assert!(
        state
            .with_active_input(|i| i.text().to_owned(), String::new)
            .is_empty()
    );
    // And the popup is not active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none()
    );
}

// -----------------------------------------------------------------------------
// Cursor movement within and across the token boundary.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn cursor_left_within_token_keeps_popup_active() {
    // Given an active @ popup with filter "foo" (cursor at end).
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When moving the cursor left once.
    let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);

    // Then the popup stays active (still inside the token).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "cursor left within token should keep the popup active"
    );
}

#[rstest::rstest]
fn cursor_left_past_token_start_deactivates_popup() {
    // Given a buffer `x @foo` with the @ popup active and the cursor after `foo`.
    // The `@` is at a valid boundary (preceded by a space).
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("x "));
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);
    seed_file_picker(&state, vec![dir_entry("foo")]);
    for ch in "foo".chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }

    // When moving the cursor left until it sits in `x ` (before the `@`).
    for _ in 0..5 {
        let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);
    }

    // Then the popup is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "cursor left past token start should deactivate the popup"
    );
}

#[rstest::rstest]
fn cursor_right_within_token_keeps_popup_active() {
    // Given an active @ popup where the cursor is at the start of the filter.
    // Type `@foo`, then move the cursor left twice so it's between `@` and `foo`.
    let mut state = at_popup_with_filter_and_entries("foo", vec![dir_entry("foo")]);

    // When the cursor moves left three times and then right once (back into
    // the filter).
    for _ in 0..3 {
        let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);
    }
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);

    // Then the popup is active again (re-activated on cursor reentry).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "cursor right into token should keep/activate the popup"
    );
}

/// A `@foo bar` buffer with the @ popup deactivated: a space (which closes
/// the popup) and `bar` typed after `foo`, cursor left back into `foo`.
fn file_popup_deactivated_by_trailing_bar() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    let _ = jinn_chat_input::intent::handle_insert_char('@', &mut state);
    seed_file_picker(&state, vec![dir_entry("foo")]);
    for ch in "foo".chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }
    state.update_active_input(|i| i.insert_text(" bar"));
    for _ in 0..4 {
        let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);
    }
    state
}

#[rstest::rstest]
fn cursor_left_past_the_token_start_does_not_reactivate_the_popup() {
    // Given a buffer `@foo bar` whose @ popup was closed by the trailing space.
    let state = file_popup_deactivated_by_trailing_bar();

    // When reading the input's autocomplete state back.
    // (The fixture above already walked the cursor left back into `foo`.)

    // Then the popup is still closed — reactivation happens on re-ENTERING
    // the token from outside it, not on merely moving left inside it.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "moving left within the token does not reactivate a closed popup"
    );
}

#[rstest::rstest]
fn cursor_right_past_token_end_deactivates_popup() {
    // Given a buffer `@foo bar` with the @ popup active and the cursor back
    // inside `foo`.
    let mut state = file_popup_deactivated_by_trailing_bar();

    // When moving the cursor right past the token end (over `foo` and the
    // trailing space).
    for _ in 0..4 {
        let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    }

    // Then the popup is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "cursor right past token end should deactivate the popup"
    );
}

// -----------------------------------------------------------------------------
// Confirm: directory vs file branching.
// -----------------------------------------------------------------------------

#[rstest::rstest]
fn confirm_directory_entry_inserts_name_with_slash() {
    // Given an active @ popup with a directory selected at index 0.
    let mut state = at_popup_with_entries(vec![dir_entry("src")]);

    // When confirming the selection.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the buffer contains `@src/`.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@src/"
    );
}

#[rstest::rstest]
fn confirm_directory_entry_keeps_popup_active() {
    // Given an active @ popup with a directory selected at index 0.
    let mut state = at_popup_with_entries(vec![dir_entry("src")]);

    // When confirming the selection.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the popup stays active.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some(),
        "confirming a directory should keep the popup active"
    );
}

#[rstest::rstest]
fn confirm_directory_entry_emits_list_directory() {
    // Given an active @ popup with a directory selected at index 0.
    let mut state = at_popup_with_entries(vec![dir_entry("src")]);

    // When confirming the selection.
    let result = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then a ListDirectory command was emitted for the deeper path.
    assert!(
        emits_list_directory(&result),
        "confirming a directory should emit a ListDirectory for the deeper path"
    );
}

#[rstest::rstest]
fn confirm_file_entry_inserts_name() {
    // Given an active @ popup with a file selected at index 0.
    let mut state = at_popup_with_entries(vec![file_entry("img.png")]);

    // When confirming the selection.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the buffer contains `@img.png`.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@img.png"
    );
}

#[rstest::rstest]
fn confirm_file_entry_deactivates_popup() {
    // Given an active @ popup with a file selected at index 0.
    let mut state = at_popup_with_entries(vec![file_entry("img.png")]);

    // When confirming the selection.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then the popup is deactivated.
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "confirming a file should deactivate the popup"
    );
}

#[rstest::rstest]
fn confirm_inserts_filtered_entry_not_full_list() {
    // Given an active @ popup with [src, img.png] and a typed filter 's'.
    // 'src' starts with 's'; 'img.png' does not. The filter narrows the
    // visible rows to just [src], so index 0 is 'src'.
    let mut state =
        at_popup_with_filter_and_entries("s", vec![dir_entry("src"), file_entry("img.png")]);

    // When confirming the selection at index 0.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then 'src/' is inserted (the filtered entry), not 'img.png'.
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@src/",
        "confirm should insert the filtered-visible entry, not the full-list entry at that index"
    );
}

#[rstest::rstest]
fn confirm_with_no_filtered_entries_is_noop() {
    // Given an active @ popup where the typed filter matches nothing.
    let mut state = at_popup_with_filter_and_entries("zzz", vec![file_entry("src")]);

    // When confirming the selection.
    let _ = jinn_chat_input::intent::handle_autocomplete_confirm(&mut state);

    // Then nothing is inserted (the buffer stays as typed).
    assert_eq!(
        state.with_active_input(|i| i.text().to_owned(), String::new),
        "@zzz",
        "confirm with no visible entries should be a no-op"
    );
}

#[rstest::rstest]
fn arrow_down_moves_within_filtered_entries() {
    // Given an active @ popup with [src, srv, static] all matching filter 's'.
    let mut state = at_popup_with_filter_and_entries(
        "s",
        vec![dir_entry("src"), dir_entry("srv"), dir_entry("static")],
    );
    // Selection starts at index 0 (popup default for an empty-matches trigger).
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::autocomplete_selected_index,
            Default::default
        ),
        0,
        "selection should start at 0"
    );

    // When pressing arrow down.
    let _ = jinn_chat_input::intent::handle_move_cursor_down(&mut state);

    // Then the selection moves to index 1.
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::autocomplete_selected_index,
            Default::default
        ),
        1,
        "arrow down should move within the filtered entries"
    );
}

#[rstest::rstest]
fn arrow_down_clamps_at_filtered_entry_count() {
    // Given an active @ popup with [src, img.png] where only 'src' matches 's'.
    let mut state =
        at_popup_with_filter_and_entries("s", vec![dir_entry("src"), file_entry("img.png")]);

    // When pressing arrow down repeatedly.
    for _ in 0..5 {
        let _ = jinn_chat_input::intent::handle_move_cursor_down(&mut state);
    }

    // Then the selection clamps at the last filtered entry (index 0, since only
    // 'src' is visible).
    assert_eq!(
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::autocomplete_selected_index,
            Default::default
        ),
        0,
        "arrow down should clamp at the filtered entry count"
    );
}

#[rstest::rstest]
fn hash_trigger_valid_after_space() {
    // Given an input with "hello #".

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );

    // When typing "hello #" - the '#' is preceded by a space.
    let _ = jinn_chat_input::intent::handle_insert_char('h', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('e', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('l', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('l', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('o', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char(' ', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete activates (the || check passes with space).
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(ac.is_some(), "'#' after space should trigger autocomplete");
}

#[rstest::rstest]
fn hash_trigger_valid_after_newline() {
    // Given an input with "\n#".

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );

    // When typing "\n#" - the '#' is preceded by newline.
    let _ = jinn_chat_input::intent::handle_insert_char('\n', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete activates (the || check passes with newline).
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_some(),
        "'#' after newline should trigger autocomplete"
    );
}

#[rstest::rstest]
fn hash_trigger_invalid_after_letter() {
    // Given an input with "abc#".

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );

    // When typing "abc#" - the '#' is preceded by 'c' (not space or newline).
    let _ = jinn_chat_input::intent::handle_insert_char('a', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('b', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('c', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);

    // Then autocomplete does NOT activate.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_none(),
        "'#' after letter should NOT trigger autocomplete"
    );
}

#[rstest::rstest]
fn slash_trigger_only_at_position_zero() {
    // Given an input with text then "/".

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);

    // When typing "x/" - slash is NOT at position 0.
    let _ = jinn_chat_input::intent::handle_insert_char('x', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('/', &mut state);

    // Then autocomplete does NOT activate.
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_none(),
        "'/' not at position 0 should NOT trigger autocomplete"
    );
}

/// The chat input's active autocomplete, cloned out of the current session.
fn active_autocomplete(state: &AppState) -> Option<jinn_chat_input_msg::AutocompleteState> {
    state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default)
}

/// The chat input's active autocomplete filter, defaulted to empty.
fn autocomplete_filter(state: &AppState) -> String {
    state
        .active_session()
        .with_input(
            jinn_chat_input_msg::ChatInputBoxState::autocomplete_filter,
            Default::default,
        )
        .unwrap_or_default()
}

/// An Input-scoped state offering one `#test` prompt template, with "#t"
/// already typed so hash autocomplete is active with filter "t".
fn state_with_hash_autocomplete_on_test() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );
    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('t', &mut state);
    state
}

#[rstest::rstest]
fn typing_hash_and_t_activates_autocomplete_with_filter_t() {
    // Given an Input-scoped state offering a `#test` prompt template.
    let state = state_with_hash_autocomplete_on_test();

    // When reading the input's autocomplete state back.
    // (The fixture above already typed "#t" through the input handlers.)

    // Then autocomplete is active.
    assert!(active_autocomplete(&state).is_some());
    // And its filter is "t".
    assert_eq!(
        autocomplete_filter(&state),
        "t",
        "filter should be 't' before deletion"
    );
}

#[rstest::rstest]
fn delete_grapheme_deactivates_when_cursor_at_token_start_plus_one() {
    // Given "#t" with hash autocomplete active and filter "t".
    let mut state = state_with_hash_autocomplete_on_test();

    // When deleting back to "#" (cursor moves to position 1 = token_start + 1).
    let _ = jinn_chat_input::intent::handle_delete_grapheme(&mut state);

    // Then autocomplete reactivates with empty filter (the 't' was deleted).
    assert!(
        active_autocomplete(&state).is_some(),
        "autocomplete should reactivate after deleting back to #"
    );
    // And the filter it reactivates with is empty, not "t".
    assert_eq!(
        autocomplete_filter(&state),
        "",
        "filter should be empty after deleting the filter char"
    );
}

#[rstest::rstest]
fn delete_forward_deactivates_when_cursor_at_token_start() {
    // Given "#t" with cursor moved to position 0 (the '#' position).

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );

    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);
    let _ = jinn_chat_input::intent::handle_insert_char('t', &mut state);

    // Move cursor to position 0 (before the '#').
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::move_cursor_to_start);
    // Token start is 0, cursor is now 0.

    // When deleting forward from cursor position 0 (== token_start).
    let _ = jinn_chat_input::intent::handle_delete_grapheme_forward(&mut state);

    // Then autocomplete is deactivated (cursor == token_start triggers deactivation).
    let ac = state
        .active_session()
        .with_input(|i| i.autocomplete().clone(), Default::default);
    assert!(
        ac.is_none(),
        "delete forward at token_start should deactivate"
    );
}

/// An Input-scoped state with "a #test" typed, offering a `#test` template.
/// Position 0='a', 1=' ', 2='#', 3='t', 4='e'.
fn state_with_typed_a_hash_test() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );
    for ch in "a #test".chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }
    state
}

/// "a #test" with the cursor moved to start and then right to position 4
/// (within "#te|st"), where the popup is active — asserted by
/// [`cursor_inside_hash_token_reactivates_autocomplete`], which drives this
/// same fixture.
fn a_hash_test_with_cursor_in_token() -> jinn_kernel::AppState {
    let mut state = state_with_typed_a_hash_test();
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    state
}

#[rstest::rstest]
fn cursor_inside_hash_token_reactivates_autocomplete() {
    // Given "a #test" with the cursor at the 'a' position, BEFORE the '#'
    // token.
    let state = a_hash_test_with_cursor_in_token();

    // When reading the input's autocomplete state back.
    // (The fixture above already walked the cursor into the token.)

    // Then it is active (cursor at position 4 is within #test).
    assert!(
        active_autocomplete(&state).is_some(),
        "cursor at position 4 should reactivate (within #test)"
    );
}

#[rstest::rstest]
fn cursor_move_left_deactivates_when_cursor_before_token() {
    // Given "a #test" with autocomplete active and the cursor at position 4,
    // within the token.
    let mut state = a_hash_test_with_cursor_in_token();

    // When moving the cursor left four times to position 0 ('a'), which is
    // before the '#' token.
    for _ in 0..4 {
        let _ = jinn_chat_input::intent::handle_move_cursor_left(&mut state);
    }

    // Then autocomplete is deactivated (cursor at position 0, token_start at
    // 2): cursor 0 <= token_start 2 deactivates, and try_reactivate finds no
    // '#' at 0 (it's 'a'), so it stays deactivated.
    assert!(
        active_autocomplete(&state).is_none(),
        "cursor before token should deactivate permanently"
    );
}

/// An Input-scoped state with "#test" typed, offering a `#test` template.
fn state_with_typed_hash_test() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );
    for ch in "#test".chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }
    state
}

/// "#test" with the cursor walked to the start of the buffer, which
/// deactivates the popup.
fn hash_test_with_cursor_at_start() -> jinn_kernel::AppState {
    let mut state = state_with_typed_hash_test();
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);
    state
}

#[rstest::rstest]
fn hash_autocomplete_deactivates_when_cursor_moves_to_start() {
    // Given "#test" with autocomplete active.
    let mut state = state_with_typed_hash_test();

    // When moving the cursor to the start of the buffer.
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);

    // Then autocomplete is deactivated.
    assert!(active_autocomplete(&state).is_none());
}

#[rstest::rstest]
fn reactivating_hash_autocomplete_within_token() {
    // Given "#test" with autocomplete deactivated and the cursor at the start.
    let mut state = hash_test_with_cursor_at_start();

    // When moving the cursor right to position 2 (within "#te|st").
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);

    // Then autocomplete reactivates via try_reactivate_autocomplete /
    // find_hash_token_at_cursor.
    assert!(
        active_autocomplete(&state).is_some(),
        "cursor within #token should reactivate autocomplete"
    );
}

/// An Input-scoped state with "/help" typed.
fn state_with_typed_slash_help() -> jinn_kernel::AppState {
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    for ch in "/help".chars() {
        let _ = jinn_chat_input::intent::handle_insert_char(ch, &mut state);
    }
    state
}

/// "/help" with the cursor walked to the start of the buffer, which
/// deactivates the popup.
fn slash_help_with_cursor_at_start() -> jinn_kernel::AppState {
    let mut state = state_with_typed_slash_help();
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);
    state
}

#[rstest::rstest]
fn slash_autocomplete_is_active_for_a_typed_command() {
    // Given an Input-scoped state with "/help" typed.
    let state = state_with_typed_slash_help();

    // When reading the input's autocomplete state back.
    // (The fixture above already typed "/help" through the input handlers.)

    // Then autocomplete is active.
    assert!(active_autocomplete(&state).is_some());
}

#[rstest::rstest]
fn slash_autocomplete_deactivates_when_cursor_moves_to_start() {
    // Given "/help" with autocomplete active.
    let mut state = state_with_typed_slash_help();

    // When moving the cursor to the start of the buffer.
    let _ = jinn_chat_input::intent::handle_move_cursor_to_start(&mut state);

    // Then autocomplete is deactivated.
    assert!(active_autocomplete(&state).is_none());
}

#[rstest::rstest]
fn reactivating_slash_autocomplete_within_command() {
    // Given "/help" with autocomplete deactivated and the cursor at the start.
    let mut state = slash_help_with_cursor_at_start();

    // When moving the cursor right to position 2 (within "/he|lp").
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);
    let _ = jinn_chat_input::intent::handle_move_cursor_right(&mut state);

    // Then autocomplete reactivates.
    assert!(
        active_autocomplete(&state).is_some(),
        "cursor within /command should reactivate autocomplete"
    );
}

/// The element the chat-input slice contributes to the UI registry.
///
/// It exercises the chat_input element rendering to kill the `>` → `>=`
/// scroll-indicator regression: the element has to be registrable at all
/// before the boundary case can be rendered at the exact viewport fill.
#[rstest::rstest]
fn registering_chat_input_adds_a_ui_element_to_the_registry() {
    // Given an empty UI registry and an Input-scoped state.
    let mut registry = jinn_kernel::AppUiRegistry::new();
    let _state = AppState::default_with_scope_focus();

    // When the chat-input slice registers its element.
    jinn_chat_input::register(&mut registry);

    // Then the registry holds at least one element.
    assert!(registry.iter_mut().count() > 0);
}

#[rstest::rstest]
fn enter_normal_mode_dismisses_active_autocomplete_without_scope_change() {
    // Given a state in Input scope with hash autocomplete active.

    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Input);
    state.active_session_mut().set_discovered_prompt_templates(
        jinn_context::PromptTemplateStore::from_vec(vec![jinn_context::PromptTemplate {
            name: "test".to_owned(),
            description: "desc".to_owned(),
            body: "body".to_owned(),
        }]),
    );

    let _ = jinn_chat_input::intent::handle_insert_char('#', &mut state);
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_some()
    );

    // When handling EnterNormalMode.
    let result = jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then autocomplete is dismissed but scope stays Input (not Normal).
    assert!(
        state
            .active_session()
            .with_input(|i| i.autocomplete().clone(), Default::default)
            .is_none(),
        "enter_normal_mode should dismiss autocomplete"
    );
    assert_eq!(
        state.frontend.scope(),
        FocusScope::Input,
        "first ESC should stay in Input, not switch to Normal"
    );
    assert!(result.message_names.is_empty());
}

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use jinn_chat_input::autocomplete_render::render_autocomplete_popup;

/// Helper to create an `AppState` with autocomplete active.
///
/// Sets the input buffer to the given text, activates autocomplete at `token_start`,
/// and populates matches.
fn state_with_autocomplete(
    buffer_text: &str,
    token_start: usize,
    matches: Vec<AutocompleteMatch>,
) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.replace_all(buffer_text.to_owned()));
    // Position cursor after the buffer text.
    // Note: cursor must be at the end for autocomplete to be consistent.
    state.update_active_input(|i| {
        i.activate_autocomplete(token_start, AutocompleteTrigger::Hash, matches);
    });
    state
}

/// Extract a line of text from a buffer at the given row.
fn buffer_line(buf: &ratatui::buffer::Buffer, y: u16, start_x: u16, max_len: u16) -> String {
    let mut s = String::new();
    for x in start_x..start_x + max_len {
        let cell = buf.cell((x, y));
        let sym = cell.map_or(" ", ratatui::buffer::Cell::symbol);
        if sym == " " && s.ends_with("  ") {
            break;
        }
        s.push_str(sym);
    }
    s.trim_end().to_owned()
}

#[rstest::rstest]
fn render_autocomplete_popup_shows_matches() {
    // Given an AppState with autocomplete active and 3 matches.

    let matches = vec![
        AutocompleteMatch {
            name: "code-review".to_owned(),
            description: "Perform code review".to_owned(),
        },
        AutocompleteMatch {
            name: "summarize".to_owned(),
            description: "Summarize text".to_owned(),
        },
        AutocompleteMatch {
            name: "test-gen".to_owned(),
            description: "Generate tests".to_owned(),
        },
    ];
    let state = state_with_autocomplete("#co", 0, matches);

    let (mut terminal, _area) = setup_term(80, 24);

    // When rendering the autocomplete popup with a known input area.
    let input_area = Rect::new(0, 20, 80, 4);
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup shows all three matches.
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 5; // 3 matches + 2 border rows = 5, popup sits above input_area
    // Check that match names appear in the popup content.
    let line1 = buffer_line(&buffer, popup_top + 1, 1, 60);
    let line2 = buffer_line(&buffer, popup_top + 2, 1, 60);
    let line3 = buffer_line(&buffer, popup_top + 3, 1, 60);
    assert!(
        line1.contains("code-review"),
        "first match should contain 'code-review', got: {line1}"
    );
    assert!(
        line2.contains("summarize"),
        "second match should contain 'summarize', got: {line2}"
    );
    assert!(
        line3.contains("test-gen"),
        "third match should contain 'test-gen', got: {line3}"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_highlights_selected() {
    // Given an AppState with 2 matches and the second (most-relevant) selected.

    let matches = vec![
        AutocompleteMatch {
            name: "alpha".to_owned(),
            description: String::new(),
        },
        AutocompleteMatch {
            name: "beta".to_owned(),
            description: String::new(),
        },
    ];
    let mut state = state_with_autocomplete("#", 0, matches);
    // Default selected_index is last (index 1 = "beta").
    // Move selection up to select index 0 ("alpha").
    state.update_active_input(jinn_chat_input_msg::ChatInputBoxState::autocomplete_move_up);
    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the selected row has Modifier::REVERSED.
    // Popup: anchor_x = 0 + 2 (prompt_indent) + 0 (token_col) = 2.
    // Popup at (2, 16, 20, 4). Content starts at x=3.
    // First match (index 0, selected) at y=17, second at y=18.
    let buffer = terminal.backend().buffer().clone();
    let selected_cell = buffer.cell((3, 17)).expect("selected cell");
    assert!(
        selected_cell.modifier.contains(Modifier::REVERSED),
        "selected cell should have REVERSED modifier"
    );

    // Second match (index 1) is NOT selected.
    let unselected_cell = buffer.cell((3, 18)).expect("unselected cell");
    assert!(
        !unselected_cell.modifier.contains(Modifier::REVERSED),
        "unselected cell should NOT have REVERSED modifier"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_shows_no_matches_message() {
    // Given an AppState with autocomplete active but 0 matches.

    let state = state_with_autocomplete("#xyz", 0, vec![]);

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup shows "<no prompts found>".
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 3; // 1 content + 2 borders
    let line = buffer_line(&buffer, popup_top + 1, 1, 60);
    assert!(
        line.contains("<no prompts found>"),
        "should show no matches message, got: {line}"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_positioned_above_input() {
    // Given a known input area at row 20.

    let matches = vec![AutocompleteMatch {
        name: "test".to_owned(),
        description: "A test".to_owned(),
    }];
    let state = state_with_autocomplete("#", 0, matches);

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup's bottom edge touches input_area.y.
    // Popup: anchor_x = 0 + 2 + 0 = 2. Height = 1 + 2 = 3.
    // popup_y = 20 - 3 = 17. Bottom border at y = 19.
    let buffer = terminal.backend().buffer().clone();
    let border_cell = buffer.cell((2, 19)).expect("bottom border cell");
    assert_eq!(
        border_cell.fg,
        Color::DarkGray,
        "bottom border of popup should be at row 19, x=2 (popup anchor)"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_anchored_at_hash() {
    // Given a buffer "foo #co" - the # is at grapheme index 4.

    let matches = vec![AutocompleteMatch {
        name: "code".to_owned(),
        description: "Code stuff".to_owned(),
    }];
    let state = state_with_autocomplete("foo #co", 4, matches);

    let (mut terminal, _area) = setup_term(80, 24);
    // Input area starts at x=10 to see horizontal anchoring.
    let input_area = Rect::new(10, 20, 70, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup's left edge is near the # column.
    // # is at grapheme index 4 in the buffer, col 4 on the first line.
    // Input inner starts at x=10, prompt_indent=2, so anchor_x = 10 + 2 + 4 = 16.
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 3; // 1 match + 2 borders
    // Top-left corner of the popup should be at or near x=16.
    let corner_cell = buffer.cell((16, popup_top)).expect("popup corner");
    assert_eq!(
        corner_cell.fg,
        Color::DarkGray,
        "popup left border should be anchored at # column (x=16)"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_width_based_on_content() {
    // Given matches with varying name lengths.

    let matches = vec![
        AutocompleteMatch {
            name: "short".to_owned(),
            description: "s".to_owned(),
        },
        AutocompleteMatch {
            name: "a-very-long-template-name".to_owned(),
            description: "A very long description indeed".to_owned(),
        },
    ];
    let state = state_with_autocomplete("#", 0, matches);

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup width accommodates the longest line plus borders.
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 4; // 2 matches + 2 borders
    // The longest line: "a-very-long-template-name - A very long description indeed"
    // Check that the longer match text is visible in the buffer.
    let long_line = buffer_line(&buffer, popup_top + 2, 1, 60);
    assert!(
        long_line.contains("a-very-long-template-name"),
        "long name should be visible, got: {long_line}"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_does_not_render_when_inactive() {
    // Given an AppState with autocomplete inactive.

    let state = AppState::default_with_scope_focus();

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then no popup renders - the buffer should remain empty (default space chars).
    let buffer = terminal.backend().buffer().clone();
    // Check an area above the input where the popup would be.
    let cell = buffer.cell((0, 15)).expect("cell should exist");
    assert_eq!(
        cell.symbol(),
        " ",
        "no popup content should appear when autocomplete is inactive"
    );
}

#[rstest::rstest]
fn render_slash_command_popup_shows_commands() {
    // Given an AppState with slash autocomplete active.
    let matches = vec![AutocompleteMatch {
        name: "new".to_owned(),
        description: "Create a new session".to_owned(),
    }];
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.replace_all("/".to_owned()));
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Slash, matches));

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup shows the slash command.
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 3; // 1 match + 2 borders
    let line = buffer_line(&buffer, popup_top + 1, 1, 60);
    assert!(
        line.contains("new"),
        "should show 'new' command, got: {line}"
    );
    assert!(
        line.contains("Create a new session"),
        "should show description, got: {line}"
    );
}

#[rstest::rstest]
fn render_slash_command_popup_shows_no_commands_message() {
    // Given an AppState with slash autocomplete active but 0 matches.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.replace_all("/xyz".to_owned()));
    state.update_active_input(|i| i.activate_autocomplete(0, AutocompleteTrigger::Slash, vec![]));

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // When rendering the popup for that input area.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup shows "<no commands found>".
    let buffer = terminal.backend().buffer().clone();
    let popup_top = 20 - 3; // 1 content + 2 borders
    let line = buffer_line(&buffer, popup_top + 1, 1, 60);
    assert!(
        line.contains("<no commands found>"),
        "should show no commands message, got: {line}"
    );
}

#[rstest::rstest]
fn render_autocomplete_popup_clears_background() {
    // Given a terminal with pre-existing content behind the popup area.
    let matches = vec![AutocompleteMatch {
        name: "short".to_owned(),
        description: String::new(),
    }];
    let state = state_with_autocomplete("#", 0, matches);

    let (mut terminal, _area) = setup_term(80, 24);
    let input_area = Rect::new(0, 20, 80, 4);

    // Pre-fill the popup area with visible content to simulate bleed-through.
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 80, 20);
            for y in 0..area.height {
                let row = Rect::new(area.x, area.y + y, area.width, 1);
                frame.render_widget(ratatui::widgets::Paragraph::new("X".repeat(80)), row);
            }
        })
        .unwrap();

    // When rendering the autocomplete popup on top.
    terminal
        .draw(|frame| {
            render_autocomplete_popup(frame, input_area, &state);
        })
        .unwrap();

    // Then the popup area background is cleared - cells outside the content
    // text but inside the popup should not contain the filler 'X' characters.
    let buffer = terminal.backend().buffer().clone();
    // Popup: anchor_x = 0 + 2 + 0 = 2. 1 match => height = 3.
    // popup_y = 20 - 3 = 17. Popup area is (2, 17, width, 3).
    // The right side of the inner area (past the match text) should be spaces, not X.
    let inner_x = 3; // past left border
    let cell_past_content = buffer.cell((inner_x + 10, 18)).expect("cell past content");
    assert_eq!(
        cell_past_content.symbol(),
        " ",
        "cells past popup content should be cleared, got: '{}'",
        cell_past_content.symbol()
    );
}

use std::path::PathBuf;

use jinn_chat_input_msg::ListDirectory;
use jinn_core_types::SessionId;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::app_paths::AppPaths;
use jinn_kernel::common::services::test_services::TestServices;
use jinn_kernel::common::state::State;
use jinn_testutil::bus_harness::TestHarness;

use jinn_chat_input::directory_lister_actor::{DirectoryListerActor, DirectoryListerActorDeps};

// ── DirectoryListerActor (spawned harness) ─────────────────────────────────

async fn create_harness() -> (TestHarness, State, ActorDeps) {
    let harness = TestHarness::new().await;
    // `default_with_scope_focus`, not `default`: the `@path` popup's state
    // is a cell, and a bare `default` attaches no slice registry, so the
    // actor's write would reach nothing and the popup would stay empty
    // while every assertion around it stayed green.
    let state = State::new(AppState::default_with_scope_focus());
    let mut services = TestServices::builder()
        .paths(AppPaths::new_in(std::path::Path::new("")))
        .build();
    services.bus = harness.bus();
    services.trouper_system = harness.system().clone();
    let deps = ActorDeps { services };
    (harness, state, deps)
}

fn make_temp_dir(entries: &[(&str, bool)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jinn-file-lister-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for (name, is_dir) in entries {
        let path = dir.join(name);
        if *is_dir {
            std::fs::create_dir_all(&path).expect("create subdir");
        } else {
            std::fs::write(&path, b"x").expect("create file");
        }
    }
    dir
}

async fn wait_for_list_complete(state: &State) {
    // The actor clears `loading` when it finishes (success or error).
    // Poll until that happens, with a timeout.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while state
        .read()
        .frontend
        .with_file_picker(|picker| picker.loading)
        .unwrap_or(false)
    {
        assert!(
            std::time::Instant::now() <= deadline,
            "timed out waiting for DirectoryListerActor to finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[expect(clippy::unused_async, reason = "async for test-helper symmetry")]
async fn spawn_actor(deps: &ActorDeps, state: &State) -> trouper::actor::ActorPath {
    // The path is a placeholder; the actor self-subscribes to the domain
    // topic at its static path. Tests drive it via bus publishes only.
    DirectoryListerActor::spawn(
        &deps.services.trouper_system,
        DirectoryListerActorDeps {
            deps: deps.clone(),
            state: state.clone(),
        },
    )
}

#[rstest::rstest]
#[tokio::test]
async fn actor_reads_directory_entries_into_file_picker() {
    // Given a temp dir with a file and a subdirectory.
    let dir = make_temp_dir(&[("alpha.txt", false), ("subdir", true)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;

    // Set the expected request id and mark loading.
    state.with_file_picker(|picker| {
        picker.expected_request_id = 1;
        picker.loading = true;
    });

    // When the actor lists the directory.
    harness
        .publish(ListDirectory {
            session_id: SessionId::new(),
            path: dir.clone(),
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the file picker is populated and loading is cleared.
    let entries = file_picker_entries(&state.read());
    let loading = state
        .read()
        .frontend
        .with_file_picker(|picker| picker.loading)
        .unwrap_or(false);
    assert!(
        !loading,
        "loading should be cleared after a successful read"
    );
    assert!(
        entries.iter().any(|e| e.name == "alpha.txt" && !e.is_dir),
        "file entry should be present: {entries:?}"
    );
    assert!(
        entries.iter().any(|e| e.name == "subdir" && e.is_dir),
        "dir entry should be present: {entries:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn actor_drops_stale_reply_when_request_id_mismatches() {
    // Given a temp dir with one file.
    let dir = make_temp_dir(&[("stale.txt", false)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;

    // The expected id is 5, but we send a request with id 1 (stale).
    state.with_file_picker(|picker| {
        picker.expected_request_id = 5;
        picker.loading = true;
    });

    // When the actor processes a request whose id does not match.
    harness
        .publish(ListDirectory {
            session_id: SessionId::new(),
            path: dir,
            request_id: 1, // stale
        })
        .await;

    // The actor processes the stale request but does NOT clear loading.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Then the stale reply is dropped: entries stay empty, loading unchanged.
    let entries = file_picker_entries(&state.read());
    let loading = state
        .read()
        .frontend
        .with_file_picker(|picker| picker.loading)
        .unwrap_or(false);
    assert!(entries.is_empty(), "stale reply must not populate entries");
    assert!(loading, "stale reply must not clear loading");
}

#[rstest::rstest]
#[tokio::test]
async fn actor_returns_empty_for_nonexistent_directory() {
    // Given an actor and a path that does not exist.
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;
    state.with_file_picker(|picker| {
        picker.expected_request_id = 1;
        picker.loading = true;
    });
    let bogus = PathBuf::from("/this/path/does/not/exist/jinn-test");

    // When the actor lists the nonexistent directory.
    harness
        .publish(ListDirectory {
            session_id: SessionId::new(),
            path: bogus,
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the entries are empty (not an error), loading cleared.
    let entries = file_picker_entries(&state.read());
    let loading = state
        .read()
        .frontend
        .with_file_picker(|picker| picker.loading)
        .unwrap_or(false);
    assert!(entries.is_empty(), "nonexistent dir yields empty entries");
    assert!(!loading, "loading should be cleared even on read error");
}

#[rstest::rstest]
#[tokio::test]
async fn actor_lists_hidden_files() {
    // Given a temp dir with a dotfile and a regular file.
    let dir = make_temp_dir(&[(".hidden", false), ("visible.txt", false)]);
    let (harness, state, deps) = create_harness().await;
    let _actor = spawn_actor(&deps, &state).await;
    state.with_file_picker(|picker| {
        picker.expected_request_id = 1;
        picker.loading = true;
    });

    // When the actor lists the directory.
    harness
        .publish(ListDirectory {
            session_id: SessionId::new(),
            path: dir,
            request_id: 1,
        })
        .await;

    wait_for_list_complete(&state).await;

    // Then the dotfile appears in the listing (hidden files shown).
    let entries = file_picker_entries(&state.read());
    assert!(
        entries.iter().any(|e| e.name == ".hidden"),
        "hidden files should be listed: {entries:?}"
    );
}

/// Leaving insert mode abandons a stashed session creation.
///
/// The stash comes from a project-picker confirm, midway through the
/// lifecycle/args chain. Escaping out of insert mode must drop it, or it
/// leaks into a later `n`/`N`.
#[rstest::rstest]
fn enter_normal_mode_clears_pending_session_creation() {
    // Given a state with a pending session creation stashed from a
    // project-picker confirm (midway through the lifecycle/args chain).
    let mut state = AppState::default_with_scope_focus();
    let active_cwd = state.active_session().cwd().to_path_buf();
    state.frontend.pending_creation = Some(PendingSessionCreation {
        project_dir: std::path::PathBuf::from("/tmp/project-a"),
        starting_cwd: std::path::PathBuf::from("/tmp/project-a"),
    });

    // When abandoning the chain via ESC (enter normal mode).
    jinn_chat_input::intent::handle_enter_normal_mode(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the stash is cleared so it never leaks into a future `n`/`N`.
    assert!(state.frontend.pending_creation.is_none());
    // And the active session's CWD is unchanged (no side-channel mutation).
    assert_eq!(state.active_session().cwd(), active_cwd);
}

/// Pasting while the box is focused inserts the text verbatim.
#[rstest::rstest]
fn paste_text_inserts_into_the_buffer() {
    // Given an AppState with the chat input focused.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(jinn_slices::FocusScope::Input);

    // When pasting multi-line text.
    jinn_chat_input::intent::handle_paste_text("hello\nworld", &mut state);

    // Then the buffer has the pasted text verbatim.
    assert_eq!(
        state
            .active_session()
            .with_input(|i| i.text().to_owned(), String::new),
        "hello\nworld"
    );
}

/// Composition's `register` puts the box in the registry under the name
/// the renderer fetches it by.
///
/// The renderer looks the box up with `if let Some(..)`, so a missing
/// `register` call does not panic — the box silently stops drawing. This
/// pins the one link that catches that.
#[rstest::rstest]
fn register_puts_the_box_in_the_ui_registry() {
    // Given an empty UI registry.
    let mut registry = UiRegistry::new();

    // When composition registers the slice's element.
    jinn_chat_input::register(&mut registry);

    // Then the box is fetchable under the name the renderer uses.
    assert!(
        registry.get_mut("chat-input-box").is_some(),
        "the renderer fetches the box by this name; a missing register means it never draws",
    );
}

#[rstest::rstest]
fn seed_mode_submission_pins_and_does_not_dispatch() {
    // Given a seed-mode attendant as the active session.
    let mut state = AppState::default_with_scope_focus();
    {
        let session = state.active_session_mut();
        let parent = jinn_session_state::ChatSessionState::new();
        *session = jinn_session_state::ChatSessionState::new_attendant(&parent, true);
        // Attendant defaults to Seed activation.
    }
    state.update_active_input(|i| i.insert_text("judge this repo"));

    // When the message is submitted.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the entry is pushed already pinned, persisted — and nothing
    // dispatches. The pin rides on the push rather than following it as a
    // separate `PinChatEntry`, because two messages race and a pin that
    // arrives first finds no entry and is dropped.
    let names = &result.message_names;
    assert!(
        names.iter().any(|n| n.contains("PushChatEntry")),
        "expected PushChatEntry, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("PinChatEntry")),
        "the pin must travel with the push, not as a racing second message; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("PersistSession")),
        "expected PersistSession, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("EnqueueUserMessage")),
        "seed mode must not dispatch, got {names:?}"
    );
}

#[rstest::rstest]
fn seed_mode_submission_leaves_normal_sessions_dispatching() {
    // Given a plain user session with text in the buffer.
    let mut state = AppState::default_with_scope_focus();
    state.update_active_input(|i| i.insert_text("hello"));

    // When the message is submitted.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the normal enqueue path runs — no pin, no push.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage"))
    );
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("PinChatEntry"))
    );
}

#[rstest::rstest]
fn a_composed_attendant_submissions_dispatch_normally() {
    // Given a composed attendant — out of prep mode, so it is runnable — as
    // the active session.
    let mut state = AppState::default_with_scope_focus();
    {
        let session = state.active_session_mut();
        let parent = jinn_session_state::ChatSessionState::new();
        let mut attendant = jinn_session_state::ChatSessionState::new_attendant(&parent, true);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_attendant_is_prepping(false);
        *session = attendant;
    }
    state.update_active_input(|i| i.insert_text("go"));

    // When the message is submitted.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the normal enqueue path runs — only prep mode pins without
    // dispatching.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage"))
    );
}

/// A submission from a session that has been running under automation emits
/// the interaction mark and the enqueue — nothing else.
///
/// The enqueue branch used to also emit a message clearing a per-session
/// automation marker, so a submission from a session that had already been
/// dispatched put a second command on the bus ahead of its own enqueue.
#[rstest::rstest]
fn a_submission_emits_exactly_the_interaction_mark_and_the_enqueue() {
    // Given a composed attendant — out of prep mode, so it is runnable — as
    // the active session.
    let mut state = AppState::default_with_scope_focus();
    {
        let session = state.active_session_mut();
        let parent = jinn_session_state::ChatSessionState::new();
        let mut attendant = jinn_session_state::ChatSessionState::new_attendant(&parent, true);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_attendant_is_prepping(false);
        *session = attendant;
    }
    state.update_active_input(|i| i.insert_text("go"));

    // When the message is submitted.
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut state,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );

    // Then the submission is exactly those two messages.
    assert_eq!(
        result.message_names.len(),
        2,
        "got {:?}",
        result.message_names
    );
    assert!(result.message_names[0].contains("MarkSessionInteracted"));
    assert!(result.message_names[1].contains("EnqueueUserMessage"));
}

/// A seed-mode attendant's submission must leave a PINNED entry in the
/// session's own history.
///
/// The intent emits `PushChatEntry` then `PinChatEntry` in that order, but
/// each message is published through its own spawned task, so the order the
/// user wrote in is not the order the session actor sees. When the pin wins
/// that race it finds no entry to attach to and is dropped. A current-thread
/// runtime hides this by running the spawns in issue order, so this test
/// drives the real bridge on a multi-thread runtime.
#[rstest::rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seed_mode_submission_leaves_the_entry_pinned_in_the_session() {
    // Given a seed-mode attendant as the active session, with the
    // session-turn actor live on that same state so the push lands.
    let (harness, state, deps) = create_harness().await;
    let session_id = {
        let mut guard = state.write();
        let parent = jinn_session_state::ChatSessionState::new();
        let attendant = jinn_session_state::ChatSessionState::new_attendant(&parent, true);
        // Inserted under its own id and made active: writing through
        // `active_session_mut` would leave the map keyed by the old id
        // while the session carried a new one.
        let id = attendant.session_id().clone();
        guard.session.insert(attendant);
        guard.session.set_active(id.clone());
        id
    };
    // The context-assembly service answers the queue actor's assemble ask;
    // the handle is dropped here, but the service stays spawned.
    let system = deps.services.trouper_system.clone();
    drop(jinn_context_assembly::service::ensure_spawned(&system));
    jinn_session_turn::activate(
        &system,
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps,
            state: state.clone(),
            counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::system(),
        },
    );
    state
        .write()
        .update_active_input(|i| i.insert_text("judge this repo"));

    // When the message is submitted and the emitted messages are published
    // the way the bridge publishes them.
    let mut guard = state.write();
    let result = jinn_chat_input::intent::handle_submit_message(
        &mut guard,
        jinn_kernel::common::render_ctx::empty_config_layer(),
    );
    drop(guard);
    for closure in result.messages {
        closure(&harness.bus());
    }
    // Given the session actor a moment to drain the two messages.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    // Then the entry is in the attendant's history, pinned. Unpinned, it
    // would be dropped from the very context the mode exists to preserve.
    let guard = state.read();
    let session = guard.session.get(&session_id).expect("attendant");
    let entry = session
        .history()
        .iter()
        .find(|e| e.text() == "judge this repo")
        .expect("the submission is in the attendant's history");
    assert!(
        entry.is_pinned(),
        "a seed-mode submission must be pinned into the attendant's context"
    );
}
