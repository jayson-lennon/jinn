//! Word-wrap computation for the chat input box.
//!
//! Maps buffer text + available width → a list of visual lines with grapheme
//! index ranges. Each visual line tracks its start/end grapheme position in the
//! original text, whether it's a wrapped continuation, and which logical line
//! it belongs to.
//!
//! Column counting uses `unicode_width` so that wide characters (CJK, emoji)
//! are correctly counted as 2 display columns.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A single visual line produced by word-wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedLine {
    /// Grapheme index in the original text where this visual line starts.
    pub grapheme_start: usize,
    /// Grapheme index in the original text where this visual line ends (exclusive).
    pub grapheme_end: usize,
    /// Whether this is a wrapped continuation (not starting from a `\n`).
    pub is_continuation: bool,
    /// Which logical line (0-indexed, separated by `\n`) this belongs to.
    pub logical_line_index: usize,
}

/// Wraps `text` to the given `width` (in grapheme columns), returning visual lines.
///
/// Splits on `\n` first, then word-wraps each logical line. Each grapheme is
/// assumed to be 1 column wide (which matches how the input element positions
/// characters). Words are broken if they exceed `width`.
///
/// Returns at least one [`WrappedLine`] even for empty text.
///
/// When `width` is 0, each logical line maps to one visual line (no wrapping).
pub fn wrap_text(text: &str, width: usize) -> Vec<WrappedLine> {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut lines = Vec::new();
    let mut grapheme_offset = 0;
    let mut logical_index = 0;

    // Split into logical lines by `\n`.
    let mut logical_start = 0;
    for (i, g) in graphemes.iter().enumerate() {
        if *g == "\n" {
            let logical_len = i - logical_start;
            wrap_logical_line(
                graphemes.get(logical_start..i).unwrap_or(&[]),
                width,
                grapheme_offset,
                logical_index,
                &mut lines,
            );
            grapheme_offset += logical_len + 1; // +1 for `\n`
            logical_start = i + 1;
            logical_index += 1;
        }
    }

    // Handle the last logical line (no trailing `\n`).
    wrap_logical_line(
        graphemes.get(logical_start..).unwrap_or(&[]),
        width,
        grapheme_offset,
        logical_index,
        &mut lines,
    );

    if lines.is_empty() {
        lines.push(WrappedLine {
            grapheme_start: 0,
            grapheme_end: 0,
            is_continuation: false,
            logical_line_index: 0,
        });
    }

    lines
}

/// Wraps a single logical line (no `\n` characters) into visual lines.
///
/// Each grapheme is counted as 1 column. Word boundaries are at whitespace
/// graphemes. Long words that exceed `width` are broken at grapheme boundaries.
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
fn wrap_logical_line(
    graphemes: &[&str],
    width: usize,
    grapheme_offset: usize,
    logical_index: usize,
    out: &mut Vec<WrappedLine>,
) {
    if graphemes.is_empty() {
        out.push(WrappedLine {
            grapheme_start: grapheme_offset,
            grapheme_end: grapheme_offset,
            is_continuation: false,
            logical_line_index: logical_index,
        });
        return;
    }

    if width == 0 {
        // Degenerate: no wrapping.
        out.push(WrappedLine {
            grapheme_start: grapheme_offset,
            grapheme_end: grapheme_offset + graphemes.len(),
            is_continuation: false,
            logical_line_index: logical_index,
        });
        return;
    }

    let mut line_start = 0; // grapheme index within this logical line
    let mut col = 0; // current column position
    let mut last_word_break = None; // grapheme index of last whitespace position
    let mut first_line = true;

    for (i, g) in graphemes.iter().enumerate() {
        let is_whitespace = g.chars().all(char::is_whitespace);

        if is_whitespace && col > 0 {
            // Record potential break point: break *before* this whitespace,
            // so the whitespace goes to the next line. But actually, for
            // visual wrapping, we want to break *after* the whitespace so
            // the word stays on the current line. Let's think...
            //
            // "hello world" at width 6:
            //   "hello " (6 cols) - break after space
            //   "world" (5 cols)
            //
            // So the break point is i+1 (the grapheme after the whitespace).
            // But only if i+1 < graphemes.len() (not at end of line).
            if i + 1 < graphemes.len() {
                last_word_break = Some(i + 1);
            }
        }

        col += UnicodeWidthStr::width(*g);

        if col > width {
            // Need to wrap.
            if let Some(break_pos) = last_word_break {
                // Break at the recorded word boundary.
                out.push(WrappedLine {
                    grapheme_start: grapheme_offset + line_start,
                    grapheme_end: grapheme_offset + break_pos,
                    is_continuation: !first_line,
                    logical_line_index: logical_index,
                });
                line_start = break_pos;
                first_line = false;
                // Recompute col from the remaining graphemes.
                // i+1 graphemes have been processed; break_pos started the new line.
                col = graphemes.get(break_pos..=i).map_or(0, |gs| {
                    gs.iter().map(|g| UnicodeWidthStr::width(*g)).sum::<usize>()
                });
                last_word_break = None;
            } else {
                // No word break found - break at the current position
                // (forced break in the middle of a long word).
                out.push(WrappedLine {
                    grapheme_start: grapheme_offset + line_start,
                    grapheme_end: grapheme_offset + i,
                    is_continuation: !first_line,
                    logical_line_index: logical_index,
                });
                line_start = i;
                first_line = false;
                col = UnicodeWidthStr::width(*g);
                last_word_break = None;
            }
        }
    }

    // Remaining text after the last line break.
    if line_start < graphemes.len() {
        out.push(WrappedLine {
            grapheme_start: grapheme_offset + line_start,
            grapheme_end: grapheme_offset + graphemes.len(),
            is_continuation: !first_line,
            logical_line_index: logical_index,
        });
    } else if line_start == graphemes.len() {
        // Edge case: the last line ended exactly at the boundary.
        // Push an empty line to represent the cursor position after the last char.
        out.push(WrappedLine {
            grapheme_start: grapheme_offset + graphemes.len(),
            grapheme_end: grapheme_offset + graphemes.len(),
            is_continuation: !first_line,
            logical_line_index: logical_index,
        });
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

    #[rstest::rstest]
    fn empty_text_returns_single_empty_line() {
        // Given empty text and width 20.
        // When wrapping.
        let lines = wrap_text("", 20);

        // Then there is exactly 1 line with start=0, end=0.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 0);
        assert!(!lines[0].is_continuation);
        assert_eq!(lines[0].logical_line_index, 0);
    }

    #[rstest::rstest]
    fn short_text_returns_single_line() {
        // Given "hello" and width 20.
        // When wrapping.
        let lines = wrap_text("hello", 20);

        // Then there is 1 line spanning graphemes 0..5.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
        assert!(!lines[0].is_continuation);
    }

    #[rstest::rstest]
    fn long_text_wraps_at_word_boundary() {
        // Given "hello world and more" at width 10.
        // Line 1: "hello " (6 graphemes) - wrap after space at col 6
        // Line 2: "world and " (10 graphemes) - exactly fits width 10
        // Line 3: "more" (4 graphemes)
        let text = "hello world and more";

        // When wrapping at width 10.
        let lines = wrap_text(text, 10);

        // Then there are 3 lines.
        assert_eq!(lines.len(), 3);
        // And the first line covers "hello " (graphemes 0..6).
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 6);
        assert!(!lines[0].is_continuation);
        // And the second line covers "world and " (graphemes 6..16).
        assert_eq!(lines[1].grapheme_start, 6);
        assert_eq!(lines[1].grapheme_end, 16);
        assert!(lines[1].is_continuation);
        // And the third line covers "more" (graphemes 16..20).
        assert_eq!(lines[2].grapheme_start, 16);
        assert_eq!(lines[2].grapheme_end, 20);
        assert!(lines[2].is_continuation);
        // And all lines belong to the same logical line.
        for line in &lines {
            assert_eq!(line.logical_line_index, 0);
        }
    }

    #[rstest::rstest]
    fn overflow_word_breaks_long_word() {
        // Given a single long word that exceeds width.
        let text = "abcdefghijklmn";

        // When wrapping at width 5.
        let lines = wrap_text(text, 10);

        // Then the word is broken into 2 lines (10 + 4).
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 10);
        assert_eq!(lines[1].grapheme_end - lines[1].grapheme_start, 4);
        assert!(lines[1].is_continuation);
    }

    #[rstest::rstest]
    fn text_with_newlines_produces_multiple_logical_lines() {
        // Given "hello\nworld" with wide width.
        // When wrapping.
        let lines = wrap_text("hello\nworld", 80);

        // Then there are 2 visual lines (no wrapping needed).
        assert_eq!(lines.len(), 2);
        // And they have different logical_line_index.
        assert_eq!(lines[0].logical_line_index, 0);
        assert_eq!(lines[1].logical_line_index, 1);
        // And the first covers "hello" (graphemes 0..5).
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
        // And the second covers "world" (graphemes 6..11, skipping the \n).
        assert_eq!(lines[1].grapheme_start, 6);
        assert_eq!(lines[1].grapheme_end, 11);
    }

    #[rstest::rstest]
    fn wrapping_with_newlines() {
        // Given a long first line and a short second line.
        // "hello world and more" (20 chars) + \n + "short" (5 chars)
        let text = "hello world and more\nshort";

        // When wrapping at width 10.
        let lines = wrap_text(text, 10);

        // Then the first logical line is wrapped into 3 visual lines.
        let first_logical: Vec<_> = lines.iter().filter(|l| l.logical_line_index == 0).collect();
        assert_eq!(first_logical.len(), 3);
        // And the second logical line is a single visual line.
        let second_logical: Vec<_> = lines.iter().filter(|l| l.logical_line_index == 1).collect();
        assert_eq!(second_logical.len(), 1);
        // And the second logical line starts at the correct offset.
        // "hello world and more\n" = 21 graphemes, so "short" starts at 21.
        assert_eq!(second_logical[0].grapheme_start, 21);
        assert_eq!(second_logical[0].grapheme_end, 26);
        // And all ranges are contiguous (within each logical line).
        for window in first_logical.windows(2) {
            assert_eq!(window[0].grapheme_end, window[1].grapheme_start);
        }
    }

    #[rstest::rstest]
    fn unicode_text_wraps_correctly() {
        // Given text with emoji (each emoji is 1 grapheme, 2 display columns).
        let text = "🎉🎊🎈🎁🎁🎊🎈🎉";

        // When wrapping at width 5 (display columns).
        let lines = wrap_text(text, 5);

        // Then emoji wrap by display width: 2 emoji = 4 cols fits, 3rd = 6 > 5.
        // 8 emoji → 4 lines of 2 graphemes each.
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 2);
        assert!(lines[1].is_continuation);
    }

    #[rstest::rstest]
    fn width_zero_returns_single_line_per_logical() {
        // Given "hello\nworld" with width 0.
        // When wrapping.
        let lines = wrap_text("hello\nworld", 0);

        // Then there are 2 lines (one per logical line, no wrapping).
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
        assert_eq!(lines[1].grapheme_start, 6);
        assert_eq!(lines[1].grapheme_end, 11);
    }

    #[rstest::rstest]
    fn trailing_newline_produces_empty_last_line() {
        // Given "hello\n" with wide width.
        // When wrapping.
        let lines = wrap_text("hello\n", 80);

        // Then there are 2 lines.
        assert_eq!(lines.len(), 2);
        // And the first covers "hello".
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
        // And the second is empty (from the trailing \n).
        assert_eq!(lines[1].grapheme_start, 6);
        assert_eq!(lines[1].grapheme_end, 6);
    }

    #[rstest::rstest]
    fn exact_width_no_wrap() {
        // Given text exactly matching the width.
        let text = "hello";

        // When wrapping at width 5.
        let lines = wrap_text(text, 5);

        // Then there is 1 line.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
    }

    #[rstest::rstest]
    fn double_newline_produces_empty_middle_line() {
        // Given "a\n\nb" with wide width.
        // When wrapping.
        let lines = wrap_text("a\n\nb", 80);

        // Then there are 3 logical lines.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 1);
        assert_eq!(lines[0].logical_line_index, 0);
        assert_eq!(lines[1].grapheme_start, 2);
        assert_eq!(lines[1].grapheme_end, 2);
        assert_eq!(lines[1].logical_line_index, 1);
        assert_eq!(lines[2].grapheme_start, 3);
        assert_eq!(lines[2].grapheme_end, 4);
        assert_eq!(lines[2].logical_line_index, 2);
    }

    #[rstest::rstest]
    fn cjk_chars_wrap_at_display_width() {
        // Given 6 CJK characters (each 2 display columns).
        let text = "中文测试字符";

        // When wrapping at width 10 (display columns).
        let lines = wrap_text(text, 10);

        // Then 5 CJK = 10 cols fits, 6th overflows → 2 lines.
        assert_eq!(lines.len(), 2);
        // Line 1: 5 graphemes (10 display cols).
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 5);
        // Line 2: 1 grapheme (2 display cols).
        assert_eq!(lines[1].grapheme_end - lines[1].grapheme_start, 1);
        assert!(lines[1].is_continuation);
    }

    #[rstest::rstest]
    fn cjk_exactly_fits_width() {
        // Given 5 CJK characters (10 display columns).
        let text = "中文测试字";

        // When wrapping at width 10.
        let lines = wrap_text(text, 10);

        // Then no wrap - exactly fits.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 5);
    }

    #[rstest::rstest]
    fn mixed_ascii_cjk_exactly_fits() {
        // Given "hello中文" (5 ASCII + 2 CJK = 9 display cols).
        let text = "hello中文";

        // When wrapping at width 9.
        let lines = wrap_text(text, 9);

        // "hello中文" = 9 cols, exactly fits.
        // Then no wrap.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 7);
    }

    #[rstest::rstest]
    fn mixed_ascii_cjk_forces_wrap() {
        // Given "hello中文测试" (5+2+2 = 9 display cols).
        let text = "hello中文测试";

        // When wrapping at width 7.
        let lines = wrap_text(text, 7);

        // "hello中" = 7 cols, "文" = 2 → 9 > 7, forced break.
        assert_eq!(lines.len(), 2);
        // Line 1: "hello中" = 6 graphemes (7 display cols).
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 6);
        // Line 2: "文测试" = 3 graphemes (6 display cols).
        assert_eq!(lines[1].grapheme_end - lines[1].grapheme_start, 3);
    }

    #[rstest::rstest]
    fn exact_width_no_wrap_boundary() {
        // Given text exactly at width (col == width, NOT > width).
        let text = "hello"; // 5 cols

        // When wrapping at width 5.
        let lines = wrap_text(text, 5);

        // Then there is exactly 1 line (no wrap at boundary).
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 5);
    }

    #[rstest::rstest]
    fn one_over_width_forces_wrap() {
        // Given text one col over width.
        let text = "abcdef"; // 6 cols

        // When wrapping at width 5.
        let lines = wrap_text(text, 5);

        // Then it wraps into 2 lines.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 5);
        assert_eq!(lines[1].grapheme_end - lines[1].grapheme_start, 1);
    }

    #[rstest::rstest]
    fn whitespace_at_end_of_line_is_not_break_point() {
        // Given "hello " (6 cols) at width 6 - the space is the last grapheme.
        let text = "hello ";

        // When wrapping at width 6.
        let lines = wrap_text(text, 6);

        // Then no wrap (no break point recorded because space is at end).
        assert_eq!(lines.len(), 1);
    }

    #[rstest::rstest]
    fn whitespace_mid_line_records_break() {
        // Given "hello world" at width 6 - break should happen after space.
        let text = "hello world";

        // When wrapping at width 6.
        let lines = wrap_text(text, 6);

        // Then line 1 is "hello " (6 cols), line 2 is "world".
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].grapheme_end - lines[0].grapheme_start, 6); // "hello "
        assert_eq!(lines[1].grapheme_end - lines[1].grapheme_start, 5); // "world"
    }

    #[rstest::rstest]
    fn continuation_lines_are_marked() {
        // Given text that wraps.
        let text = "abcdefghij"; // 10 cols

        // When wrapping at width 5.
        let lines = wrap_text(text, 5);

        // Then first line is NOT a continuation, rest ARE.
        assert!(!lines[0].is_continuation);
        for line in &lines[1..] {
            assert!(line.is_continuation);
        }
    }

    #[rstest::rstest]
    fn grapheme_ranges_are_correct() {
        // Given "hello world and more" at width 10.
        let text = "hello world and more";
        let lines = wrap_text(text, 10);

        // Then ranges are contiguous and correct.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].grapheme_start, 0);
        assert_eq!(lines[0].grapheme_end, 6); // "hello "
        assert_eq!(lines[1].grapheme_start, 6);
        assert_eq!(lines[1].grapheme_end, 16); // "world and "
        assert_eq!(lines[2].grapheme_start, 16);
        assert_eq!(lines[2].grapheme_end, 20); // "more"
    }

    #[rstest::rstest]
    fn non_whitespace_is_not_detected_as_whitespace() {
        // Given a long word with no spaces.
        let text = "abcdefghij";

        // When wrapping at width 5.
        let lines = wrap_text(text, 5);

        // Then it breaks at exact width (forced break, no word boundary).
        // This verifies that whitespace detection is correct.
        assert_eq!(lines.len(), 2);
        assert!(lines[1].is_continuation);
    }

    //
    // `ChatInputBoxState::wrapped_lines()` is memoized; this confirms the memo's output is
    // byte-identical to a fresh `wrap_text(buffer, width)` call across a representative
    // corpus. The memo must never diverge from the underlying computation.

    #[rstest::rstest]
    #[case::empty("", 20)]
    #[case::short("hello", 20)]
    #[case::wraps_word_boundary("hello world and more", 10)]
    #[case::overflow_long_word("abcdefghijklmn", 5)]
    #[case::newlines("hello\nworld", 80)]
    #[case::wrap_then_newline("hello world and more\nshort", 10)]
    #[case::unicode_emoji("🎉🎊🎈🎁🎁🎊🎈🎉", 5)]
    #[case::cjk("中文测试字符", 10)]
    #[case::mixed_ascii_cjk("hello中文", 9)]
    #[case::trailing_newline("hello\n", 80)]
    #[case::exact_width("hello", 5)]
    fn wrapped_lines_memo_matches_fresh_wrap_text(#[case] text: &str, #[case] width: usize) {
        // Given a ChatInputBoxState loaded with `text` at the given width.
        use crate::chat_input_state::ChatInputBoxState;
        let mut state = ChatInputBoxState::new();
        state.set_wrap_width(width);
        state.insert_text(text);

        // When fetching memoized wrapped_lines.
        let memoized = state.wrapped_lines();
        // And computing fresh from wrap_text.
        let fresh = wrap_text(text, width);

        // Then they are byte-identical (grapheme ranges, continuation flags, logical indices).
        assert_eq!(
            memoized.len(),
            fresh.len(),
            "line count mismatch for {text:?}@{width}"
        );
        for (i, (m, f)) in memoized.iter().zip(fresh.iter()).enumerate() {
            assert_eq!(
                m.grapheme_start, f.grapheme_start,
                "start mismatch line {i}"
            );
            assert_eq!(m.grapheme_end, f.grapheme_end, "end mismatch line {i}");
            assert_eq!(
                m.is_continuation, f.is_continuation,
                "continuation mismatch line {i}"
            );
            assert_eq!(
                m.logical_line_index, f.logical_line_index,
                "logical_line_index mismatch line {i}"
            );
        }
    }
}
