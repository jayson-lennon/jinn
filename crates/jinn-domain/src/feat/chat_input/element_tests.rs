#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::common::app_state::AppState;
use crate::common::app_state::FocusScope;
use crate::common::render_ctx::RenderCtx;
use crate::common::ui_element::UiElement;
use crate::feat::chat_input::element::ChatInputBoxElement;
use crate::feat::session::queue_item::QueueItem;
use crate::feat::theme::default_theme;
use crate::protocol::ChatEntry;

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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_to_start);
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_right); // cursor at 1 (between 'a' and 'b')
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_to_start);
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then line 0 (row 0) has "> " prefix and "hello".
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then line 1 (row 1) has "  " indent and "world".
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_left); // now at pos 3, which is after the second \n, before 'b'
        // Actually: "a\n\nb" → graphemes: a(0) \n(1) \n(2) b(3). cursor at 3 = before 'b'.
        // Move left once more to be at pos 2 = after first \n, on empty line.
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_left);
        s
    };

    let (mut terminal, area) = setup_term(40, 5);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::move_cursor_left);
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::toggle_input_mode); // Steer → Queue
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::toggle_input_mode); // Steer -> Queue
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::toggle_input_mode); // Steer → Queue
        s.frontend.scope_pop(); // pop Input → back to Normal
        s
    };

    let (mut terminal, area) = setup_term(40, 3);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
        s.update_active_input(crate::feat::chat_input::ChatInputBoxState::toggle_input_mode); // Steer → Queue
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
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
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
