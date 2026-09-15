//! Pruner accumulation threshold input popup rendering — a centered overlay.

use crate::common::render_ctx::RenderCtx;
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

/// Computes the popup rectangle for the pruner accumulation input overlay.
pub fn pruner_accumulation_popup_rect(area: Rect) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(POPUP_MIN_WIDTH)
        .min(area.width);

    let popup_height = 3u16.min(area.height); // border(2) + 1 input line

    // Integer division is intentional — we're computing cell positions for centering.
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3;

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// Renders the pruner accumulation threshold input popup.
///
/// Shows a centered popup with:
/// - Title: "Pruner Accumulation Threshold"
/// - Input line with cursor showing the current value
pub fn render_pruner_accumulation_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let input_state = &state.frontend.pruner_accumulation_input;
    let theme = &state.frontend.theme;
    let popup_area = pruner_accumulation_popup_rect(area);

    let title = Line::from(Span::styled(
        " Pruner Accumulation Threshold ",
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

    // Input line: "> {input}" — the ">" uses focus_accent for consistency.
    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let input_span = Span::raw(&input_state.text.input);
    let input_line = Line::from(vec![prefix, input_span]);
    let input_para = Paragraph::new(input_line);
    frame.render_widget(input_para, Rect::new(inner.x, inner.y, inner.width, 1));

    // Compute cursor x position: "> " (2) + grapheme count up to cursor_pos.
    let prefix_len = 2u16;
    let grapheme_count = input_state
        .text
        .input
        .get(..input_state.text.cursor_pos)
        .map_or(0, |s| s.graphemes(true).count());
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope};
    use crate::common::line_input::LineInput;
    use crate::common::render_ctx::RenderCtx;
    use crate::feat::pruner_accumulation_input::state::PrunerAccumulationInputState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[rstest::rstest]
    #[test]
    fn render_shows_title_and_seeded_value() {
        // Given the popup scope active with a seeded threshold of 10000.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(FocusScope::PrunerAccumulationInput);
        state.frontend.pruner_accumulation_input = PrunerAccumulationInputState {
            text: LineInput {
                input: "10000".to_owned(),
                cursor_pos: 5,
            },
        };

        // When rendering the popup.
        let area = Rect::new(0, 0, 80, 24);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_pruner_accumulation_input(frame, area, &ctx);
            })
            .expect("draw");

        // Then the title and seeded value are present in the rendered buffer.
        let buffer = terminal.backend().buffer();
        let rendered: String = buffer
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            rendered.contains("Pruner Accumulation Threshold"),
            "popup title should be rendered"
        );
        assert!(
            rendered.contains("10000"),
            "seeded value should be rendered"
        );
    }
}
