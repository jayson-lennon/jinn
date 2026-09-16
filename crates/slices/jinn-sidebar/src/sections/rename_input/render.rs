//! Rename session input popup rendering - a centered overlay for renaming a session.

use jinn_domain::common::render_ctx::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

/// Horizontal padding fraction for the popup (20% each side).
const POPUP_H_PAD_FRAC: f32 = 0.20;
/// Minimum popup width in cells.
const POPUP_MIN_WIDTH: u16 = 30;

/// Computes the popup rectangle for the rename session input overlay.
pub fn rename_session_popup_rect(area: Rect) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(POPUP_MIN_WIDTH)
        .min(area.width);

    let popup_height = 3u16.min(area.height); // border(2) + 1 input line

    // Integer division is intentional - we're computing cell positions for centering.
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3;

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// Renders the rename session input popup.
///
/// Shows a centered popup with:
/// - Title: "Rename Session"
/// - Input line with cursor showing the current value
pub fn render_rename_session_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let input_text = state
        .frontend
        .with_sections(|s| s.rename_input.text.input.clone(), String::new);
    let input_cursor = state
        .frontend
        .with_sections(|s| s.rename_input.text.cursor_pos, || 0);
    let theme = &state.frontend.theme;
    let popup_area = rename_session_popup_rect(area);

    let title = Line::from(Span::styled(
        " Rename Session ",
        Style::default().fg(theme.popup_title),
    ));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));

    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    // Inner area (1 padding on each side from border).
    let inner = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: popup_area.width.saturating_sub(2),
        height: popup_area.height.saturating_sub(2),
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Input line: "> {input}" - the ">" uses focus_accent for consistency.
    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let input_span = Span::raw(input_text.as_str());
    let input_line = Line::from(vec![prefix, input_span]);
    let input_para = Paragraph::new(input_line);
    frame.render_widget(input_para, Rect::new(inner.x, inner.y, inner.width, 1));

    // Compute cursor x position: "> " (2) + grapheme count up to cursor_pos.
    let prefix_len = 2u16;
    let grapheme_count = input_text
        .get(..input_cursor)
        .map_or(0, |s| s.graphemes(true).count());
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));
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
    use super::*;
    use jinn_domain::common::app_state::{AppState, FocusScope, RenameSessionInputState};
    use jinn_testutil::setup_term;

    #[rstest::rstest]
    fn rename_popup_shows_title() {
        // Given a state in RenameSessionInput scope with input.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_domain::common::line_input::LineInput {
                    input: "My Session".to_owned(),
                    cursor_pos: 10,
                },
            };
        });
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_rename_session_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the popup title appears in the top border.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = rename_session_popup_rect(area);
        let title_line_y = popup_area.y;

        let title_text = " Rename Session ";
        let mut found_title = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, title_line_y)) {
                let cell_text: &str = cell.symbol();
                if matches!(cell_text, "┌" | "─" | "┐") {
                    continue;
                }
                if title_text.contains(cell_text) {
                    found_title = true;
                    break;
                }
            }
        }
        assert!(found_title, "title should appear in the top border");
    }

    #[rstest::rstest]
    fn rename_popup_shows_input_text() {
        // Given a state with input "Hello World".
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_domain::common::line_input::LineInput {
                    input: "Hello World".to_owned(),
                    cursor_pos: 11,
                },
            };
        });
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_rename_session_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the input line shows "> Hello World".
        let buffer = terminal.backend().buffer().clone();
        let popup_area = rename_session_popup_rect(area);
        let inner_y = popup_area.y + 1;
        let inner_x = popup_area.x + 1;

        let row_text: String = (inner_x..inner_x + 20)
            .filter_map(|x| buffer.cell((x, inner_y)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            row_text.starts_with("> Hello World"),
            "expected '> Hello World' on input line, got: {row_text}"
        );
    }

    #[rstest::rstest]
    fn rename_popup_prefix_uses_focus_accent_color() {
        // Given a state with input "Test".
        let mut state = AppState::default_with_scope_focus();
        let expected_color = state.frontend.theme.focus_accent;
        state.frontend.scope_push(FocusScope::RenameSessionInput);
        state.frontend.update_sections(|s| {
            s.rename_input = RenameSessionInputState {
                text: jinn_domain::common::line_input::LineInput {
                    input: "Test".to_owned(),
                    cursor_pos: 4,
                },
            };
        });
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_rename_session_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the ">" prefix uses focus_accent color.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = rename_session_popup_rect(area);
        let inner_x = popup_area.x + 1;
        let inner_y = popup_area.y + 1;

        let gt_cell = buffer.cell((inner_x, inner_y)).expect("> cell exists");
        assert_eq!(gt_cell.symbol(), ">");
        assert_eq!(
            gt_cell.style().fg,
            Some(expected_color),
            "> prefix should use focus_accent color"
        );
    }

    #[rstest::rstest]
    fn rename_popup_rect_is_centered() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);

        // When computing popup rect.
        let popup = rename_session_popup_rect(area);

        // Then the popup is centered horizontally.
        let expected_width = 48u16; // 80 * 0.6 = 48
        assert_eq!(popup.width, expected_width);
        // And the popup has 3 rows (border + 1 content).
        assert_eq!(popup.height, 3);
    }
}
