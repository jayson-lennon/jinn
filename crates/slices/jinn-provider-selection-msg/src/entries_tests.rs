//! The provider entry's row rendering tests: the `PickerItem`/`TreeItem`
//! rendering, markers, and highlight mapping. The row *data* builder
//! (loading, sorting) is tested in `provider_rows_tests` beside it.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_selection_widget::PickerItem;
use jinn_theme::default_theme;
use std::ops::Range;

use crate::entries::ProviderPickerEntry;

fn make_picker_entry(
    model: &str,
    provider_name: &str,
    is_available: bool,
    is_alias: bool,
) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: format!("{provider_name}/{model}"),
        name: provider_name.to_owned(),
        provider_name: provider_name.to_owned(),
        backend: "test".to_owned(),
        model: model.to_owned(),
        search_text: format!("{model} {provider_name}"),
        is_alias,
        alias_target: None,
        is_available,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: default_theme(),
    }
}

#[rstest::rstest]
fn render_row_with_empty_match_indices_same_as_render_row() {
    // Given a provider entry.
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When rendering with and without match indices.
    let normal = entry.render_row(false);
    let highlighted = entry.render_row_with_highlight(false, &[]);

    // Then the output is identical.
    assert_eq!(normal.spans.len(), highlighted.spans.len());
    for (n, h) in normal.spans.iter().zip(highlighted.spans.iter()) {
        assert_eq!(n.content, h.content);
        assert_eq!(n.style, h.style);
    }
}

#[rstest::rstest]
fn provider_highlight_row_contains_matched_char() {
    // Given a provider entry with model "llama3".
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When highlighting with match at byte 0 (the "l").
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "genuinely want a slice containing one Range<usize>"
    )]
    let highlights: &[Range<usize>] = &[0..1];
    let line = entry.render_row_with_highlight(false, highlights);

    // Then the row text contains the matched character.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(text.contains('l'), "highlighted row should contain 'l'");
}

#[rstest::rstest]
fn render_row_with_highlight_preserves_provider_name_suffix() {
    // Given a provider entry with model "gpt-4" and provider "openrouter".
    let entry = make_picker_entry("gpt-4", "openrouter", true, false);

    // When highlighting with match at byte 0.
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "genuinely want a slice containing one Range<usize>"
    )]
    let highlights: &[Range<usize>] = &[0..1];
    let line = entry.render_row_with_highlight(false, highlights);

    // Then the full text still contains provider name.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(text.contains("openrouter"), "should contain provider name");
    assert!(text.contains("gpt-4"), "should contain model");
}

#[rstest::rstest]
fn display_label_includes_provider_name() {
    // Given a ProviderPickerEntry with model "glm5.1" and provider_name "zai".
    let entry = make_picker_entry("glm5.1", "zai", true, false);

    // When reading its display label.
    // Then display_label contains both model and provider name.
    assert!(entry.display_label().contains("glm5.1"));
    assert!(entry.display_label().contains("zai"));
}

#[rstest::rstest]
fn display_label_allows_searching_by_provider_name() {
    // Given two entries with different providers.
    let zai_entry = make_picker_entry("glm5.1", "zai", true, false);
    let openrouter_entry = make_picker_entry("gpt-4o", "openrouter", true, false);

    // When reading both display labels.
    // Then the zai entry's display_label contains "zai".
    assert!(zai_entry.display_label().contains("zai"));
    // And the openrouter entry's display_label does NOT contain "zai".
    assert!(!openrouter_entry.display_label().contains("zai"));
}

#[rstest::rstest]
fn provider_name_match_appears_in_highlighted_row() {
    // Given a provider entry with model "llama3" and provider "ollama".
    let entry = make_picker_entry("llama3", "ollama", true, false);

    // When highlighting with match in the provider-name portion (byte 7..13 = "ollama").
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "API takes a slice of ranges"
    )]
    let line = entry.render_row_with_highlight(false, &[7..13]);

    // Then the row text contains the provider name characters.
    let text: String = line.spans.iter().map(|s| &*s.content).collect();
    assert!(
        text.contains('o'),
        "highlighted row should contain 'o' from provider name"
    );
}
