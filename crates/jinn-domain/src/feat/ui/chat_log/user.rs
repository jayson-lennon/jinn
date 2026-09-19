//! User entry rendering - markdown-rendered text on a block background.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::protocol::AttachmentOutcome;

use super::markdown::render_markdown;
use super::shared::{Pad, RenderContext, pad_entry_with, pad_line_to_width};

/// Renders a user entry: markdown body on the user-block background, with
/// `@path` tokens colored by resolution outcome — green when attached as an
/// image, red when degraded (missing file or not an image).
pub fn to_lines(
    text: &str,
    outcome: &AttachmentOutcome,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let text = super::shared::strip_ansi(text);
    let mut lines = render_markdown(&text, ctx.content_width, &ctx.theme);

    // Build a lookup from a raw token body to its render color. Resolution
    // happens once at enqueue; this is a frozen result the render reads.
    let token_colors: Vec<(String, Color)> = attached_and_degraded_colors(outcome, ctx);

    // Recolor each `@raw` occurrence within the rendered spans. Coloring is
    // best-effort: a token that can't be split cleanly out of a span is left
    // as-is (the line is never corrupted).
    for line in &mut lines {
        color_at_path_tokens(&mut line.spans, &token_colors);
    }

    // Apply user block background to every line and pad to full width.
    let bg = Style::default().bg(ctx.theme.user_block_bg);
    for line in &mut lines {
        // Patch each span to include the user block background while preserving
        // inline markdown styling (bold, code, etc.).
        for span in &mut line.spans {
            span.style = span.style.patch(bg);
        }
        pad_line_to_width(line, ctx.content_width, bg);
    }

    // Add padding above and below with the user block background.
    let pad_bg = Style::default().bg(ctx.theme.user_block_bg);
    let pad_line = Line::from(Span::styled(" ".repeat(ctx.content_width as usize), pad_bg));
    pad_entry_with(&mut lines, Pad::Both, pad_line);
    lines
}

/// Returns `(raw_token_body, color)` pairs: green for attached, red for
/// degraded.
fn attached_and_degraded_colors(
    outcome: &AttachmentOutcome,
    ctx: &RenderContext,
) -> Vec<(String, Color)> {
    let mut pairs = Vec::with_capacity(outcome.attached.len() + outcome.degraded.len());
    for t in &outcome.attached {
        pairs.push((t.raw.clone(), ctx.theme.success));
    }
    for t in &outcome.degraded {
        pairs.push((t.raw.clone(), ctx.theme.error_text));
    }
    pairs
}

/// Recolors `@raw` substrings within the spans of a single line.
///
/// For each span, scans for any `@raw` (prefixed with `@`) among the token
/// color pairs. On a match the span is split into up to three parts — the text
/// before, the recolored token, and the remainder — with markdown styling
/// preserved via `patch`. The remainder is re-scanned for further matches.
fn color_at_path_tokens(spans: &mut Vec<Span<'static>>, token_colors: &[(String, Color)]) {
    if token_colors.is_empty() {
        return;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        out.extend(split_and_recolor(span, token_colors));
    }
    *spans = out;
}

/// Splits a single span around any `@raw` occurrence, recoloring the match.
fn split_and_recolor(span: Span<'static>, token_colors: &[(String, Color)]) -> Vec<Span<'static>> {
    let mut produced = Vec::new();
    let mut current = span;
    loop {
        let Some((needle_idx, color, needle_len)) = earliest_match(&current.content, token_colors)
        else {
            produced.push(current);
            break;
        };
        let (before, matched_and_rest) = current.content.split_at(needle_idx);
        let (token_text, remainder) = matched_and_rest.split_at(needle_len);
        // Emit the text before the token, if any, preserving original style.
        if !before.is_empty() {
            produced.push(Span::styled(before.to_owned(), current.style));
        }
        // Emit the recolored token, patched over the original style.
        produced.push(Span::styled(
            token_text.to_owned(),
            current.style.patch(Style::default().fg(color)),
        ));
        // Continue scanning the remainder for further matches.
        current = Span::styled(remainder.to_owned(), current.style);
    }
    produced
}

/// Finds the earliest `@raw` occurrence in `text`, returning its byte index,
/// the color to apply, and the full match length (`@` + raw body).
fn earliest_match(text: &str, token_colors: &[(String, Color)]) -> Option<(usize, Color, usize)> {
    token_colors
        .iter()
        .filter_map(|(raw, color)| {
            let needle = format!("@{raw}");
            text.find(&needle).map(|i| (i, *color, needle.len()))
        })
        .min_by_key(|(i, _, _)| *i)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::feat::ui::chat_log::shared::RenderContext;
    use crate::protocol::{AttachmentOutcome, ResolvedToken};

    fn ctx() -> RenderContext {
        RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 5,
            theme: crate::feat::theme::default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        }
    }

    fn token_span(lines: &[Line<'_>], needle: &str) -> Option<ratatui::style::Color> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains(needle))
            .and_then(|s| s.style.fg)
    }

    #[rstest::rstest]
    #[test]
    fn attached_token_renders_green() {
        // Given a user entry whose @img.png token attached.
        let outcome = AttachmentOutcome {
            attached: vec![ResolvedToken {
                raw: "img.png".to_owned(),
                abs: "/abs/img.png".into(),
            }],
            degraded: vec![],
        };

        // When rendering.
        let lines = to_lines("describe @img.png here", &outcome, &ctx());

        // Then the @img.png token uses the success (green) foreground.
        assert_eq!(token_span(&lines, "@img.png"), Some(ctx().theme.success));
    }

    #[rstest::rstest]
    #[test]
    fn degraded_missing_token_renders_red() {
        // Given a user entry whose @whatever token degraded (missing file).
        let outcome = AttachmentOutcome {
            attached: vec![],
            degraded: vec![ResolvedToken {
                raw: "whatever".to_owned(),
                abs: "/abs/whatever".into(),
            }],
        };

        // When rendering.
        let lines = to_lines("describe @whatever here", &outcome, &ctx());

        // Then the @whatever token uses the error_text (red) foreground.
        assert_eq!(
            token_span(&lines, "@whatever"),
            Some(ctx().theme.error_text)
        );
    }

    #[rstest::rstest]
    #[test]
    fn degraded_non_image_token_renders_red() {
        // Given a user entry whose @notes.txt token degraded (not an image).
        let outcome = AttachmentOutcome {
            attached: vec![],
            degraded: vec![ResolvedToken {
                raw: "notes.txt".to_owned(),
                abs: "/abs/notes.txt".into(),
            }],
        };

        // When rendering.
        let lines = to_lines("see @notes.txt", &outcome, &ctx());

        // Then the @notes.txt token uses the error_text (red) foreground.
        assert_eq!(
            token_span(&lines, "@notes.txt"),
            Some(ctx().theme.error_text)
        );
    }

    #[rstest::rstest]
    #[test]
    fn non_boundary_at_word_renders_without_outcome_color() {
        // Given a user entry with an email-style @ (no boundary before it) and
        // an empty outcome (never resolved as an attachment).
        let outcome = AttachmentOutcome::default();

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines("contact foo@bar.com", &outcome, &ctx());

        // Then no span carries the success or error_text color.
        let any_special = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| matches!(s.style.fg, Some(c) if c == theme.success || c == theme.error_text));
        assert!(!any_special, "email-style @ must not be outcome-colored");
    }

    #[rstest::rstest]
    #[test]
    fn mixed_attached_and_degraded_tokens_color_separately() {
        // Given a user entry with one attached and one degraded token.
        let outcome = AttachmentOutcome {
            attached: vec![ResolvedToken {
                raw: "real.png".to_owned(),
                abs: "/abs/real.png".into(),
            }],
            degraded: vec![ResolvedToken {
                raw: "whatever".to_owned(),
                abs: "/abs/whatever".into(),
            }],
        };

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines("see @real.png and @whatever", &outcome, &ctx());

        // Then the attached token is green and the degraded token is red.
        assert_eq!(token_span(&lines, "@real.png"), Some(theme.success));
        assert_eq!(token_span(&lines, "@whatever"), Some(theme.error_text));
    }
}
