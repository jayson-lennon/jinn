//! Tests for the shared persona rows — the vocabulary both picker
//! surfaces consume.

use jinn_slices::Persona;

use super::filter_rows;
use super::PersonaRow;

/// Two personas, the first active.
fn fixture() -> Vec<Persona> {
    vec![
        Persona {
            name: "coding-assistant".to_owned(),
            description: "writes code".to_owned(),
            body: String::new(),
            handoff_prompt: "<compaction>x</compaction>".to_owned(),
        },
        Persona {
            name: "learning-tutor".to_owned(),
            description: "teaches".to_owned(),
            body: String::new(),
            handoff_prompt: String::new(),
        },
    ]
}

#[rstest::rstest]
#[test]
fn rows_marks_the_active_persona_and_keeps_the_cell_order() {
    // Given two personas with the second one active.
    let personas = fixture();

    // When building rows marking "learning-tutor".
    let rows = PersonaRow::rows(&personas, Some("learning-tutor"));

    // Then the order is the input's (the cell's name sort) and the mark
    // landed on the right row.
    assert_eq!(rows[0].name, "coding-assistant");
    assert!(!rows[0].is_active);
    assert!(rows[1].is_active);
}

#[rstest::rstest]
#[test]
fn search_text_is_name_then_description() {
    // Given a row built from a persona.
    let row = &PersonaRow::rows(&fixture(), None)[0];

    // Then its search text is the space-joined fields — the shape the
    // field split's offset contract relies on.
    assert_eq!(row.search_text(), "coding-assistant writes code");
}

#[rstest::rstest]
#[test]
fn empty_filter_returns_the_rows_unchanged_with_no_ranges() {
    // Given built rows and an empty filter.
    let rows = PersonaRow::rows(&fixture(), None);

    // When filtering with the empty string.
    let filtered = filter_rows(&rows, "");

    // Then every row comes back in order with empty match indices.
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].row.name, "coding-assistant");
    assert!(filtered.iter().all(|f| f.match_indices.is_empty()));
}

#[rstest::rstest]
#[test]
fn filter_matches_across_both_fields_by_fuzzy_terms() {
    // Given built rows.
    let rows = PersonaRow::rows(&fixture(), None);

    // When filtering with a term matching only the description field.
    let filtered = filter_rows(&rows, "teaches");

    // Then the tutor row wins with indices inside the description field.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].row.name, "learning-tutor");
    let name_len = "learning-tutor".len();
    assert!(
        filtered[0].match_indices.iter().all(|&i| i > name_len),
        "indices live in the description field: {:?}",
        filtered[0].match_indices
    );
}

#[rstest::rstest]
#[test]
fn segments_split_the_fields_at_the_separator() {
    // Given a row and match indices straddling both fields — 0,1,2 (the
    // leading "cod") in the name, name_len+1 (the 'w') and name_len+11
    // (the final 'e'; the description starts at name_len+1). Hand-picked
    // so the test pins the split contract, not the matcher's alignment
    // preferences (the filter tests cover those).
    let row = &PersonaRow::rows(&fixture(), None)[0];
    let name_len = row.name.len();
    let indices = vec![0, 1, 2, name_len + 1, name_len + 11];

    // When splitting the fields.
    let segments = row.segments(&indices);

    // Then the name runs reconstruct the name and the description runs
    // reconstruct the description — the separator belongs to neither.
    let name: String = segments.name.iter().map(|s| s.text.as_str()).collect();
    let description: String = segments
        .description
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(name, "coding-assistant");
    assert_eq!(description, "writes code");
    // And the matched runs carry the mark on both sides of the split.
    assert!(
        segments.name.iter().any(|s| s.matched && s.text == "cod"),
        "the name's leading 'cod' is one marked run: {segments:?}"
    );
    assert!(
        segments
            .description
            .iter()
            .any(|s| s.matched && s.text == "w"),
        "the description's 'w' is marked: {segments:?}"
    );
    assert!(
        segments
            .description
            .iter()
            .any(|s| s.matched && s.text == "e"),
        "the description's trailing 'e' is marked: {segments:?}"
    );
}

#[rstest::rstest]
#[test]
fn segments_without_a_filter_are_one_plain_run_per_field() {
    // Given rows filtered by the empty string.
    let rows = PersonaRow::rows(&fixture(), None);
    let filtered = &filter_rows(&rows, "")[0];

    // When splitting the fields.
    let segments = filtered.segments();

    // Then each field is a single unmarked run.
    assert_eq!(segments.name.len(), 1);
    assert!(!segments.name[0].matched);
    assert_eq!(segments.description.len(), 1);
    assert!(!segments.description[0].matched);
}
