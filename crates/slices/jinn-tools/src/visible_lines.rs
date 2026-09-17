//! Shared visible-line splitting for text tools.
//!
//! Splits content into the lines a reader perceives: a trailing newline does
//! not produce an extra empty final line, and empty content yields no lines.
//! Used by `read` (numbering output) and `edit` (changed-region snippets) so
//! both tools agree on line geometry.

/// Returns the visible (perceived) lines of `text`.
///
/// A text ending in `\n` does not include a trailing empty line; empty text
/// yields an empty vec.
pub fn get_visible_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines
            .get(..lines.len().saturating_sub(1))
            .map(<[&str]>::to_vec)
            .unwrap_or(lines)
    } else {
        lines
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
    fn empty_text_yields_no_lines() {
        // Given empty text.
        // When splitting into visible lines.
        // Then there are none.
        assert!(get_visible_lines("").is_empty());
    }

    #[rstest::rstest]
    fn trailing_newline_does_not_add_empty_line() {
        // Given text with a terminal newline.
        // When splitting into visible lines.
        // Then the trailing empty element is dropped.
        assert_eq!(get_visible_lines("a\nb\n"), vec!["a", "b"]);
    }

    #[rstest::rstest]
    fn text_without_terminal_newline_keeps_last_line() {
        // Given text without a terminal newline.
        // When splitting into visible lines.
        // Then all lines are kept.
        assert_eq!(get_visible_lines("a\nb"), vec!["a", "b"]);
    }
}
