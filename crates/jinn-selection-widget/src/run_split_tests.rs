//! Tests for the shared matched-run splitting helper.

use super::*;

#[rstest::rstest]
#[test]
fn split_of_empty_match_returns_one_plain_run() {
    // Given a label and no matched offsets.
    // When splitting into runs.
    let runs = split_matched_runs("sample model", &[]);

    // Then one plain run carries the whole label.
    assert_eq!(
        runs,
        vec![TextRun {
            text: "sample model".to_owned(),
            matched: false,
        }]
    );
}

#[rstest::rstest]
#[test]
fn split_of_empty_text_returns_one_plain_run() {
    // Given empty text and a match offset.
    // When splitting.
    let runs = split_matched_runs("", &[0]);

    // Then one empty plain run is returned (nothing to highlight).
    assert_eq!(
        runs,
        vec![TextRun {
            text: String::new(),
            matched: false,
        }]
    );
}

#[rstest::rstest]
#[test]
fn split_marks_only_the_matched_run() {
    // Given a match on the middle bytes of the label.
    // When splitting.
    let runs = split_matched_runs("sample model", &[7, 8]);

    // Then three runs: plain prefix, matched middle, plain suffix.
    assert_eq!(
        runs,
        vec![
            TextRun {
                text: "sample ".to_owned(),
                matched: false,
            },
            TextRun {
                text: "mo".to_owned(),
                matched: true,
            },
            TextRun {
                text: "del".to_owned(),
                matched: false,
            },
        ]
    );
}

#[rstest::rstest]
#[test]
fn split_marks_leading_and_trailing_runs() {
    // Given matches at the very start and very end.
    // When splitting.
    let runs = split_matched_runs("abc", &[0, 2]);

    // Then the first run is matched with no plain prefix, and the last
    // is matched with no plain suffix.
    assert_eq!(
        runs,
        vec![
            TextRun {
                text: "a".to_owned(),
                matched: true,
            },
            TextRun {
                text: "b".to_owned(),
                matched: false,
            },
            TextRun {
                text: "c".to_owned(),
                matched: true,
            },
        ]
    );
}

#[rstest::rstest]
#[test]
fn split_adjacent_matches_join_into_one_run() {
    // Given consecutive matched offsets (no gap).
    // When splitting.
    let runs = split_matched_runs("abcd", &[0, 1, 2, 3]);

    // Then one matched run spans the whole label.
    assert_eq!(
        runs,
        vec![TextRun {
            text: "abcd".to_owned(),
            matched: true,
        }]
    );
}

#[rstest::rstest]
#[test]
fn split_is_grapheme_safe_on_multibyte_text() {
    // Given matched offsets on three multi-byte graphemes of one label
    // (é is two bytes; the emoji is four; each offset names the byte
    // where that grapheme starts).
    // When splitting at those boundaries.
    let runs = split_matched_runs("émile 🚀 model", &[0, 7]);

    // Then runs cut on grapheme boundaries: "é" (matched), "mile "
    // (plain), the emoji (matched), " model" (plain).
    assert_eq!(
        runs,
        vec![
            TextRun {
                text: "é".to_owned(),
                matched: true,
            },
            TextRun {
                text: "mile ".to_owned(),
                matched: false,
            },
            TextRun {
                text: "🚀".to_owned(),
                matched: true,
            },
            TextRun {
                text: " model".to_owned(),
                matched: false,
            },
        ]
    );
}

#[rstest::rstest]
#[test]
fn split_absorbs_offsets_inside_a_grapheme_into_the_plain_run() {
    // Given a matched offset that falls inside a multi-byte grapheme
    // (byte 1 is the combining part of "é") rather than its boundary.
    // When splitting.
    let runs = split_matched_runs("éx", &[1]);

    // Then the offset matches no grapheme boundary; it is absorbed and
    // the whole label stays one plain run.
    assert_eq!(
        runs,
        vec![TextRun {
            text: "éx".to_owned(),
            matched: false,
        }]
    );
}

#[rstest::rstest]
#[test]
fn split_ignores_offsets_beyond_the_text() {
    // Given an offset past the end (a stale index into a longer text).
    // When splitting.
    let runs = split_matched_runs("ab", &[1, 99]);

    // Then only the in-bounds offset takes effect.
    assert_eq!(
        runs,
        vec![
            TextRun {
                text: "a".to_owned(),
                matched: false,
            },
            TextRun {
                text: "b".to_owned(),
                matched: true,
            },
        ]
    );
}

#[rstest::rstest]
#[test]
fn split_returns_plain_run_when_no_offset_is_a_grapheme_boundary() {
    // Given only an unalignable offset.
    // When splitting.
    let runs = split_matched_runs("é", &[1]);

    // Then one plain run.
    assert_eq!(
        runs,
        vec![TextRun {
            text: "é".to_owned(),
            matched: false,
        }]
    );
}
