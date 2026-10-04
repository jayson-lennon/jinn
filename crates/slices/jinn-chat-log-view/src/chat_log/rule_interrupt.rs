//! Rule interrupt entry rendering — guidance the harness injected mid-turn.
//!
//! Renders a rule interrupt as a full-width block on `rule_interrupt_bg`.
//! The block is deliberately distinct from the user-message block: an interrupt
//! is not something the user typed, and it must not read as though it were.
//!
//! The header names the rule that fired, and the body is the same guidance the
//! model receives, markdown-rendered like `user.rs` renders a typed message —
//! rule bodies are user-authored markdown, and escaping them would change how a
//! rule reads.
//!
//! The body is always shown: an interrupt is the reason the turn changed
//! course, so hiding it behind a toggle would defeat it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markdown::render_markdown;
use super::shared::{Pad, RenderContext, pad_entry_with, pad_line_to_width};

/// Renders a rule interrupt as a full-width block.
///
/// Shows the fired rule's name as a header, then the markdown-rendered
/// guidance. Every line is padded to full content width on the interrupt
/// background.
pub fn to_lines(rule: &str, body: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let bg = ctx.theme.rule_interrupt_bg;
    let bg_style = Style::default().bg(bg);
    let muted_style = Style::default().fg(ctx.theme.muted_text).bg(bg);

    let header_text = format!("⚡ Rule fired: {rule}");
    let mut header_line = Line::from(Span::styled(header_text, muted_style));
    pad_line_to_width(&mut header_line, ctx.content_width, bg_style);

    let mut lines = vec![header_line];

    // The body is what the model was told; render it the way a typed message
    // is rendered so the guidance reads the same in the log as in the prompt.
    let body = super::shared::strip_ansi(body);
    if !body.trim().is_empty() {
        let mut body_lines = render_markdown(&body, ctx.content_width, &ctx.theme);
        // Apply the interrupt background to every markdown line, preserving
        // inline styles (bold, code, etc.) via patch.
        for line in &mut body_lines {
            for span in &mut line.spans {
                span.style = span.style.patch(bg_style);
            }
            pad_line_to_width(line, ctx.content_width, bg_style);
        }
        lines.extend(body_lines);
    }

    let pad_line = Line::from(Span::styled(
        " ".repeat(ctx.content_width as usize),
        bg_style,
    ));
    pad_entry_with(&mut lines, Pad::Both, pad_line);

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

    use super::*;
    use jinn_theme::default_theme;

    fn render_ctx() -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    #[rstest::rstest]
    fn header_names_the_fired_rule() {
        // Given a rule interrupt for the "no-cat" rule.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("no-cat", "stop using cat", &ctx);

        // Then the header names the rule.
        let header: String = lines[1].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            header.contains("no-cat"),
            "header should name the fired rule, got {header:?}"
        );
    }

    #[rstest::rstest]
    fn body_is_rendered_without_an_expand_hint() {
        // Given a rule interrupt whose body carries the guidance.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("no-cat", "stop using cat", &ctx);

        // Then the guidance appears.
        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            all_text.contains("stop using cat"),
            "body should be shown without expanding, got {all_text:?}"
        );
        // And no collapse hint is offered.
        assert!(
            !all_text.contains("e to expand"),
            "an interrupt is always shown, so no expand hint"
        );
    }

    #[rstest::rstest]
    fn background_uses_rule_interrupt_bg() {
        // Given a rule interrupt.
        let ctx = render_ctx();
        let theme = default_theme();

        // When rendering.
        let lines = to_lines("no-cat", "stop using cat", &ctx);

        // Then its spans carry the rule_interrupt_bg background.
        let has_bg = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.style.bg == Some(theme.rule_interrupt_bg))
        });
        assert!(has_bg, "should use rule_interrupt_bg background");
    }

    #[rstest::rstest]
    fn background_differs_from_the_user_block() {
        // Given the default theme.
        let theme = default_theme();

        // Then the interrupt block is not the user block, or it would be
        // indistinguishable from something the user typed.
        assert_ne!(
            theme.rule_interrupt_bg, theme.user_block_bg,
            "interrupt must be visually distinct from a typed user message"
        );
    }

    #[rstest::rstest]
    fn empty_body_renders_header_only() {
        // Given a rule interrupt with an empty body.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("no-cat", "", &ctx);

        // Then only the pads and the header are drawn.
        assert_eq!(
            lines.len(),
            3,
            "empty body should be pad + header + pad, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn every_line_spans_full_content_width() {
        // Given a rule interrupt rendered at width 80.
        let ctx = render_ctx();

        // When rendering.
        let lines = to_lines("no-cat", "stop using cat", &ctx);

        // Then every line, pads included, is padded to the content width.
        for (idx, line) in lines.iter().enumerate() {
            assert_eq!(
                line.width(),
                80,
                "line {idx} should be padded to content_width, got {}",
                line.width()
            );
        }
    }
}
