//! Assistant entry rendering - markdown-rendered text.

use ratatui::text::Line;

use super::markdown::render_markdown;
use super::shared::{Pad, RenderContext, pad_entry};

pub fn to_lines(text: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = super::shared::strip_ansi(text);
    let mut lines = render_markdown(&text, ctx.content_width, &ctx.theme);
    pad_entry(&mut lines, Pad::Both);
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
    fn assistant_to_lines_has_exactly_one_pad_above_and_below() {
        // Given assistant text with leading newlines (as reasoning models produce).
        let ctx = render_context();

        // When converting to lines.
        let lines = super::to_lines("\n\nHello! How can I help you today?", &ctx);

        // Then there are exactly 3 lines: pad + content + pad.
        assert_eq!(
            lines.len(),
            3,
            "assistant entry should have exactly 3 lines (pad + content + pad), got {}",
            lines.len()
        );
        // And the first line is the top pad.
        assert_eq!(lines[0].spans, [] as [ratatui::prelude::Span<'_>; 0]);
        // And the last line is the bottom pad.
        assert_eq!(lines[2].spans, [] as [ratatui::prelude::Span<'_>; 0]);
    }

    #[rstest::rstest]
    fn assistant_plain_text_no_extra_blank_lines() {
        // Given plain assistant text without any surrounding newlines.
        let ctx = render_context();

        // When converting to lines.
        let lines = super::to_lines("Hello!", &ctx);

        // Then there are exactly 3 lines: pad + content + pad.
        assert_eq!(
            lines.len(),
            3,
            "plain assistant should have 3 lines (pad + content + pad), got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn empty_assistant_text_produces_zero_lines() {
        // Given empty assistant text.
        let ctx = render_context();

        // When converting to lines.
        let lines = super::to_lines("", &ctx);

        // Then zero lines are produced (no padding, no content).
        assert!(
            lines.is_empty(),
            "empty assistant should produce zero lines, got {}",
            lines.len()
        );
    }
}
