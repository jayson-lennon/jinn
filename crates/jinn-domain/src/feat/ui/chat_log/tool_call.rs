//! Tool call entry rendering - supports streaming, collapsed, and expanded modes.
//!
//! **Bash tools** always render as a single line: `$ <command>`, extracted from
//! JSON arguments, truncated to content width.
//!
//! **Non-bash tools** have three rendering modes:
//!
//! - **Streaming** (`ctx.is_streaming`): Arguments are displayed with `\n`
//!   unescaped into real newlines, showing file content as it arrives.
//!   Collapsed to `tool_entry_max_lines` with a truncation indicator.
//! - **Finalized + Collapsed** (default): Single line `name arguments`,
//!   truncated to content width. Same as the original behavior.
//! - **Finalized + Expanded** (`ctx.is_expanded`): Full arguments with
//!   newlines rendered, no truncation.
//!
//! A `task` tool call still awaiting its result while its linked child
//! session runs (`ctx.is_waiting_on_subagent`) renders an additional status
//! line beneath the call, purple on the subagent block background.
//!
//! Task calls (tool name `task`) render on the light purple subagent block
//! background; their result closes the block with a status row.
//!
//! Background color is determined by the paired tool result's status:
//! no background while pending, green on success, red on failure.

use crate::protocol::ToolResultStatus;
use jinn_tools_msg::TASK_TOOL_NAME;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::shared::{RenderContext, pad_line_to_width, truncate_to_width};

/// Status line shown under a `task` call whose subagent session is running.
const WAITING_TEXT: &str = "Waiting for subagent session to complete";

/// Hint appended to the waiting and outcome lines — the enter-key affordance.
pub(crate) const ENTER_HINT: &str = " · press enter to open";

/// Render a tool call entry to visual lines.
///
/// Dispatches to the appropriate sub-function based on tool name and context.
///
/// A `task` call renders on [`jinn_theme::Theme::subagent_bg`] — the light
/// purple subagent block — and its waiting line carries that background too.
/// All other tools render with the usual status-derived backgrounds.
pub fn to_lines(name: &str, arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    if name == TASK_TOOL_NAME {
        return to_lines_task(name, arguments, ctx);
    }
    if name == "bash" {
        to_lines_bash(arguments, ctx)
    } else if ctx.is_streaming {
        to_lines_streaming(name, arguments, ctx)
    } else if ctx.is_expanded {
        to_lines_expanded(name, arguments, ctx)
    } else {
        to_lines_collapsed(name, arguments, ctx)
    }
}

/// Task tool call: like the collapsed/streaming/expanded variants, but styled
/// with the subagent block background and the purple waiting line.
fn to_lines_task(name: &str, arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let mut lines = if ctx.is_streaming {
        to_lines_streaming(name, arguments, ctx)
    } else if ctx.is_expanded {
        to_lines_expanded(name, arguments, ctx)
    } else {
        to_lines_collapsed(name, arguments, ctx)
    };

    repaint_task_block(&mut lines, ctx);

    if ctx.is_waiting_on_subagent {
        let full = format!("{WAITING_TEXT}{ENTER_HINT}");
        let mut waiting = Line::from(Span::styled(
            truncate_to_width(&full, ctx.content_width as usize),
            Style::default()
                .fg(ctx.theme.subagent_fg)
                .bg(ctx.theme.subagent_bg),
        ));
        pad_line_to_width(
            &mut waiting,
            ctx.content_width,
            Style::default().bg(ctx.theme.subagent_bg),
        );
        lines.push(waiting);
    }

    lines
}

/// Restyle task-entry lines onto the subagent block background.
///
/// Task entries drop the status-derived backgrounds entirely — the block's
/// light purple is the identity signal, and the outcome is reported by the
/// status row appended to the task result.
fn repaint_task_block(lines: &mut [Line<'static>], ctx: &RenderContext) {
    let style = Style::default()
        .fg(ctx.theme.primary_text)
        .bg(ctx.theme.subagent_bg);
    let pad_style = Style::default().bg(ctx.theme.subagent_bg);
    for line in lines.iter_mut() {
        for span in &mut line.spans {
            span.style = style;
        }
        pad_line_to_width(line, ctx.content_width, pad_style);
    }
}

/// Bash tool call: single line `$ <command>`, truncated to content width.
fn to_lines_bash(arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let display_text = format_bash_display(arguments);
    let truncated = truncate_to_width(&display_text, ctx.content_width as usize);

    let fg = ctx.theme.primary_text;
    let bg = status_background(ctx);
    let style = match bg {
        Some(bg_color) => Style::default().fg(fg).bg(bg_color),
        None => Style::default().fg(fg),
    };

    let mut lines = vec![Line::from(Span::styled(truncated, style))];

    if let (Some(bg_color), Some(first_line)) = (bg, lines.first_mut()) {
        pad_line_to_width(first_line, ctx.content_width, Style::default().bg(bg_color));
    }

    lines
}

/// Non-bash finalized + collapsed: single line `name arguments`, truncated.
fn to_lines_collapsed(name: &str, arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let display_text = format_non_bash_display(name, arguments);
    let truncated = truncate_to_width(&display_text, ctx.content_width as usize);

    let fg = ctx.theme.primary_text;
    let bg = status_background(ctx);
    let style = match bg {
        Some(bg_color) => Style::default().fg(fg).bg(bg_color),
        None => Style::default().fg(fg),
    };

    let mut lines = vec![Line::from(Span::styled(truncated, style))];

    if let (Some(bg_color), Some(first_line)) = (bg, lines.first_mut()) {
        pad_line_to_width(first_line, ctx.content_width, Style::default().bg(bg_color));
    }

    lines
}

/// Non-bash streaming: multi-line with `\n` unescaped, collapsed to max lines.
///
/// Follows the same pattern as `tool_result::to_lines_collapsed`:
/// shows the last N lines with a truncation indicator when content exceeds
/// `tool_entry_max_lines`.
fn to_lines_streaming(name: &str, arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let style = content_style(ctx);

    let text = super::shared::unescape_newlines(arguments);
    let text = super::shared::strip_ansi(&text);
    let display = format!("{name} {text}");
    let all_lines: Vec<&str> = display.split('\n').collect();

    let mut lines = Vec::new();
    let max = ctx.tool_entry_max_lines as usize;

    if all_lines.len() <= max {
        for line_text in &all_lines {
            lines.push(Line::from(Span::styled(
                truncate_to_width(line_text, ctx.content_width as usize),
                style,
            )));
        }
    } else {
        let remaining = all_lines.len() - max;
        if let Some(remaining_lines) = all_lines.get(remaining..) {
            for line_text in remaining_lines {
                lines.push(Line::from(Span::styled(
                    truncate_to_width(line_text, ctx.content_width as usize),
                    style,
                )));
            }
        }
        let indicator = truncate_to_width(
            &format!("---({remaining} lines hidden above)---"),
            ctx.content_width as usize,
        );
        lines.push(Line::from(Span::styled(indicator, style)));
    }

    pad_lines(&mut lines, ctx);
    lines
}

/// Non-bash finalized + expanded: full arguments with newlines, no truncation.
fn to_lines_expanded(name: &str, arguments: &str, ctx: &RenderContext) -> Vec<Line<'static>> {
    let style = content_style(ctx);

    let text = super::shared::unescape_newlines(arguments);
    let text = super::shared::strip_ansi(&text);
    let display = format!("{name} {text}");
    let all_lines: Vec<&str> = display.split('\n').collect();

    let mut lines = Vec::new();
    for line_text in &all_lines {
        lines.push(Line::from(Span::styled(
            truncate_to_width(line_text, ctx.content_width as usize),
            style,
        )));
    }

    pad_lines(&mut lines, ctx);
    lines
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a bash tool call for display: `$ <command>`.
fn format_bash_display(arguments: &str) -> String {
    let command = extract_bash_command(arguments).unwrap_or_else(|| arguments.to_owned());
    let display = format!("$ {command}");
    super::shared::strip_ansi(&display)
}

/// Format a non-bash tool call for single-line display: `name arguments`.
fn format_non_bash_display(name: &str, arguments: &str) -> String {
    let arguments = super::shared::unescape_newlines(arguments);
    let display = format!("{name} {arguments}");
    super::shared::strip_ansi(&display)
}

/// Extract the "command" field from bash JSON arguments.
fn extract_bash_command(arguments: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(arguments).ok()?;
    parsed
        .get("command")?
        .as_str()
        .map(std::borrow::ToOwned::to_owned)
}

/// Determine the background color from the paired result status.
fn status_background(ctx: &RenderContext) -> Option<ratatui::style::Color> {
    match ctx.paired_status? {
        ToolResultStatus::Success => Some(ctx.theme.tool_success_bg),
        ToolResultStatus::Failure => Some(ctx.theme.tool_failure_bg),
        ToolResultStatus::Pending => None,
    }
}

/// Build the content style based on paired status.
fn content_style(ctx: &RenderContext) -> Style {
    let fg = ctx.theme.primary_text;
    match ctx.paired_status {
        Some(ToolResultStatus::Success) => Style::default().fg(fg).bg(ctx.theme.tool_success_bg),
        Some(ToolResultStatus::Failure) => Style::default().fg(fg).bg(ctx.theme.tool_failure_bg),
        Some(ToolResultStatus::Pending) | None => Style::default().fg(fg),
    }
}

/// Pad all lines to full content width so the background spans the entire row.
fn pad_lines(lines: &mut [Line<'static>], ctx: &RenderContext) {
    let bg = status_background(ctx);
    if let Some(bg) = bg {
        let bg_style = Style::default().bg(bg);
        for line in lines.iter_mut() {
            pad_line_to_width(line, ctx.content_width, bg_style);
        }
    }
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
    use jinn_tools_msg::TASK_TOOL_NAME;

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
            tool_entry_max_lines: 6,
            theme: crate::feat::theme::default_theme(),
            paired_status: status,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    fn streaming_context(max_lines: u16) -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: max_lines,
            theme: crate::feat::theme::default_theme(),
            paired_status: None,
            is_streaming: true,
            is_waiting_on_subagent: false,
        }
    }

    fn expanded_context() -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: true,
            tool_entry_max_lines: 6,
            theme: crate::feat::theme::default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    #[rstest::rstest]
    fn bash_renders_with_dollar_prefix() {
        // Given a bash tool call with a JSON command argument.
        let ctx = render_context(6, false);

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls -la"}"#, &ctx);

        // Then the first line starts with "$ ".
        let text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.starts_with("$ ls -la"),
            "bash tool call should start with '$ ls -la', got: {text}"
        );
    }

    #[rstest::rstest]
    fn bash_is_single_line() {
        // Given a bash tool call with multi-line arguments.
        let ctx = render_context(6, false);
        let args = "line 1\\nline 2\\nline 3";

        // When converting to lines.
        let lines = to_lines("bash", args, &ctx);

        // Then there is exactly one line.
        assert_eq!(
            lines.len(),
            1,
            "bash tool call should always be a single line, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn bash_streaming_still_single_line() {
        // Given a bash tool call in streaming mode.
        let ctx = streaming_context(6);

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then there is exactly one line.
        assert_eq!(
            lines.len(),
            1,
            "bash should still be single line during streaming, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn bash_expanded_still_single_line() {
        // Given a bash tool call in expanded mode.
        let ctx = expanded_context();

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then there is exactly one line.
        assert_eq!(
            lines.len(),
            1,
            "bash should still be single line when expanded, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn non_bash_renders_with_name_args() {
        // Given a non-bash tool call.
        let ctx = render_context(6, false);

        // When converting to lines.
        let lines = to_lines("read", r#"{"path":"/foo.rs"}"#, &ctx);

        // Then the first line starts with "read ".
        let text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.starts_with("read "),
            "non-bash tool call should start with 'read ', got: {text}"
        );
    }

    #[rstest::rstest]
    fn non_bash_collapsed_is_single_line() {
        // Given a non-bash tool call with multi-line arguments, not streaming.
        let ctx = render_context(6, false);
        let args = "line 1\\nline 2\\nline 3";

        // When converting to lines.
        let lines = to_lines("write", args, &ctx);

        // Then there is exactly one line (collapsed mode).
        assert_eq!(
            lines.len(),
            1,
            "collapsed non-bash tool call should be single line, got {}",
            lines.len()
        );
    }

    #[rstest::rstest]
    fn streaming_non_bash_renders_multiple_lines() {
        // Given a streaming write tool call with escaped newlines.
        let ctx = streaming_context(6);
        let args = r"line 1\nline 2\nline 3";

        // When converting to lines.
        let lines = to_lines("write", args, &ctx);

        // Then there are multiple lines (name + 3 content lines).
        assert!(
            lines.len() > 1,
            "streaming non-bash should produce multiple lines, got {}",
            lines.len()
        );

        // And the first line contains the tool name.
        let first: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            first.starts_with("write"),
            "first line should start with tool name, got: {first}"
        );
    }

    #[rstest::rstest]
    fn streaming_shows_truncation_indicator() {
        // Given a streaming tool call with more lines than max.
        let ctx = streaming_context(3);
        let args: String = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\\n");

        // When converting to lines.
        let lines = to_lines("write", &args, &ctx);

        // Then some line contains the truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("lines hidden above"))
        });
        assert!(
            has_indicator,
            "streaming tool call exceeding max_lines should show truncation indicator"
        );
    }

    #[rstest::rstest]
    fn streaming_short_args_no_indicator() {
        // Given a streaming tool call with short arguments.
        let ctx = streaming_context(6);
        let args = "short";

        // When converting to lines.
        let lines = to_lines("write", args, &ctx);

        // Then no line contains a truncation indicator.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("lines hidden"))
        });
        assert!(
            !has_indicator,
            "short streaming tool call should not show truncation indicator"
        );
    }

    #[rstest::rstest]
    fn streaming_empty_args_renders_name() {
        // Given a streaming tool call with empty arguments.
        let ctx = streaming_context(6);

        // When converting to lines.
        let lines = to_lines("write", "", &ctx);

        // Then at least one line is produced containing the name.
        assert!(
            !lines.is_empty(),
            "streaming tool call with empty args should produce lines"
        );
        let first: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            first.starts_with("write"),
            "first line should start with tool name, got: {first}"
        );
    }

    #[rstest::rstest]
    fn expanded_non_bash_shows_all_lines() {
        // Given an expanded write tool call with escaped newlines.
        let ctx = expanded_context();
        let args = r"line 1\nline 2\nline 3";

        // When converting to lines.
        let lines = to_lines("write", args, &ctx);

        // Then multiple lines are produced.
        assert!(
            lines.len() > 1,
            "expanded non-bash should produce multiple lines, got {}",
            lines.len()
        );

        // And no truncation indicator appears.
        let has_indicator = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| s.content.contains("lines hidden"))
        });
        assert!(
            !has_indicator,
            "expanded tool call should not show truncation indicator"
        );
    }

    #[rstest::rstest]
    fn pending_has_no_background() {
        // Given a tool call with no paired result.
        let ctx = render_context_with_status(None);

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line has no background color.
        assert!(
            lines[0].spans.iter().all(|s| s.style.bg.is_none()),
            "pending tool call should have no background"
        );
    }

    #[rstest::rstest]
    fn success_has_green_background() {
        // Given a tool call paired with a successful result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let theme = crate::feat::theme::default_theme();

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line has the success background color.
        let has_bg = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.tool_success_bg));
        assert!(has_bg, "successful tool call should have green background");
    }

    #[rstest::rstest]
    fn failure_has_red_background() {
        // Given a tool call paired with a failed result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Failure));
        let theme = crate::feat::theme::default_theme();

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line has the failure background color.
        let has_bg = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.tool_failure_bg));
        assert!(has_bg, "failed tool call should have red background");
    }

    #[rstest::rstest]
    fn pending_status_has_no_background() {
        // Given a tool call paired with a pending result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Pending));

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line has no background color.
        assert!(
            lines[0].spans.iter().all(|s| s.style.bg.is_none()),
            "pending paired tool call should have no background"
        );
    }

    #[rstest::rstest]
    fn bash_json_parse_failure_fallback() {
        // Given a bash tool call with malformed JSON arguments.
        let ctx = render_context(6, false);

        // When converting to lines.
        let lines = to_lines("bash", "not json at all", &ctx);

        // Then the line still starts with "$ ".
        let text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.starts_with("$ "),
            "bash should fallback to raw args on JSON parse failure, got: {text}"
        );
    }

    #[rstest::rstest]
    fn long_command_truncated_to_width() {
        // Given a tool call with a very long command and narrow content width.
        let ctx = RenderContext {
            content_width: 20,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 6,
            theme: crate::feat::theme::default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        };
        let long_cmd = "a".repeat(100);

        // When converting to lines.
        let lines = to_lines("bash", &format!(r#"{{"command":"{long_cmd}"}}"#), &ctx);

        // Then the line is truncated to 20 graphemes.
        let text: String = lines[0].spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(
            text.len(),
            20,
            "long command should be truncated to content_width, got {} chars",
            text.len()
        );
    }

    #[rstest::rstest]
    fn uses_primary_text_foreground() {
        // Given a tool call.
        let ctx = render_context(6, false);
        let theme = crate::feat::theme::default_theme();

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line uses primary_text as foreground.
        let has_fg = lines[0]
            .spans
            .iter()
            .any(|s| s.style.fg == Some(theme.primary_text));
        assert!(has_fg, "tool call should use primary_text foreground");
    }

    #[rstest::rstest]
    fn task_call_lines_carry_subagent_background() {
        // Given a task tool call.
        let ctx = render_context(6, false);
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(TASK_TOOL_NAME, r#"{"prompt":"hi"}"#, &ctx);

        // Then the call line uses the subagent block background, padded to
        // the full content width.
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(
            spans.iter().all(|s| s.style.bg == Some(theme.subagent_bg)),
            "task call should render on subagent_bg"
        );
        assert_eq!(lines[0].width(), ctx.content_width as usize);
    }

    #[rstest::rstest]
    fn non_task_call_keeps_status_backgrounds() {
        // Given a non-task tool call paired with a successful result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Success));
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines("bash", r#"{"command":"ls"}"#, &ctx);

        // Then the line keeps the success background, not the subagent one.
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.tool_success_bg)),
            "non-task call should keep status backgrounds"
        );
        assert!(
            !lines[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.subagent_bg)),
            "non-task call should not use subagent_bg"
        );
    }

    #[rstest::rstest]
    fn waiting_line_on_task_call_uses_block_background() {
        // Given a waiting task call.
        let mut ctx = render_context(6, false);
        ctx.is_waiting_on_subagent = true;
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(TASK_TOOL_NAME, r#"{"prompt":"hi"}"#, &ctx);

        // Then the waiting line is the last line, purple on subagent_bg.
        let waiting = lines.last().expect("lines");
        let text: String = waiting.spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains(WAITING_TEXT), "expected waiting line: {text}");
        assert!(
            waiting
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.subagent_bg)),
            "waiting line should sit on subagent_bg"
        );
        assert!(
            waiting
                .spans
                .iter()
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme.subagent_fg)),
            "waiting text should be subagent_fg"
        );
    }

    #[rstest::rstest]
    fn task_call_status_background_is_replaced_by_block() {
        // Given a task call paired with a failed result.
        let ctx = render_context_with_status(Some(ToolResultStatus::Failure));
        let theme = ctx.theme.clone();

        // When converting to lines.
        let lines = to_lines(TASK_TOOL_NAME, r#"{"prompt":"hi"}"#, &ctx);

        // Then the call renders on subagent_bg, not the failure background —
        // the outcome is reported by the result's status row instead.
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.subagent_bg)),
            "task call should ignore status backgrounds"
        );
    }
}
