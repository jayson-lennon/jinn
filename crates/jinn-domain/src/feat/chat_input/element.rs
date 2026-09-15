//! Renders the chat input prompt line.
//!
//! Shows the user's in-progress message below a `>` prompt. When the user is
//! actively typing (input mode), the prompt and border are highlighted in yellow and
//! the cursor appears at the current cursor position within the text. When browsing
//! (normal mode), the prompt is shown without highlighting and no cursor is displayed.
//!
//! Long lines are word-wrapped at the available width, with continuation lines indented
//! by two spaces. When the content exceeds the visible area, it scrolls to keep the
//! cursor visible.

use crate::common::render_ctx::RenderCtx;
use crate::common::ui_element::UiElement;
use crate::feat::chat_input::state::chat_input_box::ChatInputBoxState;
use crate::feat::chat_input::state::wrap::WrappedLine;
use crate::protocol::Mode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Display element for the user's message composition area.
#[derive(Debug)]
pub struct ChatInputBoxElement;

impl UiElement for ChatInputBoxElement {
    fn name(&self) -> String {
        "chat-input-box".to_owned()
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let input_mode = state.frontend.scope().mode() == Mode::Input;
        let theme = &state.frontend.theme;

        let disabled = state.active_chat_input().disabled();

        let prompt_style = if disabled {
            Style::default().fg(theme.muted_text)
        } else if input_mode {
            Style::default()
                .fg(theme.focus_accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };

        let text_style = if disabled {
            Style::default().fg(theme.muted_text)
        } else {
            Style::default()
        };

        let border_style = if disabled {
            Style::default().fg(theme.muted_text)
        } else if input_mode {
            Style::default().fg(theme.focus_accent)
        } else {
            Style::default().fg(theme.border_unfocused)
        };

        let badge_line = {
            use crate::feat::chat_input::state::chat_input_box::InputMode;
            let mode = state.active_chat_input().input_mode();
            let buffer_count = match mode {
                InputMode::Queue => state.active_session().queue_len(),
                InputMode::Steer => state.active_session().steering_buffer().len(),
            };
            // When the input box is focused: vivid per-mode colors + orange accent.
            // When unfocused: everything muted so the badge reads as informational context.
            let (word_color, accent) = if input_mode {
                let wc = match mode {
                    InputMode::Queue => theme.input_mode_queue,
                    InputMode::Steer => theme.input_mode_steer,
                };
                (wc, theme.accent_action)
            } else {
                (theme.muted_text, theme.muted_text)
            };
            let rest = if buffer_count > 0 {
                format!(":{} · {}]", mode.label(), buffer_count)
            } else {
                format!(":{}]", mode.label())
            };
            Line::from(vec![
                Span::styled("[", Style::default().fg(word_color)),
                Span::styled("Q", Style::default().fg(accent)),
                Span::styled(rest, Style::default().fg(word_color)),
            ])
        };
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(border_style);
        let inner = block.inner(area);
        let max_visible_lines = inner.height as usize;

        // Fetch the memoized wrapped lines ONCE. Every consumer below reuses this slice;
        // `wrapped_lines()` is `&self` and returns the cached slice (refreshed eagerly on
        // every buffer mutation via the mutation seam, and on `set_wrap_width`).
        let input = state.active_chat_input();
        let wrapped = input.wrapped_lines();

        let lines = build_wrapped_lines(
            input,
            wrapped,
            input.scroll_offset(),
            max_visible_lines,
            prompt_style,
            text_style,
        );

        let input_widget = Paragraph::new(lines).block(block);
        frame.render_widget(input_widget, area);

        render_mode_badge(frame, area, badge_line);

        // Render scroll position indicators if content overflows.
        let total_lines = wrapped.len();
        let scroll_offset = input.scroll_offset();
        render_scroll_indicators(
            frame,
            inner,
            total_lines,
            scroll_offset,
            max_visible_lines,
            theme.age_fresh,
            theme.scroll_indicator_bg,
        );

        // Position cursor when in input mode.
        if input_mode {
            let (row, col) = input.cursor_row_col();
            let scroll_offset = input.scroll_offset();
            let visual_row = row.saturating_sub(scroll_offset);
            let prefix_width: usize = 2; // "> " = 2 columns
            let display_col = compute_display_col(input, wrapped, row, col);
            let cursor_x = inner.x + (prefix_width + display_col) as u16;
            let cursor_y = inner.y + visual_row as u16;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

/// Build visual lines from wrapped line data, applying scroll offset and visibility limit.
///
/// The first visual line gets a `> ` prompt prefix, all others get `  ` indentation.
fn build_wrapped_lines<'a>(
    input: &'a ChatInputBoxState,
    wrapped: &[WrappedLine],
    scroll_offset: usize,
    max_visible_lines: usize,
    prompt_style: Style,
    text_style: Style,
) -> Vec<Line<'a>> {
    if input.text().is_empty() {
        return vec![Line::from(vec![Span::styled("> ", prompt_style)])];
    }

    let mut lines = Vec::new();

    for (row, line) in wrapped.iter().enumerate() {
        if row < scroll_offset {
            continue;
        }
        if lines.len() >= max_visible_lines {
            break;
        }

        let prefix = if row == 0 { "> " } else { "  " };
        let content = input.grapheme_slice(line.grapheme_start, line.grapheme_end);
        lines.push(Line::from(vec![
            Span::styled(prefix, prompt_style),
            Span::styled(content, text_style),
        ]));
    }

    // If all lines were scrolled past, show at least the prompt.
    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled("> ", prompt_style)]));
    }

    lines
}

/// Render scroll position indicators when content exceeds the visible area.
///
/// Shows `↑ N` on the top-right when lines are hidden above, and `↓ N` on the
/// bottom-right when lines are hidden below. Styled like the chat log indicator
/// (dark gray on black).
#[expect(clippy::similar_names, reason = "fg/bg pair naming is intentional")]
fn render_scroll_indicators(
    frame: &mut Frame<'_>,
    inner: Rect,
    total_lines: usize,
    scroll_offset: usize,
    max_visible_lines: usize,
    indicator_fg: ratatui::style::Color,
    indicator_bg: ratatui::style::Color,
) {
    let lines_above = scroll_offset;
    let lines_below = total_lines
        .saturating_sub(scroll_offset)
        .saturating_sub(max_visible_lines);

    if lines_above == 0 && lines_below == 0 {
        return;
    }

    let style = Style::default().fg(indicator_fg).bg(indicator_bg);

    if lines_above > 0 {
        let label = format!("↑ {lines_above}");
        render_indicator_overlay(frame, &label, inner, inner.y, style);
    }

    if lines_below > 0 {
        let label = format!("↓ {lines_below}");
        let bottom_y = inner.y + inner.height.saturating_sub(1);
        render_indicator_overlay(frame, &label, inner, bottom_y, style);
    }
}

/// Render a single indicator label as a right-aligned overlay on the given row.
fn render_indicator_overlay(frame: &mut Frame<'_>, label: &str, inner: Rect, y: u16, style: Style) {
    let indicator_line = Line::from(Span::styled(label, style));
    let indicator_width = u16::try_from(indicator_line.width())
        .unwrap_or(inner.width)
        .min(inner.width);
    let indicator = Paragraph::new(indicator_line);
    let indicator_area = Rect {
        x: inner.x + inner.width.saturating_sub(indicator_width),
        y,
        width: indicator_width,
        height: 1,
    };
    frame.render_widget(indicator, indicator_area);
}

/// Render the `[Q:QUEUE]`/`[Q:STEER]` mode badge as an overlay 2 columns in from the
/// left, so it aligns with the cursor column (the `"> "` prompt prefix is 2 cells wide)
/// and exposes the bottom border `─` line in columns 0–1. The `Q` is the orange hotkey
/// accent; the rest of the badge uses the per-mode word color.
fn render_mode_badge(frame: &mut Frame<'_>, area: Rect, badge_line: Line<'_>) {
    let badge_width = u16::try_from(badge_line.width())
        .unwrap_or(area.width)
        .min(area.width.saturating_sub(2));
    let badge_area = Rect {
        x: area.x + 2,
        y: area.bottom().saturating_sub(1),
        width: badge_width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(badge_line), badge_area);
}

/// Converts a grapheme offset within a wrapped line to a display column.
///
/// Sums the display widths of graphemes from the start of the wrapped line
/// up to `col` graphemes in. For ASCII text, this is equivalent to `col`.
/// For wide characters (CJK, emoji), each grapheme may contribute 2+ columns.
fn compute_display_col(
    input: &ChatInputBoxState,
    lines: &[WrappedLine],
    row: usize,
    col: usize,
) -> usize {
    let Some(line) = lines.get(row) else {
        return col;
    };
    let end = line.grapheme_start + col;
    if line.grapheme_start >= end {
        return 0;
    }
    UnicodeWidthStr::width(input.grapheme_slice(line.grapheme_start, end))
}
