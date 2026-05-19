//! Token budget input popup rendering — a centered overlay for adjusting the budget.

use crate::common::app_state::AppState;
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

/// Computes the popup rectangle for the token budget input overlay.
pub fn token_budget_popup_rect(area: Rect) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(POPUP_MIN_WIDTH)
        .min(area.width);

    let popup_height = 4u16.min(area.height); // border(2) + input line(1) + error line(1)

    // Integer division is intentional — we're computing cell positions for centering.
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3;

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// Renders the token budget input popup.
///
/// Shows a centered popup with:
/// - Title: "Token Budget"
/// - Input line with cursor showing the current value
pub fn render_token_budget_input(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let input_state = &state.frontend.token_budget_input;
    let theme = &state.frontend.theme;

    let popup_area = token_budget_popup_rect(area);

    let title = Line::from(Span::styled(
        " Token Budget ",
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

    // Input line: "> {input}"
    let input_line = Line::from(Span::raw(format!("> {}", input_state.input)));
    let input_para = Paragraph::new(input_line);
    frame.render_widget(input_para, Rect::new(inner.x, inner.y, inner.width, 1));

    // Error line (row below input, when present).
    if let Some(ref err) = input_state.error_message {
        let error_line = Line::from(Span::styled(
            err.clone(),
            Style::default().fg(theme.error_text),
        ));
        frame.render_widget(
            Paragraph::new(error_line),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }

    // Compute cursor x position: "> " (2) + grapheme count up to cursor_pos.
    let prefix_len = 2u16;
    let grapheme_count = input_state.input[..input_state.cursor_pos]
        .graphemes(true)
        .count();
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope, TokenBudgetInputState};
    use nullslop_testutil::setup_term;

    #[rstest::rstest]
    fn token_budget_popup_shows_title() {
        // Given a state in TokenBudgetInput scope with input.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "150000".to_owned(),
            cursor_pos: 6,
            error_message: None,
        };
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                render_token_budget_input(frame, area, &state);
            })
            .unwrap();

        // Then the popup title appears in the top border.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = token_budget_popup_rect(area);
        let title_line_y = popup_area.y;

        let title_text = " Token Budget ";
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
    fn token_budget_popup_shows_input_text() {
        // Given a state with input "200000".
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(FocusScope::TokenBudgetInput);
        state.frontend.token_budget_input = TokenBudgetInputState {
            input: "200000".to_owned(),
            cursor_pos: 6,
            error_message: None,
        };
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                render_token_budget_input(frame, area, &state);
            })
            .unwrap();

        // Then the input line shows "> 200000".
        let buffer = terminal.backend().buffer().clone();
        let popup_area = token_budget_popup_rect(area);
        let inner_y = popup_area.y + 1;
        let inner_x = popup_area.x + 1;

        let row_text: String = (inner_x..inner_x + 20)
            .filter_map(|x| buffer.cell((x, inner_y)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(
            row_text.starts_with("> 200000"),
            "expected '> 200000' on input line, got: {row_text}"
        );
    }

    #[rstest::rstest]
    fn token_budget_popup_clears_background() {
        // Given a filled background.
        let state = AppState::default();
        let (mut terminal, area) = setup_term(80, 24);

        // First fill with Xs.
        terminal
            .draw(|frame| {
                let fill = Paragraph::new(Line::from(Span::raw("XXXXX")));
                frame.render_widget(fill, Rect::new(0, 0, 80, 24));
            })
            .unwrap();

        // Then draw the popup.
        terminal
            .draw(|frame| {
                render_token_budget_input(frame, area, &state);
            })
            .unwrap();

        // Then the background inside the popup is cleared.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = token_budget_popup_rect(area);
        let inner_center = (
            popup_area.x + popup_area.width / 2,
            popup_area.y + popup_area.height / 2,
        );
        if let Some(cell) = buffer.cell(inner_center) {
            assert_ne!(
                cell.symbol(),
                "X",
                "background should be cleared inside popup"
            );
        }
    }

    #[rstest::rstest]
    fn token_budget_popup_rect_is_centered() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);

        // When computing popup rect.
        let popup = token_budget_popup_rect(area);

        // Then the popup is centered horizontally.
        let expected_width = 48u16; // 80 * 0.6 = 48
        assert_eq!(popup.width, expected_width);
        // And the popup has 4 rows (border + input + error line).
        assert_eq!(popup.height, 4);
    }
}
