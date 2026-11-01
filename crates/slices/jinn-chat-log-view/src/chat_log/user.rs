//! User entry rendering - markdown-rendered text on a block background,
//! with a display-formatter registry tried before the markdown path.
//!
//! Some user entries begin with a segment jinn itself authored (the
//! canvas-metrics bookkeeping). A display formatter recognizes such a
//! segment by its text and renders it verbatim in a dedicated style; any
//! remainder, and every unclaimed message, go through the ordinary
//! markdown pipeline. All segments then share one background/padding pass,
//! so the block visual treatment stays in exactly one place.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use jinn_core_types::AttachmentOutcome;

use super::markdown::render_markdown;
use super::shared::{Pad, RenderContext, pad_entry_with, pad_line_to_width};

/// Renders a user entry: markdown body on the user-block background, with
/// `@path` tokens colored by resolution outcome — green when attached as an
/// image, red when degraded (missing file or not an image).
///
/// Before the markdown pipeline runs, the display-formatter registry gets
/// first claim on the text: the first formatter that recognizes a segment
/// renders it, and the unclaimed remainder falls through to markdown.
pub fn to_lines(
    text: &str,
    outcome: &AttachmentOutcome,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let text = super::shared::strip_ansi(text);
    match FORMATTERS.iter().find_map(|fmt| fmt(&text, ctx)) {
        Some(split) => {
            let mut lines = split.lines;
            if !split.rest.trim().is_empty() {
                lines.extend(markdown_body(&split.rest, outcome, ctx));
            }
            finish_block(&mut lines, ctx.theme.user_block_bg, ctx);
            lines
        }
        None => {
            let mut lines = markdown_body(&text, outcome, ctx);
            finish_block(&mut lines, ctx.theme.user_block_bg, ctx);
            lines
        }
    }
}

/// What a display formatter produces: styled lines for the segment it
/// recognized, plus the unclaimed remainder for the markdown path.
struct FormatterSplit {
    lines: Vec<Line<'static>>,
    rest: String,
}

/// A formatter tried against the user display text. Returns `None` to
/// decline; the next formatter is tried, then the markdown path.
type UserDisplayFormatter = fn(&str, &RenderContext) -> Option<FormatterSplit>;

const CANVAS_METRICS_OPEN: &str = "<canvas-metrics";
const CANVAS_METRICS_CLOSE: &str = "</canvas-metrics>";

/// Detects the canvas-metrics block enqueued at the front of a user entry
/// and renders it verbatim — tags, numbering, and props — in the theme's
/// metrics foreground, so jinn-authored bookkeeping stays quiet next to the
/// user's own words. Only a leading block claims: scanning mid-text would
/// grey a message the user typed that merely mentions the tag.
fn canvas_metrics_formatter(text: &str, ctx: &RenderContext) -> Option<FormatterSplit> {
    if !text.starts_with(CANVAS_METRICS_OPEN) {
        return None;
    }
    let close = text.find(CANVAS_METRICS_CLOSE)?;
    let (block, rest) = text.split_at(close + CANVAS_METRICS_CLOSE.len());
    let rest = rest.strip_prefix('\n').unwrap_or(rest);

    let style = Style::default().fg(ctx.theme.metrics_fg);
    let lines = block
        .lines()
        .map(|line| Line::from(Span::styled(line.to_owned(), style)))
        .collect();

    Some(FormatterSplit {
        lines,
        rest: rest.to_owned(),
    })
}

/// Display formatters, tried in order on the user display text. The first
/// claim wins; to support a new jinn-authored display shape, add a function
/// here.
const FORMATTERS: &[UserDisplayFormatter] = &[canvas_metrics_formatter];

/// Renders plain display text as markdown and recolors its `@path` tokens —
/// the content lines of a user-style block, without background or padding.
fn markdown_body(
    text: &str,
    outcome: &AttachmentOutcome,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let mut lines = render_markdown(text, ctx.content_width, &ctx.theme);
    recolor_at_tokens(&mut lines, outcome, ctx);
    lines
}

/// Applies the shared finish pass: block background patched over every span
/// (preserving inline styling), each line padded to full width, and blank
/// padding rows above and below — all carrying the block background.
/// Formatter lines and markdown content share this pass.
fn finish_block(
    lines: &mut Vec<Line<'static>>,
    block_bg: ratatui::style::Color,
    ctx: &RenderContext,
) {
    apply_block_background(lines, ctx.content_width, block_bg);

    // Blank padding rows above and below, carrying the block background.
    let bg = Style::default().bg(block_bg);
    let pad_line = Line::from(Span::styled(" ".repeat(ctx.content_width as usize), bg));
    pad_entry_with(lines, Pad::Both, pad_line);
}

/// Renders a markdown block entry — the shared core behind the user and
/// inter-session adapters, which differ only in block background and in
/// whether `@token` recoloring applies.
///
/// The sequence: markdown render → optional `@token` recolor → background
/// patch over every span → pad each line to full width → blank padding rows
/// above and below, all carrying the block background.
pub(crate) fn text_block_lines(
    text: &str,
    outcome: &AttachmentOutcome,
    block_bg: ratatui::style::Color,
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let text = super::shared::strip_ansi(text);
    let mut lines = markdown_body(&text, outcome, ctx);
    finish_block(&mut lines, block_bg, ctx);
    lines
}

/// Recolors each `@raw` occurrence within the rendered spans. Coloring is
/// best-effort: a token that can't be split cleanly out of a span is left
/// as-is (the line is never corrupted).
fn recolor_at_tokens(
    lines: &mut [Line<'static>],
    outcome: &AttachmentOutcome,
    ctx: &RenderContext,
) {
    // Resolution happens once at enqueue; this is a frozen result the
    // render reads.
    let token_colors: Vec<(String, Color)> = attached_and_degraded_colors(outcome, ctx);
    for line in lines {
        color_at_path_tokens(&mut line.spans, &token_colors);
    }
}

/// Patches every span to carry the block background — preserving inline
/// markdown styling (bold, code, etc.) — and pads each line to full width.
fn apply_block_background(
    lines: &mut [Line<'static>],
    content_width: u16,
    block_bg: ratatui::style::Color,
) {
    let bg = Style::default().bg(block_bg);
    for line in lines {
        for span in &mut line.spans {
            span.style = span.style.patch(bg);
        }
        pad_line_to_width(line, content_width, bg);
    }
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
    use crate::chat_log::shared::RenderContext;
    use jinn_core_types::{AttachmentOutcome, ResolvedToken};
    use jinn_testutil::volume_root;

    fn ctx() -> RenderContext {
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
                abs: volume_root()
                    .join_all(["abs", "img.png"])
                    .expect("rooted test path"),
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
                abs: volume_root()
                    .join_all(["abs", "whatever"])
                    .expect("rooted test path"),
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
                abs: volume_root()
                    .join_all(["abs", "notes.txt"])
                    .expect("rooted test path"),
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
                abs: volume_root()
                    .join_all(["abs", "real.png"])
                    .expect("rooted test path"),
            }],
            degraded: vec![ResolvedToken {
                raw: "whatever".to_owned(),
                abs: volume_root()
                    .join_all(["abs", "whatever"])
                    .expect("rooted test path"),
            }],
        };

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines("see @real.png and @whatever", &outcome, &ctx());

        // Then the attached token is green and the degraded token is red.
        assert_eq!(token_span(&lines, "@real.png"), Some(theme.success));
        assert_eq!(token_span(&lines, "@whatever"), Some(theme.error_text));
    }

    /// The block as enqueue authors it — tags, header line, the numbered
    /// event list, and the props JSON that markdown would mangle.
    fn metrics_block() -> String {
        concat!(
            "<canvas-metrics artifact=\"sundae\" >\n",
            "canvas_ops (7 events)\n",
            "1. create_sundae {\"flavor\":\"vanilla\"}\n",
            "2. add_topping {\"kind\":\"sprinkles\"}\n",
            "</canvas-metrics>",
        )
        .to_owned()
    }

    #[rstest::rstest]
    #[test]
    fn metrics_block_renders_verbatim_in_metrics_foreground() {
        // Given a user entry that opens with the canvas-metrics block,
        // followed by the user's own typed words.
        let display = format!("{}\n\nmy actual message", metrics_block());

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines(&display, &AttachmentOutcome::default(), &ctx());

        // Then every block line renders as its exact stored text — the
        // opening tag with its attributes, the closing tag, the numbered
        // list, and the braces props all intact, none markdown-processed.
        let span_with = |needle: &str| {
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .find(|s| s.content == needle)
        };
        let opening = span_with("<canvas-metrics artifact=\"sundae\" >")
            .expect("opening tag renders verbatim");
        assert_eq!(opening.style.fg, Some(theme.metrics_fg));
        assert_eq!(
            span_with("2. add_topping {\"kind\":\"sprinkles\"}")
                .expect("numbered event line renders verbatim")
                .style
                .fg,
            Some(theme.metrics_fg)
        );
        // And no block span carries any other foreground — the whole block
        // is quiet grey, nothing but the metrics color.
        let block_spans = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.content.contains("canvas"));
        for span in block_spans {
            assert_eq!(span.style.fg, Some(theme.metrics_fg));
        }
    }

    #[rstest::rstest]
    #[test]
    fn text_after_the_block_renders_through_the_markdown_path() {
        // Given a user entry with the block followed by markdown text.
        let display = format!("{}\n\nActual **bold** words", metrics_block());

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines(&display, &AttachmentOutcome::default(), &ctx());

        // Then the bold span is a markdown-style bold, not grey.
        let bold = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content == "bold")
            .expect("markdown body rendered");
        assert!(
            bold.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_ne!(bold.style.fg, Some(theme.metrics_fg));
        // And no remainder span carries the metrics foreground.
        let rest_spans = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.content.contains("Actual") || s.content == "bold");
        for span in rest_spans {
            assert_ne!(span.style.fg, Some(theme.metrics_fg));
        }
    }

    #[rstest::rstest]
    #[test]
    fn display_without_a_metrics_prefix_renders_unchanged() {
        // Given a plain user message with a markdown emphasis and an @token.
        let outcome = AttachmentOutcome::default();
        let text = "plain **msg** here";

        // When rendering through to_lines.
        let lines = to_lines(text, &outcome, &ctx());

        // Then the result is identical to the legacy markdown-plus-block
        // pipeline that preceded the formatter registry.
        let ctx = ctx();
        let legacy = text_block_lines(text, &outcome, ctx.theme.user_block_bg, &ctx);
        assert_eq!(lines, legacy);
    }

    #[rstest::rstest]
    #[test]
    fn prefix_without_a_closing_tag_falls_through() {
        // Given a display that starts with the opening prefix but is
        // truncated before any closing tag.
        let display = "<canvas-metrics artifact=\"sundae\" > cut off mid-block";

        // When rendering.
        let theme = ctx().theme;
        let lines = to_lines(display, &AttachmentOutcome::default(), &ctx());

        // Then no span carries the metrics foreground — the whole entry
        // took the ordinary markdown path, without panicking.
        let any_grey = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.fg == Some(theme.metrics_fg));
        assert!(!any_grey, "a truncated block must fall through to markdown");
    }
}
