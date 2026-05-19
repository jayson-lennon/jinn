//! Shared highlight utility for picker entry rows.
//!
//! Provides [`highlight_text`] and [`highlight_text_with_bg`], which split a string
//! into styled [`Span`]s based on fuzzy match byte offsets. Used by all picker entry
//! types so the highlight look is consistent.

use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

/// Highlight style for fuzzy-matched characters in picker rows.
///
/// Dark gray background with underline; foreground is inherited from the base style.
///
/// This constant is kept for backward compatibility. Production code should use
/// [`highlight_text_with_bg`] with a theme-provided color instead.
pub const PICKER_HIGHLIGHT_STYLE: Style = Style::new()
    .bg(Color::DarkGray)
    .add_modifier(Modifier::UNDERLINED);

/// Builds a highlight style for the given background color.
///
/// Uses the provided color as background with underline modifier.
/// Foreground is inherited from the base style via patching.
pub fn highlight_style(highlight_bg: Color) -> Style {
    Style::new()
        .bg(highlight_bg)
        .add_modifier(Modifier::UNDERLINED)
}

/// Splits `text` into spans, applying the highlight style to characters whose
/// byte offset falls within one of `match_indices`.
///
/// Matched characters get [`PICKER_HIGHLIGHT_STYLE`] patched onto the base style
/// (preserving the base foreground color).
///
/// # Panics
///
/// Does not panic; string slicing is safe because `byte_off` comes from
/// `char_indices()`, which always yields valid UTF-8 boundaries.
pub fn highlight_text<'a>(
    text: &str,
    base_style: Style,
    match_indices: &[Range<usize>],
) -> Vec<Span<'a>> {
    highlight_text_with_bg(text, base_style, match_indices, Color::DarkGray)
}

/// Theme-aware version of [`highlight_text`] that uses the provided highlight
/// background color instead of the hardcoded default.
///
/// Matched characters get the highlight style (underline + provided bg color)
/// patched onto the base style, preserving the base foreground color.
///
/// # Panics
///
/// Does not panic; grapheme iteration yields valid UTF-8 boundaries.
pub fn highlight_text_with_bg<'a>(
    text: &str,
    base_style: Style,
    match_indices: &[Range<usize>],
    highlight_bg: Color,
) -> Vec<Span<'a>> {
    if match_indices.is_empty() || text.is_empty() {
        return vec![Span::styled(text.to_owned(), base_style)];
    }

    let hl_style = base_style.patch(highlight_style(highlight_bg));

    let mut spans = Vec::new();
    let mut current_segment = String::new();
    let mut in_highlight = false;

    for (byte_off, grapheme) in text.grapheme_indices(true) {
        let is_matched = match_indices.iter().any(|r| r.contains(&byte_off));

        if is_matched != in_highlight {
            if !current_segment.is_empty() {
                spans.push(Span::styled(
                    current_segment,
                    if in_highlight { hl_style } else { base_style },
                ));
            }
            current_segment = String::new();
            in_highlight = is_matched;
        }

        current_segment.push_str(grapheme);
    }

    if !current_segment.is_empty() {
        spans.push(Span::styled(
            current_segment,
            if in_highlight { hl_style } else { base_style },
        ));
    }

    if spans.is_empty() {
        spans.push(Span::styled(text.to_owned(), base_style));
    }

    spans
}
