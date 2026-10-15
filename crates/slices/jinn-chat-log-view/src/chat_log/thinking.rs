//! Thinking entry rendering - dark gray.

use ratatui::style::Style;
use ratatui::text::Line;

use super::shared::{Pad, RenderContext, multiline_styled, pad_entry};

pub fn to_lines(text: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let text = super::shared::strip_ansi(text);
    let mut lines = multiline_styled(&text, "", "", Style::default().fg(ctx.theme.muted_text));
    pad_entry(&mut lines, Pad::Top);
    lines
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use crate::chat_log::shared::RenderContext;

    fn render_context() -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 5,
            theme: jinn_theme::default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    #[rstest::rstest]
    fn thinking_to_lines_has_top_pad_only() {
        // Given thinking text with a trailing newline (as reasoning models produce).
        let ctx = render_context();

        // When converting to lines.
        let lines = super::to_lines("The user is greeting me.\n", &ctx);

        // Then there are exactly 2 lines: pad + content (no bottom pad).
        assert_eq!(
            lines.len(),
            2,
            "thinking entry should have exactly 2 lines (pad + content), got {}",
            lines.len()
        );
        // And the first line is the top pad.
        assert_eq!(lines[0].spans, [] as [ratatui::prelude::Span<'_>; 0]);
        // And the last line has content (not a bottom pad).
        assert_ne!(lines[1].spans, [] as [ratatui::prelude::Span<'_>; 0]);
    }

    #[rstest::rstest]
    fn thinking_multiline_trailing_newline_no_extra_blank() {
        // Given multi-line thinking text with trailing newline.
        let ctx = render_context();

        // When converting to lines.
        let lines = super::to_lines("line one\nline two\n", &ctx);

        // Then there are exactly 3 lines: pad + 2 content (no bottom pad).
        assert_eq!(
            lines.len(),
            3,
            "multi-line thinking should have 3 lines (pad + 2 content), got {}",
            lines.len()
        );
    }
}
