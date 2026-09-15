//! Provider picker entry type and rendering.

use std::ops::Range;

use crate::feat::picker::style::selected_style;
use crate::feat::theme::Theme;
use jinn_selection_widget::PickerItem;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// A provider entry ready for display in the picker.
#[derive(Debug, Clone)]
pub struct ProviderPickerEntry {
    /// Full provider ID in `{name}/{model}` format (e.g., `"ollama/llama3"`).
    /// For aliases, this is the resolved target's full ID.
    /// For remote entries, this is `{provider_name}/{model}`.
    pub provider_id: String,
    /// Display name for the entry (provider block name or alias name).
    pub name: String,
    /// Provider block name (e.g., `"ollama"`). Used for display.
    pub provider_name: String,
    /// Backend type string.
    pub backend: String,
    /// Model identifier (primary display text).
    pub model: String,
    /// Searchable text combining model name and provider name for fuzzy matching.
    /// Format: `"{model} {provider_name}"`. Used as [`display_label`](PickerItem::display_label).
    pub search_text: String,
    /// Whether this entry is an alias.
    pub is_alias: bool,
    /// Alias display target (e.g., `"ollama/llama3"`). Only set for aliases.
    pub alias_target: Option<String>,
    /// Whether this provider is available (API key present or keyless).
    pub is_available: bool,
    /// Whether this entry was discovered from a remote provider (not in static config).
    pub is_remote: bool,
    /// Whether this entry is the currently active provider.
    pub is_active: bool,
    /// Whether this entry has been selected (checked) for multi-select alloy building.
    pub selected: bool,
    /// Theme for rendering.
    pub theme: Theme,
}

impl PickerItem for ProviderPickerEntry {
    fn display_label(&self) -> &str {
        &self.search_text
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_provider_row(self, is_selected, &[])
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        render_provider_row(self, is_selected, match_indices)
    }
}

/// Renders a provider picker row, optionally highlighting matched characters.
///
/// Match indices are byte offsets into `entry.search_text` (the `display_label`),
/// which has the format `"{model} {provider_name}"`. Indices are split into model
/// and provider-name portions so both can be independently highlighted in the
/// rendered row.
fn render_provider_row(
    entry: &ProviderPickerEntry,
    is_selected: bool,
    match_indices: &[Range<usize>],
) -> Line<'static> {
    let selection_marker = if entry.selected {
        // ✓
        Span::styled(
            "\u{2713} ".to_owned(),
            Style::default().fg(entry.theme.picker_active_marker),
        )
    } else {
        Span::styled("  ".to_owned(), Style::default())
    };

    let status_prefix = if !entry.is_available {
        "\u{2717} " // ✗
    } else if entry.is_alias {
        "\u{2192} " // →
    } else if entry.is_remote {
        "* "
    } else {
        "  "
    };

    let label_style = if entry.is_available {
        selected_style(is_selected, &entry.theme)
    } else {
        Style::default().fg(entry.theme.muted_text)
    };

    let highlight_bg = entry.theme.picker_highlight_bg;

    // search_text = "{model} {provider_name}"
    // Split match indices into model-portion and provider-portion.
    let (model_indices, provider_indices) = split_match_indices(match_indices, entry.model.len());

    let mut spans = Vec::new();

    // Prefix (status + alias arrow if applicable).
    if entry.is_alias {
        spans.push(Span::styled(
            format!("{}{} → ", status_prefix, entry.name),
            label_style,
        ));
    } else {
        spans.push(Span::styled(status_prefix.to_owned(), label_style));
    }

    // Model text with highlights.
    spans.extend(highlight_text_with_bg(
        &entry.model,
        label_style,
        &model_indices,
        highlight_bg,
    ));

    // Suffix: " (" + highlighted provider_name + ")"
    spans.push(Span::styled(" (".to_owned(), label_style));
    spans.extend(highlight_text_with_bg(
        &entry.provider_name,
        label_style,
        &provider_indices,
        highlight_bg,
    ));
    spans.push(Span::styled(")".to_owned(), label_style));

    Line::from(
        std::iter::once(selection_marker)
            .chain(spans)
            .collect::<Vec<_>>(),
    )
}

/// Splits match indices from `search_text = "{model} {provider_name}"` into
/// two groups: indices within the model portion and indices within the
/// provider-name portion.
///
/// The space separator between model and provider_name occupies byte offset
/// `model_len` (exactly one byte). Provider-portion indices are remapped to be
/// relative to the start of `provider_name` (offset 0).
fn split_match_indices(
    indices: &[Range<usize>],
    model_len: usize,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    // search_text layout: [0..model_len) = model, [model_len] = ' ', [model_len+1..] = provider_name
    let provider_offset = model_len + 1; // +1 for the space separator.

    let mut model_indices = Vec::new();
    let mut provider_indices = Vec::new();

    for range in indices {
        // Model portion: clamp to [0, model_len)
        if range.start < model_len {
            let end = range.end.min(model_len);
            model_indices.push(range.start..end);
        }

        // Provider portion: remap to [0, provider_name.len())
        if range.end > provider_offset {
            let start = range.start.saturating_sub(provider_offset);
            let end = range.end.saturating_sub(provider_offset);
            provider_indices.push(start..end);
        }
    }

    (model_indices, provider_indices)
}

impl jinn_selection_widget::TreeItem for ProviderPickerEntry {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.search_text
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_provider_row(self, is_selected, &[])
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        render_provider_row(self, is_selected, match_indices)
    }
}
