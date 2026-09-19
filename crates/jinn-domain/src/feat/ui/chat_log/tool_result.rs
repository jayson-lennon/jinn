//! Tool result entry rendering.
//!
//! Renders the output and metadata of a tool execution. The tool call itself
//! (the "header" line with `$ <command>` or `<name> <args>`) is rendered by
//! `tool_call.rs` - this module handles only the output content.
//!
//! **Collapsed** (default): shows the last N lines of output, with each line
//! truncated to content width. No block padding - minimal escape sequences.
//!
//! **Expanded:** shows all output lines without truncation.
//!
//! Background color is determined by `ctx.paired_status`: no background for
//! pending, green for success, red for failure. Task results (tool name
//! `task`) render on the light purple subagent block background, closed by a
//! status row — green "Subagent task finished" or red "Subagent task failed".
//!
//! Collapsed format:
//! ```text
//! <output line 1>
//! <output line 2>
//! ---(N lines hidden above)---
//! ⚠ Output truncated (X of Y)
//! ```

use crate::feat::skills::loaded_skill_summary_label;
use crate::protocol::ToolResultStatus;
use jinn_core_types::tool_types::TruncationMeta;
use jinn_tools_msg::TASK_TOOL_NAME;
use jinn_tools_msg::truncation::format_size;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::shared::{RenderContext, pad_line_to_width, truncate_to_width};
use super::tool_call::ENTER_HINT;

/// Text shown on the outcome row of a successful task result.
const FINISHED_TEXT: &str = "Subagent task finished";

/// Text shown on the outcome row of a failed task result.
const FAILED_TEXT: &str = "Subagent task failed";

pub fn to_lines(
    name: &str,
    content: &str,
    status: ToolResultStatus,
    truncation: Option<&TruncationMeta>,
    is_alert: bool,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    if name == TASK_TOOL_NAME {
        return to_lines_task(content, status, truncation, is_alert, ctx);
    }

    // Loaded skills are pinned as `<skill name="X" ...>` XML.
    // Show a clean single-line summary instead of the raw XML body.
    if name == "skill" {
        return skill_summary_lines(content, ctx);
    }
    let _ = status;
    if is_alert {
        return alert_lines(content, ctx);
    }
    if ctx.is_expanded {
        return to_lines_expanded(content, truncation, ctx);
    }
    to_lines_collapsed(content, truncation, ctx)
}

/// Task result: content on the subagent block background, closed by a
/// full-width status row — green "Subagent task finished" or red "Subagent
/// task failed" — with white text.
fn to_lines_task(
    content: &str,
    status: ToolResultStatus,
    truncation: Option<&TruncationMeta>,
    is_alert: bool,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let _ = status;
    let mut lines = if is_alert {
        alert_lines(content, ctx)
    } else if ctx.is_expanded {
        to_lines_expanded(content, truncation, ctx)
    } else {
        to_lines_collapsed(content, truncation, ctx)
    };

    lines.push(task_status_row(ctx.paired_status, ctx));

    repaint_task_block(&mut lines, ctx);
    lines
}

/// Build the subagent outcome row: full-width status background with white
/// text. Pending/unpaired results show the purple block background.
fn task_status_row(paired: Option<ToolResultStatus>, ctx: &RenderContext) -> Line<'static> {
    let width = ctx.content_width as usize;
    let (text, bg) = match paired {
        Some(ToolResultStatus::Success) => (FINISHED_TEXT, ctx.theme.tool_success_bg),
        Some(ToolResultStatus::Failure) => (FAILED_TEXT, ctx.theme.tool_failure_bg),
        Some(ToolResultStatus::Pending) | None => {
            return Line::from(Span::styled(
                " ".repeat(width),
                Style::default().bg(ctx.theme.subagent_bg),
            ));
        }
    };
    let text = truncate_to_width(&format!("{text}{ENTER_HINT}"), width);
    let padded = format!("{text}{}", " ".repeat(width.saturating_sub(text.len())));
    Line::from(Span::styled(
        padded,
        Style::default().fg(Color::White).bg(bg),
    ))
}

/// Restyle task-entry lines onto the subagent block background, preserving
/// the status row's own colors (it carries the outcome).
fn repaint_task_block(lines: &mut [Line<'static>], ctx: &RenderContext) {
    let style = Style::default()
        .fg(ctx.theme.primary_text)
        .bg(ctx.theme.subagent_bg);
    let pad_style = Style::default().bg(ctx.theme.subagent_bg);
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter_mut().enumerate() {
        if i == last {
            continue;
        }
        for span in &mut line.spans {
            span.style = style;
        }
        pad_line_to_width(line, ctx.content_width, pad_style);
    }
}

/// Render a pending alert (e.g. a detected bot challenge) as an unmissable
/// block: bright background, dark foreground, padded to the full row.
///
/// The alert styling only applies while the entry is pending —
/// `finalize_tool_result` clears `is_alert`, so the completed result renders
/// with its normal success/failure look.
fn alert_lines(content: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(ctx.theme.challenge_alert_fg)
        .bg(ctx.theme.challenge_alert_bg);

    let text = super::shared::unescape_newlines(content);
    let text = super::shared::strip_ansi(&text);
    let text = text.trim_start_matches('\n');

    let mut lines: Vec<Line<'static>> = text
        .split('\n')
        .map(|line_text| {
            Line::from(Span::styled(
                truncate_to_width(line_text, ctx.content_width as usize),
                style,
            ))
        })
        .collect();

    // Always pad: the whole block is the alert, background must span the row.
    let bg_style = Style::default().bg(ctx.theme.challenge_alert_bg);
    for line in &mut lines {
        pad_line_to_width(line, ctx.content_width, bg_style);
    }

    lines
}

/// Render a loaded-skill tool result as a single clean summary line.
///
/// Skills are pinned as `<skill name="X" ...>` XML, which is unreadable when
/// rendered verbatim. This collapses the body to `"<icon> <name>"` (or a
/// `(skill)` fallback when the name can't be parsed), styled like other content
/// and padded so the success/failure background spans the row.
fn skill_summary_lines(content: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let label = loaded_skill_summary_label(content);
    let style = content_style(ctx);
    let mut lines = vec![Line::from(Span::styled(
        truncate_to_width(&label, ctx.content_width as usize),
        style,
    ))];
    pad_lines(&mut lines, ctx);
    lines
}

/// Collapsed view: last N lines, truncated to content width, no padding.
fn to_lines_collapsed(
    content: &str,
    truncation: Option<&TruncationMeta>,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let style = content_style(ctx);

    let text = super::shared::unescape_newlines(content);
    let text = super::shared::strip_ansi(&text);
    let text = text.trim_start_matches('\n');
    let all_lines: Vec<&str> = text.split('\n').collect();

    let mut lines = Vec::new();
    let max = ctx.tool_entry_max_lines as usize;

    if all_lines.len() <= max {
        // Short content - show all lines.
        for line_text in &all_lines {
            lines.push(Line::from(Span::styled(
                truncate_to_width(line_text, ctx.content_width as usize),
                style,
            )));
        }
    } else {
        // Long content - show last N lines, then indicator at the bottom.
        let remaining = all_lines.len() - max;
        for line_text in all_lines.get(remaining..).unwrap_or(&[]) {
            lines.push(Line::from(Span::styled(
                truncate_to_width(line_text, ctx.content_width as usize),
                style,
            )));
        }
        // Truncation indicator at the bottom.
        let indicator = truncate_to_width(
            &format!("---({remaining} lines hidden above)---"),
            ctx.content_width as usize,
        );
        lines.push(Line::from(Span::styled(indicator, style)));
    }

    // Content-level truncation indicator.
    if let Some(meta) = truncation {
        let label = format_content_truncation_label(meta);
        let indicator_style = content_truncation_style(ctx);
        lines.push(Line::from(Span::styled(
            truncate_to_width(&label, ctx.content_width as usize),
            indicator_style,
        )));
    }

    // Pad all lines to full content width so background spans the entire row.
    pad_lines(&mut lines, ctx);

    lines
}

/// Expanded view: full content, no padding.
fn to_lines_expanded(
    content: &str,
    truncation: Option<&TruncationMeta>,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let style = content_style(ctx);

    let mut lines = Vec::new();

    let text = super::shared::unescape_newlines(content);
    let text = super::shared::strip_ansi(&text);
    let text = text.trim_start_matches('\n');
    let all_lines: Vec<&str> = text.split('\n').collect();

    for line_text in &all_lines {
        lines.push(Line::from(Span::styled((*line_text).to_owned(), style)));
    }

    // Content-level truncation indicator.
    if let Some(meta) = truncation {
        let indicator_style = content_truncation_style(ctx);
        let label = format_content_truncation_label(meta);
        lines.push(Line::from(Span::styled(label, indicator_style)));
    }

    // Pad all lines to full content width so background spans the entire row.
    pad_lines(&mut lines, ctx);

    lines
}

/// Build the content style based on paired status.
///
/// Uses `tool_fg` for foreground. Background is set from paired status:
/// no background for pending/unpaired, green for success, red for failure.
fn content_style(ctx: &RenderContext) -> Style {
    let fg = ctx.theme.tool_fg;
    match ctx.paired_status {
        Some(ToolResultStatus::Success) => Style::default().fg(fg).bg(ctx.theme.tool_success_bg),
        Some(ToolResultStatus::Failure) => Style::default().fg(fg).bg(ctx.theme.tool_failure_bg),
        Some(ToolResultStatus::Pending) | None => Style::default().fg(fg),
    }
}

/// Build the content truncation indicator style.
///
/// Uses `focus_accent` for foreground with the same background as content.
fn content_truncation_style(ctx: &RenderContext) -> Style {
    let fg = ctx.theme.focus_accent;
    match ctx.paired_status {
        Some(ToolResultStatus::Success) => Style::default().fg(fg).bg(ctx.theme.tool_success_bg),
        Some(ToolResultStatus::Failure) => Style::default().fg(fg).bg(ctx.theme.tool_failure_bg),
        Some(ToolResultStatus::Pending) | None => Style::default().fg(fg),
    }
}

/// Get the status background color, if any.
fn status_bg(ctx: &RenderContext) -> Option<ratatui::style::Color> {
    match ctx.paired_status {
        Some(ToolResultStatus::Success) => Some(ctx.theme.tool_success_bg),
        Some(ToolResultStatus::Failure) => Some(ctx.theme.tool_failure_bg),
        Some(ToolResultStatus::Pending) | None => None,
    }
}

/// Pad all lines to full content width so the background spans the entire row.
///
/// Only pads when there is a status background. Pending/unpaired entries are left as-is.
fn pad_lines(lines: &mut [Line<'static>], ctx: &RenderContext) {
    if let Some(bg) = status_bg(ctx) {
        let bg_style = Style::default().bg(bg);
        for line in lines.iter_mut() {
            pad_line_to_width(line, ctx.content_width, bg_style);
        }
    }
}

/// Format a human-readable label for content-level truncation.
///
/// Shows how much of the original output is visible, e.g.:
/// `⚠ Output truncated (500 of 2000 lines, 25.0KB of 100.0KB)`
fn format_content_truncation_label(meta: &TruncationMeta) -> String {
    format!(
        "⚠ Output truncated ({} of {} lines, {} of {})",
        meta.output_lines,
        meta.total_lines,
        format_size(meta.output_bytes),
        format_size(meta.total_bytes),
    )
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
    use crate::feat::ui::chat_log::shared::RenderContext;
    use crate::protocol::ToolResultStatus;

    fn render_context(max_lines: u16, is_expanded: bool) -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded,
            tool_entry_max_lines: max_lines,
            theme: crate::feat::theme::default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    fn render_context_with_status(status: Option<ToolResultStatus>) -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 5,
            theme: crate::feat::theme::default_theme(),
            paired_status: status,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    #[rstest::rstest]
    fn truncated_tool_result_shows_indicator() {
        // Given a 10-line tool result with max_lines=5, not expanded.
        let ctx = render_context(5, false);
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("---(5 lines hidden above)---"))
        });
        assert!(
            has_indicator,
            "truncated tool result should contain '---(5 lines hidden above)---'"
        );
    }

    #[rstest::rstest]
    fn expanded_tool_result_shows_all_lines() {
        // Given a 10-line tool result with max_lines=5, expanded.
        let ctx = render_context(5, true);
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then some line contains "line 10".
        let has_last_line = lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains("line 10")));
        assert!(
            has_last_line,
            "expanded tool result should contain 'line 10'"
        );

        // And no line contains the truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("lines hidden above"))
        });
        assert!(
            !has_indicator,
            "expanded tool result should not show truncation indicator"
        );
    }

    #[rstest::rstest]
    fn short_tool_result_not_truncated() {
        // Given a 3-line tool result with max_lines=5, not expanded.
        let ctx = render_context(5, false);
        let content = "line 1\nline 2\nline 3".to_owned();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then no line contains the truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("lines hidden above"))
        });
        assert!(!has_indicator, "short tool result should not be truncated");
    }

    #[rstest::rstest]
    fn tool_result_no_name_line() {
        // Given a tool result with name "bash" and content "output".
        let ctx = render_context(5, false);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then line 0 contains "output" (no name line).
        let first_content: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            first_content.starts_with("output"),
            "first line should be content, not name, got: {first_content}"
        );

        // And there is exactly 1 line (just content, no name).
        assert_eq!(
            lines.len(),
            1,
            "collapsed tool result should have 1 line (content only), got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn tool_result_with_literal_newline_renders_multiple_lines() {
        // Given a tool result with content containing literal \n.
        let ctx = render_context(5, false);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            r"line one\nline two\nline three",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the result has 3 content lines (no name line).
        assert_eq!(
            lines.len(),
            3,
            "tool result with literal \\n should produce 3 content lines, got {}",
            lines.len()
        );

        // And no line contains the literal two-character \n.
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
            assert!(
                !text.contains("\\n"),
                "line should not contain literal \\n, got: {text}"
            );
        }
    }

    #[rstest::rstest]
    fn pending_tool_result_has_no_background() {
        // Given a pending tool result.
        let ctx = render_context_with_status(None);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Pending,
            None,
            false,
            &ctx,
        );

        // Then no line has a background color.
        assert!(
            !lines.is_empty(),
            "pending tool result should produce lines"
        );
        for line in &lines {
            for span in &line.spans {
                assert!(
                    span.style.bg.is_none(),
                    "pending tool result should have no background, got: {:?}",
                    span.style.bg
                );
            }
        }
    }

    fn sample_truncation_meta() -> TruncationMeta {
        TruncationMeta {
            truncated_by: jinn_core_types::tool_types::TruncatedBy::Lines,
            total_lines: 5000,
            total_bytes: 200_000,
            output_lines: 500,
            output_bytes: 20_000,
        }
    }

    #[rstest::rstest]
    fn content_truncated_tool_result_shows_indicator() {
        // Given a tool result with content-level truncation metadata.
        let ctx = render_context(5, false);
        let meta = sample_truncation_meta();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            Some(&meta),
            false,
            &ctx,
        );

        // Then some line contains the truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("⚠ Output truncated"))
        });
        assert!(
            has_indicator,
            "content-truncated tool result should show truncation indicator"
        );
    }

    #[rstest::rstest]
    fn content_truncated_indicator_contains_line_and_byte_counts() {
        // Given a tool result with specific truncation metadata.
        let ctx = render_context(5, false);
        let meta = sample_truncation_meta();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            Some(&meta),
            false,
            &ctx,
        );

        // Then the indicator contains the expected counts.
        let indicator_line = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|s| s.content.contains("⚠ Output truncated"))
            })
            .expect("should have truncation indicator");
        let text: String = indicator_line
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect();
        assert!(
            text.contains("500 of 5000 lines"),
            "indicator should show output/total lines, got: {text}"
        );
        assert!(
            text.contains("19.5KB of 195.3KB"),
            "indicator should show output/total bytes, got: {text}"
        );
    }

    #[rstest::rstest]
    fn content_truncated_indicator_uses_accent_color() {
        // Given a tool result with content-level truncation metadata.
        let ctx = render_context(5, false);
        let theme = crate::feat::theme::default_theme();
        let meta = sample_truncation_meta();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            Some(&meta),
            false,
            &ctx,
        );

        // Then the indicator line uses the focus_accent color.
        let indicator_line = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|s| s.content.contains("⚠ Output truncated"))
            })
            .expect("should have truncation indicator");
        let has_accent_fg = indicator_line
            .spans
            .iter()
            .any(|s| matches!(s.style.fg, Some(color) if color == theme.focus_accent));
        assert!(
            has_accent_fg,
            "truncation indicator should use focus_accent color"
        );
    }

    #[rstest::rstest]
    fn content_truncated_indicator_shows_when_expanded() {
        // Given a tool result with content-level truncation, expanded.
        let ctx = render_context(5, true);
        let meta = sample_truncation_meta();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            Some(&meta),
            false,
            &ctx,
        );

        // Then the indicator still appears (content was truncated at the source).
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("⚠ Output truncated"))
        });
        assert!(
            has_indicator,
            "content-truncated indicator should appear even when expanded"
        );
    }

    #[rstest::rstest]
    fn truncated_tool_result_shows_last_lines() {
        // Given a 10-line tool result with max_lines=5, not expanded.
        let ctx = render_context(5, false);
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the last 5 lines (6-10) are visible.
        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect();
        for i in 6..=10 {
            assert!(
                all_text.contains(&format!("line {i}")),
                "truncated tool result should contain 'line {i}'"
            );
        }
    }

    #[rstest::rstest]
    fn truncated_tool_result_hides_first_lines() {
        // Given a 10-line tool result with max_lines=5, not expanded.
        let ctx = render_context(5, false);
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the first 5 lines (1-5) are not visible.
        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect();
        for i in 1..=5 {
            assert!(
                !all_text.contains(&format!("line {i}\n"))
                    && !all_text.ends_with(&format!("line {i}")),
                "truncated tool result should not contain 'line {i}'"
            );
        }
    }

    #[rstest::rstest]
    fn non_truncated_tool_result_has_no_content_indicator() {
        // Given a tool result without truncation metadata.
        let ctx = render_context(5, false);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then no line contains the content truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("⚠ Output truncated"))
        });
        assert!(
            !has_indicator,
            "non-truncated tool result should not show content truncation indicator"
        );
    }

    #[rstest::rstest]
    fn collapsed_long_line_truncated_to_content_width() {
        // Given a tool result with a line longer than content_width.
        let ctx = render_context(5, false);
        let long_line = "a".repeat(100);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &long_line,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the content line is truncated to content_width (80 graphemes).
        let content_line: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(
            content_line.len(),
            80,
            "content line should be truncated to 80 chars, got {}",
            content_line.len()
        );
    }

    #[rstest::rstest]
    fn collapsed_truncation_indicator_at_bottom() {
        // Given a 10-line tool result with max_lines=5, not expanded.
        let ctx = render_context(5, false);
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // When converting to lines.
        let lines = to_lines(
            "bash",
            &content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the last line is the truncation indicator.
        let last_text: String = lines
            .last()
            .expect("should have lines")
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect();
        assert!(
            last_text.contains("lines hidden above"),
            "truncation indicator should be the last line, got: {last_text}"
        );
    }

    #[rstest::rstest]
    fn success_has_green_background() {
        // Given a tool result paired with a successful result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let theme = crate::feat::theme::default_theme();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the content line has the success background color.
        let has_bg = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.tool_success_bg));
        assert!(
            has_bg,
            "successful tool result should have green background"
        );
    }

    #[rstest::rstest]
    fn failure_has_red_background() {
        // Given a tool result paired with a failure result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Failure));
        let theme = crate::feat::theme::default_theme();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Failure,
            None,
            false,
            &ctx,
        );

        // Then the content line has the failure background color.
        let has_bg = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.tool_failure_bg));
        assert!(has_bg, "failed tool result should have red background");
    }

    #[rstest::rstest]
    fn expanded_has_no_padding_lines() {
        // Given a tool result, expanded.
        let ctx = render_context(5, true);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then there are no padding lines (just content = 1 line).
        assert_eq!(
            lines.len(),
            1,
            "expanded tool result should have 1 line (content only, no padding), got {}",
            lines.len()
        );
    }

    /// Collect a line's text from its spans.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.clone()).collect()
    }

    #[rstest::rstest]
    fn skill_result_shows_summary_label_without_raw_xml() {
        // Given a pinned skill tool result with well-formed XML content.
        let ctx = render_context(5, false);
        let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";

        // When converting to lines.
        let lines = to_lines(
            "skill",
            content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then there is exactly one line.
        assert_eq!(
            lines.len(),
            1,
            "skill result should render as a single summary line, got {} lines",
            lines.len()
        );
        let text = line_text(&lines[0]);

        // And it shows the icon and name, with no raw XML.
        assert!(
            text.contains('\u{2756}'),
            "skill result should show the diamond icon: {text}"
        );
        assert!(
            text.contains("phased-task-loop"),
            "skill result should show the skill name: {text}"
        );
        assert!(
            !text.contains("<skill"),
            "skill result should not show raw XML: {text}"
        );
    }

    #[rstest::rstest]
    fn skill_result_malformed_content_uses_fallback_label() {
        // Given a pinned skill tool result with malformed content.
        let ctx = render_context(5, false);

        // When converting to lines.
        let lines = to_lines(
            "skill",
            "not a skill xml",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then there is one line showing the fallback label, with no panic.
        assert_eq!(
            lines.len(),
            1,
            "malformed skill should still render one line"
        );
        let text = line_text(&lines[0]);
        assert!(
            text.contains('\u{2756}'),
            "malformed skill result should still show the icon: {text}"
        );
        assert!(
            text.contains("(skill)"),
            "malformed skill result should show the fallback label: {text}"
        );
    }

    #[rstest::rstest]
    fn skill_result_in_expanded_context_still_one_line() {
        // Given a pinned skill tool result in an expanded context.
        let ctx = render_context(5, true);
        let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";

        // When converting to lines.
        let lines = to_lines(
            "skill",
            content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the skill branch short-circuits before the expanded/collapsed split,
        // so the summary is still a single line.
        assert_eq!(
            lines.len(),
            1,
            "expanded skill result should still render as a single summary line"
        );
    }

    #[rstest::rstest]
    fn non_skill_result_unchanged_by_skill_branch() {
        // Given a non-skill tool result.
        let ctx = render_context(5, false);

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "command output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then it renders content (the non-skill path), not the skill label.
        let text = line_text(&lines[0]);
        assert!(
            text.contains("command output"),
            "non-skill result should show raw content, got: {text}"
        );
        assert!(
            !text.contains('\u{2756}'),
            "non-skill result should not show the skill icon: {text}"
        );
    }

    #[rstest::rstest]
    fn skill_result_success_has_background_fill() {
        // Given a successful skill result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";

        // When converting to lines.
        let lines = to_lines(
            "skill",
            content,
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then pad_lines() filled the row to content_width with a green background,
        // so the success row visually spans the full width.
        assert!(
            lines
                .iter()
                .all(|l| l.spans.iter().any(|s| s.style.bg.is_some())),
            "success skill result should have background fill on every line"
        );
    }

    #[rstest::rstest]
    #[test]
    fn alert_tool_result_renders_with_alert_colors() {
        // Given a pending alert entry (challenge detected).
        let ctx = render_context(5, false);
        let theme = ctx.theme.clone();

        // When rendering with is_alert = true.
        let lines = to_lines(
            "sample-tool",
            "⚠ bot challenge detected — solve it in the browser window",
            ToolResultStatus::Pending,
            None,
            true,
            &ctx,
        );

        // Then every span sits on the alert background; text spans use the
        // dark alert foreground (the padding span only carries the bg).
        assert!(!lines.is_empty());
        for line in &lines {
            for span in &line.spans {
                assert_eq!(span.style.bg, Some(theme.challenge_alert_bg));
                if !span.content.trim().is_empty() {
                    assert_eq!(span.style.fg, Some(theme.challenge_alert_fg));
                }
            }
        }
    }

    #[rstest::rstest]
    #[test]
    fn alert_tool_result_pads_lines_to_content_width() {
        // Given a pending alert entry.
        let ctx = render_context(5, false);

        // When rendering with is_alert = true.
        let lines = to_lines(
            "sample-tool",
            "waiting for you",
            ToolResultStatus::Pending,
            None,
            true,
            &ctx,
        );

        // Then each line is padded so the background spans the full row.
        assert_eq!(lines.len(), 1);
        let width = lines[0].width();
        assert!(
            width >= ctx.content_width as usize,
            "line width {width} should be at least {}",
            ctx.content_width
        );
    }

    #[rstest::rstest]
    fn successful_task_result_shows_finished_status_row() {
        // Given a task tool result paired with success.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(
            TASK_TOOL_NAME,
            "child output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then the content line renders on subagent_bg.
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.subagent_bg)),
            "task result content should render on subagent_bg"
        );

        // And the status row reads "Subagent task finished" (with the
        // enter hint) on success bg with white text.
        let status = lines.last().expect("lines");
        let text: String = status.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.contains("Subagent task finished"),
            "status row text mismatch: {text}"
        );
        assert!(
            text.contains("press enter to open"),
            "status row should carry the enter hint: {text}"
        );
        assert!(
            status
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.tool_success_bg)
                    && s.style.fg == Some(Color::White)),
            "status row should be white on success bg"
        );
        // And the row fills the content width (at least one padding column).
        assert!(status.width() >= ctx.content_width as usize - 1);
    }

    #[rstest::rstest]
    fn failed_task_result_shows_failed_status_row() {
        // Given a task tool result paired with failure.
        let ctx = render_context_with_status(Some(ToolResultStatus::Failure));
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(
            TASK_TOOL_NAME,
            "child error",
            ToolResultStatus::Failure,
            None,
            false,
            &ctx,
        );

        // Then the status row reads "Subagent task failed" on failure bg.
        let status = lines.last().expect("lines");
        let text: String = status.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.contains("Subagent task failed"),
            "status row text mismatch: {text}"
        );
        assert!(
            status
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.tool_failure_bg)
                    && s.style.fg == Some(Color::White)),
            "status row should be white on failure bg"
        );
    }

    #[rstest::rstest]
    fn pending_task_result_status_row_uses_block_background() {
        // Given a task tool result without a paired status yet.
        let ctx = render_context(5, false);
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(
            TASK_TOOL_NAME,
            "running",
            ToolResultStatus::Pending,
            None,
            false,
            &ctx,
        );

        // Then the status row is a blank row on subagent_bg — no outcome yet.
        let status = lines.last().expect("lines");
        let text: String = status.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.trim().is_empty(),
            "pending row should be blank: {text}"
        );
        assert!(
            status
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.subagent_bg)),
            "pending status row should use subagent_bg"
        );
    }

    #[rstest::rstest]
    fn non_task_result_has_no_status_row_or_block() {
        // Given a non-task tool result with a success background.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(
            "bash",
            "output",
            ToolResultStatus::Success,
            None,
            false,
            &ctx,
        );

        // Then no line uses subagent_bg or the outcome texts.
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(!all_text.contains("Subagent task"));
        assert!(
            !lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.subagent_bg))),
            "non-task result should not use subagent_bg"
        );
    }
}
