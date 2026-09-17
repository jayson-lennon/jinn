//! Line ending detection, normalization, and BOM handling for the edit tool.
//!
//! When editing files, we must preserve the original line endings (LF or CRLF)
//! and UTF-8 BOM. This module provides utilities to:
//! 1. Strip BOM before matching
//! 2. Detect the dominant line ending
//! 3. Normalize to LF for matching
//! 4. Restore original line endings after editing

/// The dominant line ending style in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// Unix-style `\n`
    Lf,
    /// Windows-style `\r\n`
    Crlf,
}

/// Strips a UTF-8 BOM (byte order mark) from the beginning of content.
///
/// Returns `(content_without_bom, Some(bom))` if BOM was present,
/// or `(content, None)` if no BOM.
pub fn strip_bom(content: &str) -> (&str, Option<&str>) {
    if let Some(stripped) = content.strip_prefix('\u{feff}') {
        (stripped, content.get(..3))
    } else {
        (content, None)
    }
}

/// Detects the dominant line ending in the content.
///
/// Returns `None` if the content has no line endings.
/// Returns `Some(LineEnding::Lf)` if only LF is present (or mixed with majority LF).
/// Returns `Some(LineEnding::Crlf)` if CRLF is present and dominates.
pub fn detect_line_ending(content: &str) -> Option<LineEnding> {
    let crlf_count = content.matches("\r\n").count();
    // Count standalone LF (not preceded by CR)
    let lf_count = content.matches('\n').count() - crlf_count;

    if crlf_count == 0 && lf_count == 0 {
        return None;
    }

    if crlf_count > lf_count {
        Some(LineEnding::Crlf)
    } else {
        Some(LineEnding::Lf)
    }
}

/// Normalizes all line endings to LF (`\n`) for matching purposes.
pub fn normalize_to_lf(content: &str) -> String {
    content.replace("\r\n", "\n")
}

/// Restores line endings from LF back to the original style.
///
/// If `ending` is `None` or `LineEnding::Lf`, returns content as-is.
/// If `LineEnding::Crlf`, converts all `\n` to `\r\n`.
pub fn restore_line_endings(content: &str, ending: Option<LineEnding>) -> String {
    match ending {
        Some(LineEnding::Crlf) => content.replace('\n', "\r\n"),
        _ => content.to_owned(),
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
    fn strip_bom_removes_bom() {
        // Given content with a UTF-8 BOM.
        let content = "\u{feff}hello";

        // When stripping BOM.
        let (stripped, bom) = strip_bom(content);

        // Then BOM is removed.
        assert_eq!(stripped, "hello");
        assert_eq!(bom, Some("\u{feff}"));
    }

    #[rstest::rstest]
    fn strip_bom_no_bom() {
        // Given content without BOM.
        let content = "hello";

        // When stripping BOM.
        let (stripped, bom) = strip_bom(content);

        // Then content is unchanged.
        assert_eq!(stripped, "hello");
        assert!(bom.is_none());
    }

    #[rstest::rstest]
    fn detect_line_ending_lf() {
        // Given content with LF line endings.
        let content = "line1\nline2\nline3";

        // When detecting line ending.
        let ending = detect_line_ending(content);

        // Then LF is detected.
        assert_eq!(ending, Some(LineEnding::Lf));
    }

    #[rstest::rstest]
    fn detect_line_ending_crlf() {
        // Given content with CRLF line endings.
        let content = "line1\r\nline2\r\nline3";

        // When detecting line ending.
        let ending = detect_line_ending(content);

        // Then CRLF is detected.
        assert_eq!(ending, Some(LineEnding::Crlf));
    }

    #[rstest::rstest]
    fn detect_line_ending_no_newlines() {
        // Given content with no line endings.
        let content = "no newlines here";

        // When detecting line ending.
        let ending = detect_line_ending(content);

        // Then None is returned.
        assert!(ending.is_none());
    }

    #[rstest::rstest]
    fn detect_line_ending_mixed_majority_crlf() {
        // Given content with mixed line endings where CRLF dominates.
        let content = "line1\r\nline2\nline3\r\n";

        // When detecting line ending.
        let ending = detect_line_ending(content);

        // Then CRLF is detected.
        assert_eq!(ending, Some(LineEnding::Crlf));
    }

    #[rstest::rstest]
    fn normalize_to_lf_converts_crlf() {
        // Given content with CRLF line endings.
        let content = "line1\r\nline2\r\n";

        // When normalizing to LF.
        let normalized = normalize_to_lf(content);

        // Then all CRLF are converted to LF.
        assert_eq!(normalized, "line1\nline2\n");
    }

    #[rstest::rstest]
    fn normalize_to_lf_preserves_lf() {
        // Given content with LF line endings.
        let content = "line1\nline2\n";

        // When normalizing to LF.
        let normalized = normalize_to_lf(content);

        // Then content is unchanged.
        assert_eq!(normalized, "line1\nline2\n");
    }

    #[rstest::rstest]
    fn restore_line_endings_to_crlf() {
        // Given LF content.
        let content = "line1\nline2\n";

        // When restoring to CRLF.
        let restored = restore_line_endings(content, Some(LineEnding::Crlf));

        // Then LF is converted to CRLF.
        assert_eq!(restored, "line1\r\nline2\r\n");
    }

    #[rstest::rstest]
    fn restore_line_endings_to_lf() {
        // Given LF content.
        let content = "line1\nline2\n";

        // When restoring to LF (no-op).
        let restored = restore_line_endings(content, Some(LineEnding::Lf));

        // Then content is unchanged.
        assert_eq!(restored, "line1\nline2\n");
    }

    #[rstest::rstest]
    fn restore_line_endings_none() {
        // Given LF content and no ending.
        let content = "line1\nline2\n";

        // When restoring with None.
        let restored = restore_line_endings(content, None);

        // Then content is unchanged.
        assert_eq!(restored, "line1\nline2\n");
    }
}
