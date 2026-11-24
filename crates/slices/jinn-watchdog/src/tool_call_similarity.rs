//! Bounded similarity for the cross-call tool-call watchdogs.
//!
//! A pure module: no bus, no actor, no state. The watchdog actors hand it
//! two argument strings or two result strings and get a normalized
//! similarity in `0.0..=1.0`, or a byte-equality verdict, and a length
//! gate that lets a long argument skip the fuzzy comparison entirely.
//!
//! The comparisons are deliberately cheap and self-contained:
//!
//! - [`normalize`] collapses whitespace so a call whose only difference is
//!   line-wrapping still compares equal to its predecessor.
//! - [`similarity_ratio`] is the normalized Levenshtein ratio over Unicode
//!   scalar values — never bytes — so multi-byte characters do not inflate
//!   the distance the way a byte-wise count would.
//! - [`result_prefix`] bounds the result comparison to a fixed prefix, so
//!   a 50 KB tool output costs the same as a one-line one.
//!
//! There is no fuzzy-match crate here by design: `fuzzy-matcher` (the
//! workspace's SkimMatcherV2) answers "does a query appear as a
//! subsequence of a candidate", which is the wrong question for
//! "are two calls near-duplicates of each other".

/// The result prefix compared by both watchdogs, in characters. A tool
/// output past this point adds nothing to the verdict and could dominate
/// the edit distance with boilerplate.
pub const RESULT_PREFIX_CHARS: usize = 1024;

/// Collapses runs of whitespace to single spaces and trims the ends.
///
/// Both watchdogs compare normalized text, so a command re-wrapped across
/// lines is the same command.
#[must_use]
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_whitespace = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            in_whitespace = !out.is_empty();
        } else {
            if in_whitespace {
                out.push(' ');
                in_whitespace = false;
            }
            out.push(ch);
        }
    }
    out
}

/// The bounded result prefix: the first [`RESULT_PREFIX_CHARS`] Unicode
/// scalar values of the normalized result text.
#[must_use]
pub fn result_prefix(result_content: &str) -> String {
    let normalized = normalize(result_content);
    normalized.chars().take(RESULT_PREFIX_CHARS).collect()
}

/// The normalized Levenshtein similarity of two strings: `1 −
/// edit_distance / max(len_a, len_b)`, over Unicode scalar values.
///
/// Two empty strings are identical (1.0). The score is symmetric and
/// bounded in `0.0..=1.0`.
#[must_use]
pub fn similarity_ratio(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let denominator = a.len().max(b.len());
    let distance = levenshtein_distance(&a, &b);
    1.0 - (distance as f32) / (denominator as f32)
}

/// True when two normalized texts compare equal byte-for-byte.
#[must_use]
pub fn exact_match(a: &str, b: &str) -> bool {
    a == b
}

/// Wagner-Fischer edit distance over two full rows. No indexing: each
/// row is built by zipping character iterators with the previous row's
/// `windows(2)` values, so the workspace's no-panic-indexing gate holds
/// without `.get()` ceremony.
fn levenshtein_distance(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut previous_row: Vec<usize> = (0..=b.len()).collect();
    for (i, ach) in a.iter().enumerate() {
        let row: Vec<usize> = std::iter::once(i + 1)
            .chain(
                previous_row
                    .windows(2)
                    .zip(b.iter())
                    .map(|(pair, bch)| {
                        // `windows(2)` yields exactly two elements; take
                        // the pair as (diagonal, up).
                        let diagonal = pair.first().copied().unwrap_or_default();
                        let up = pair.last().copied().unwrap_or_default();
                        // `up` is the cell above plus one deletion.
                        up.saturating_add(1).min(diagonal + usize::from(ach != bch))
                    })
                    .scan(i + 1, |left, candidate| {
                        let value = candidate.min(*left + 1);
                        *left = value;
                        Some(value)
                    }),
            )
            .collect();
        previous_row = row;
    }
    previous_row.pop().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::float_cmp,
        reason = "test code; exact f32 constants asserted"
    )]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn normalize_collapses_whitespace_runs() {
        // Given text with mixed whitespace.
        let text = "  grep  -n\t\t\"pat\"\n  file.rs  ";

        // When normalizing.
        let out = normalize(text);

        // Then runs collapse to single spaces and the ends are trimmed.
        assert_eq!(out, "grep -n \"pat\" file.rs");
    }

    #[rstest::rstest]
    #[test]
    fn identical_text_after_normalization_scores_one() {
        // Given two spacings of the same command.
        let a = normalize("grep -n \"pat\" f.rs");
        let b = normalize("grep -n\t\"pat\"  f.rs");

        // Then similarity is exactly 1.0.
        assert!((similarity_ratio(&a, &b) - 1.0).abs() < f32::EPSILON);
    }

    #[rstest::rstest]
    #[test]
    fn two_empty_strings_score_one() {
        assert!((similarity_ratio("", "") - 1.0).abs() < f32::EPSILON);
    }

    #[rstest::rstest]
    #[test]
    fn disjoint_strings_score_zero() {
        // Given two strings with no characters in common.
        let score = similarity_ratio("aaaa", "bbbb");

        // Then similarity is 0.0.
        assert!((score - 0.0).abs() < f32::EPSILON);
    }

    #[rstest::rstest]
    #[test]
    fn one_appended_word_keeps_high_similarity() {
        // Given the loop's signature: the same command with a pipe added.
        let a = normalize("grep -n \"lrow\" crates/slices/jinn-webui/src/regions/sessions.rs");
        let b =
            normalize("grep -n \"lrow\" crates/slices/jinn-webui/src/regions/sessions.rs | wc -l");

        // Then the similarity stays above the shipped 0.85 threshold.
        assert!(similarity_ratio(&a, &b) >= 0.85);
    }

    #[rstest::rstest]
    #[test]
    fn similarity_is_symmetric() {
        // Given two different commands.
        let a = "grep -rn \"\\.lrow\" src";
        let b = "grep -n \"\\.srow {\" src";

        // Then the score does not depend on the argument order.
        let ab = similarity_ratio(a, b);
        let ba = similarity_ratio(b, a);
        assert!((ab - ba).abs() < 1e-6);
    }

    #[rstest::rstest]
    #[test]
    fn multibyte_characters_count_once_per_scalar() {
        // Given two two-character strings, one carrying a multi-byte
        // character.
        let score = similarity_ratio("éx", "ex");

        // Then the distance is one scalar over a denominator of two, so
        // similarity is 0.5 — a byte-wise count would score the é as two
        // substitutions in three bytes and read the pair as farther apart.
        assert!((score - 0.5).abs() < 1e-6);
    }

    #[rstest::rstest]
    #[test]
    fn result_prefix_is_bounded() {
        // Given a result far longer than the bound.
        let content = "x".repeat(RESULT_PREFIX_CHARS * 4);

        // When taking the prefix.
        let prefix = result_prefix(&content);

        // Then it is exactly the bound long.
        assert_eq!(prefix.chars().count(), RESULT_PREFIX_CHARS);
    }

    #[rstest::rstest]
    #[test]
    fn result_prefix_normalizes_before_bounding() {
        // Given a result whose early bytes are whitespace.
        let content = format!("{}x", "\n\t ".repeat(RESULT_PREFIX_CHARS));

        // When taking the prefix.
        let prefix = result_prefix(&content);

        // Then the whitespace collapsed first, so the single meaningful
        // character survived the bound.
        assert_eq!(prefix, "x");
    }

    #[rstest::rstest]
    #[test]
    fn exact_match_is_byte_equality_on_normalized_text() {
        // Given two spacings of the same command.
        let a = normalize("just test");
        let b = normalize("just  test");

        // Then the exact verdict is true.
        assert!(exact_match(&a, &b));

        // And a real difference is false.
        assert!(!exact_match(&a, &normalize("just lint")));
    }
}
