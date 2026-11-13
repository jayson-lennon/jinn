//! Compaction entry rendering - collapsible block with summary.
//!
//! Renders a compaction entry as a full-width light-purple block.
//!
//! **Collapsed** (default): shows a header line with message count and token
//! estimate, plus a hint line `(e to expand)`.
//!
//! **Expanded:** shows the header followed by the full markdown-rendered
//! summary text. All lines have the `compaction_block_bg` background padded
//! to full content width.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markdown::render_markdown;
use super::shared::{Pad, RenderContext, pad_entry_with, pad_line_to_width};

/// Renders a compaction entry as a collapsible block.
///
/// When collapsed, shows the header line and a hint to expand.
/// When expanded, shows the header plus the full markdown-rendered summary.
/// All lines are padded to full content width with the compaction background.
pub fn to_lines(
    summary: &str,
    entries_compacted: usize,
    tokens_before: usize,
    tokens_after: usize,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let bg = ctx.theme.compaction_block_bg;
    let bg_style = Style::default().bg(bg);
    let muted_style = Style::default().fg(ctx.theme.muted_text).bg(bg);

    // Header line (always shown).
    let header_text =
        format!("📜 Compacted {entries_compacted} messages (~{tokens_before} tokens)");
    let mut header_line = Line::from(Span::styled(header_text, muted_style));
    pad_line_to_width(&mut header_line, ctx.content_width, bg_style);

    // Summary size line (always shown).
    let summary_text = format!("Summary size: ~{tokens_after} tokens");
    let mut summary_line = Line::from(Span::styled(summary_text, muted_style));
    pad_line_to_width(&mut summary_line, ctx.content_width, bg_style);

    let mut lines = vec![header_line, summary_line];

    if ctx.is_expanded {
        // Render summary as markdown.
        let summary = super::shared::strip_ansi(summary);
        if !summary.trim().is_empty() {
            let mut summary_lines = render_markdown(&summary, ctx.content_width, &ctx.theme);
            // Apply compaction background to every markdown line, preserving
            // inline styles (bold, code, etc.) via patch.
            for line in &mut summary_lines {
                for span in &mut line.spans {
                    span.style = span.style.patch(bg_style);
                }
                pad_line_to_width(line, ctx.content_width, bg_style);
            }
            lines.extend(summary_lines);
        }
    } else {
        // Collapsed: hint line.
        let hint_text = "(e to expand)";
        let mut hint_line = Line::from(Span::styled(hint_text, muted_style));
        pad_line_to_width(&mut hint_line, ctx.content_width, bg_style);
        lines.push(hint_line);
    }

    // Pad top/bottom with styled background lines (same as user.rs).
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

    fn render_ctx(is_expanded: bool) -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        }
    }

    #[rstest::rstest]
    fn collapsed_shows_header_summary_and_hint() {
        // Given a compaction entry, not expanded.
        let ctx = render_ctx(false);

        // When rendering.
        let lines = to_lines("some summary", 5, 1000, 50, &ctx);

        // Then there are 5 lines: top pad + header + summary size + hint + bottom pad.
        assert_eq!(lines.len(), 5);
        // And the header line contains the compacted count.
        let header: String = lines[1].spans.iter().map(|s| s.content.clone()).collect();
        assert!(header.contains("5 messages"));
        assert!(header.contains("~1000 tokens"));
        // And the summary size line contains the token count.
        let summary_size: String = lines[2].spans.iter().map(|s| s.content.clone()).collect();
        assert!(summary_size.contains("Summary size"));
        assert!(summary_size.contains("~50 tokens"));
        // And the hint line contains "e to expand".
        let hint: String = lines[3].spans.iter().map(|s| s.content.clone()).collect();
        assert!(hint.contains("e to expand"));
    }

    #[rstest::rstest]
    fn expanded_renders_markdown_summary() {
        // Given an expanded compaction entry with summary text.
        let ctx = render_ctx(true);

        // When rendering.
        let lines = to_lines("Summary of the conversation.", 3, 500, 25, &ctx);

        // Then some content line contains the summary text.
        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(
            all_text.contains("Summary of the conversation."),
            "expanded mode should contain the summary text"
        );
        // And no line contains "e to expand".
        assert!(
            !all_text.contains("e to expand"),
            "expanded mode should not show the hint"
        );
    }

    #[rstest::rstest]
    fn padding_added_with_background() {
        // Given a compaction entry.
        let ctx = render_ctx(false);
        let theme = default_theme();

        // When rendering.
        let lines = to_lines("summary", 1, 100, 10, &ctx);

        // Then the top and bottom pad lines have the compaction background.
        let top_pad = &lines[0];
        let bottom_pad = lines.last().expect("should have bottom pad");
        let has_bg = |line: &Line<'_>| {
            line.spans
                .iter()
                .any(|s| s.style.bg == Some(theme.compaction_block_bg))
        };
        assert!(has_bg(top_pad), "top pad should have compaction background");
        assert!(
            has_bg(bottom_pad),
            "bottom pad should have compaction background"
        );
    }

    #[rstest::rstest]
    fn all_content_lines_padded_to_full_width() {
        // Given a collapsed compaction entry with content width 80.
        let ctx = render_ctx(false);

        // When rendering.
        let lines = to_lines("summary", 1, 100, 50, &ctx);

        // Then every content line (not just pad lines) spans full width.
        // Header is lines[1], summary size is lines[2], hint is lines[3].
        let header_width: usize = lines[1].width();
        let summary_width: usize = lines[2].width();
        let hint_width: usize = lines[3].width();
        assert_eq!(
            header_width, 80,
            "header line should be padded to content_width"
        );
        assert_eq!(
            summary_width, 80,
            "summary size line should be padded to content_width"
        );
        assert_eq!(
            hint_width, 80,
            "hint line should be padded to content_width"
        );
    }

    #[rstest::rstest]
    fn background_uses_compaction_block_bg() {
        // Given a compaction entry.
        let ctx = render_ctx(false);
        let theme = default_theme();

        // When rendering.
        let lines = to_lines("summary", 1, 100, 50, &ctx);

        // Then at least one span has the compaction_block_bg background.
        let has_bg = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.style.bg == Some(theme.compaction_block_bg))
        });
        assert!(has_bg, "should use compaction_block_bg background");
    }

    #[rstest::rstest]
    fn empty_summary_expanded_shows_header_only() {
        // Given an expanded compaction entry with empty summary.
        let ctx = render_ctx(true);

        // When rendering.
        let lines = to_lines("", 1, 100, 0, &ctx);

        // Then there are 4 lines: top pad + header + summary size + bottom pad (no summary content lines).
        assert_eq!(
            lines.len(),
            4,
            "empty summary expanded should have pad + header + summary size + pad, got {}",
            lines.len()
        );
        // And the header line contains the compacted count.
        let header: String = lines[1].spans.iter().map(|s| s.content.clone()).collect();
        assert!(header.contains("1 messages"));
    }

    #[rstest::rstest]
    fn whitespace_only_summary_expanded_shows_header_only() {
        // Given an expanded compaction entry with whitespace-only summary.
        let ctx = render_ctx(true);

        // When rendering.
        let lines = to_lines("   \n  \n  ", 1, 100, 0, &ctx);

        // Then there are 4 lines: top pad + header + summary size + bottom pad.
        assert_eq!(
            lines.len(),
            4,
            "whitespace-only summary expanded should have pad + header + summary size + pad, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn header_uses_muted_text_foreground() {
        // Given a compaction entry.
        let ctx = render_ctx(false);
        let theme = default_theme();

        // When rendering.
        let lines = to_lines("summary", 1, 100, 50, &ctx);

        // Then the header line uses muted_text as foreground.
        let header_line = &lines[1];
        let has_muted_fg = header_line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(theme.muted_text));
        assert!(has_muted_fg, "header should use muted_text foreground");
    }
}
