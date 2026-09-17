//! Edit engine — exact string replacement on LF-normalized content.
//!
//! The engine is deliberately thin: count occurrences, replace exactly
//! (first-only or all), and compute the changed line range so the response
//! can include a cat -n snippet of the edited region. Byte-exactness is the
//! contract — there is no fuzzy or whitespace-flexible matching, so a stale
//! `old_string` fails loudly instead of applying to a near-miss location.

// ─── Occurrence counting ────────────────────────────────────────────────

/// Counts non-overlapping occurrences of `needle` in `content`.
///
/// Non-overlapping matches Claude Code semantics: `aa` in `aaa` counts as 1.
pub fn count_occurrences(content: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    content.split(needle).count().saturating_sub(1)
}

// ─── Replacement ────────────────────────────────────────────────────────

/// Replaces occurrences of `old_string` with `new_string`.
///
/// Replaces every occurrence when `replace_all` is true, otherwise only the
/// first. The caller guarantees at least one occurrence exists and, when
/// `replace_all` is false, that it is unique.
#[must_use]
pub fn replace_exact(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> String {
    if old_string.is_empty() || old_string == new_string {
        return content.to_owned();
    }
    if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    }
}

// ─── Errors ─────────────────────────────────────────────────────────────

/// Builds the error returned when `old_string` is not present in the file.
#[must_use]
pub fn not_found_error(old_string: &str) -> String {
    format!(
        "[E_NOT_FOUND] old_string not found in file: {:?}. \
         Use `read` to verify the current content and match it exactly, \
         including whitespace and indentation.",
        preview(old_string)
    )
}

/// Builds the error returned when `old_string` is not unique and
/// `replace_all` is false.
#[must_use]
pub fn not_unique_error(old_string: &str, occurrences: usize) -> String {
    format!(
        "[E_NOT_UNIQUE] old_string appears {occurrences} times: {:?}. \
         Provide a larger string with more surrounding context to make it \
         unique, or set `replace_all` to true to replace every occurrence.",
        preview(old_string)
    )
}

/// One-line preview of a matched string for error messages.
fn preview(s: &str) -> String {
    if s.chars().count() <= 40 {
        return s.replace('\n', "\\n");
    }
    let truncated: String = s.chars().take(37).collect();
    format!("{truncated}...")
}

// ─── Changed-range computation ──────────────────────────────────────────

/// Computes the first and last changed line numbers between original and result.
///
/// Lines are 1-indexed over the *result* content; `(None, None)` means the
/// texts are identical. The comparison is byte-based, so content earlier in
/// the file that happens to be identical still counts as unchanged — the
/// changed range is the minimal span of differing bytes converted to lines.
pub fn compute_changed_line_range(original: &str, result: &str) -> (Option<usize>, Option<usize>) {
    if original == result {
        return (None, None);
    }

    // Find first differing byte.
    let min_len = original.len().min(result.len());
    let mut first_diff = 0;
    while first_diff < min_len
        && original.as_bytes().get(first_diff) == result.as_bytes().get(first_diff)
    {
        first_diff += 1;
    }

    // Find last differing byte.
    let mut last_orig = original.len() as isize - 1;
    let mut last_res = result.len() as isize - 1;
    while last_orig >= first_diff as isize
        && last_res >= first_diff as isize
        && original.as_bytes().get(last_orig as usize) == result.as_bytes().get(last_res as usize)
    {
        last_orig -= 1;
        last_res -= 1;
    }

    let first_line = byte_to_line(first_diff + 1, result);
    let last_line = if last_res < first_diff as isize {
        visible_line_count(result)
    } else {
        byte_to_line((last_res + 1) as usize, result)
    };

    (Some(first_line), Some(last_line.max(1)))
}

/// Number of visible lines — a trailing newline does not add an empty line.
fn visible_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let count = text.split('\n').count();
    if text.ends_with('\n') {
        count - 1
    } else {
        count
    }
}

/// Converts a 1-indexed byte position to the 1-indexed line containing it.
fn byte_to_line(byte_pos: usize, text: &str) -> usize {
    let mut line = 1;
    for (i, b) in text.bytes().enumerate() {
        if i >= byte_pos.saturating_sub(1) {
            break;
        }
        if b == b'\n' {
            line += 1;
        }
    }
    line
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
    fn counts_unique_and_repeated_occurrences() {
        // Given content with one and two occurrences.
        // When counting.
        // Then counts are exact.
        assert_eq!(count_occurrences("hello world", "world"), 1);
        assert_eq!(count_occurrences("a a a", "a"), 3);
    }

    #[rstest::rstest]
    fn counts_non_overlapping_matches() {
        // Given overlapping candidate matches.
        // When counting "aa" in "aaa".
        // Then only the non-overlapping match counts.
        assert_eq!(count_occurrences("aaa", "aa"), 1);
    }

    #[rstest::rstest]
    fn empty_needle_counts_zero() {
        // Given an empty needle.
        // When counting.
        // Then nothing is counted.
        assert_eq!(count_occurrences("abc", ""), 0);
    }

    #[rstest::rstest]
    fn replace_first_only_by_default() {
        // Given two occurrences.
        // When replacing without replace_all.
        // Then only the first changes.
        assert_eq!(replace_exact("x x", "x", "y", false), "y x");
    }

    #[rstest::rstest]
    fn replace_all_swaps_every_occurrence() {
        // Given two occurrences.
        // When replacing with replace_all.
        // Then both change.
        assert_eq!(replace_exact("x x", "x", "y", true), "y y");
    }

    #[rstest::rstest]
    fn identical_strings_return_content_unchanged() {
        // Given old and new equal.
        // When replacing.
        // Then content is returned as-is.
        assert_eq!(replace_exact("abc", "b", "b", false), "abc");
    }

    #[rstest::rstest]
    fn changed_range_is_minimal_line_span() {
        // Given a replacement on line 2 of 3.
        // When computing the changed range.
        // Then it is (2, 2).
        assert_eq!(
            compute_changed_line_range("a\nb\nc\n", "a\nB\nc\n"),
            (Some(2), Some(2))
        );
    }

    #[rstest::rstest]
    fn identical_texts_have_no_changed_range() {
        // Given identical texts.
        // When computing the changed range.
        // Then there is none.
        assert_eq!(compute_changed_line_range("a\nb", "a\nb"), (None, None));
    }

    #[rstest::rstest]
    fn appended_content_extends_range_to_new_last_line() {
        // Given a replacement that appends lines.
        // When computing the changed range.
        // Then the range ends at the new last line.
        assert_eq!(
            compute_changed_line_range("a\n", "a\nb\nc\n"),
            (Some(2), Some(3))
        );
    }

    #[rstest::rstest]
    fn not_found_error_names_read() {
        // Given a missing string.
        // When building the error.
        // Then it instructs a re-read.
        assert!(not_found_error("zzz").contains("E_NOT_FOUND"));
        assert!(not_found_error("zzz").contains("read"));
    }

    #[rstest::rstest]
    fn not_unique_error_reports_count() {
        // Given a duplicated string.
        // When building the error.
        // Then the occurrence count is reported.
        let err = not_unique_error("dup", 3);
        assert!(err.contains("E_NOT_UNIQUE"));
        assert!(err.contains("3 times"));
    }
}
