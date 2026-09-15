//! Reasoning effort picker entry type.

use crate::feat::theme::Theme;

use super::ReasoningEffort;

/// A reasoning effort entry ready for display in the picker.
///
/// Carries the [`ReasoningEffort`] variant so the confirm handler can read it
/// back without re-parsing the display name.
#[derive(Debug, Clone)]
pub struct ReasoningEffortEntry {
    /// The effort variant this entry represents.
    pub effort: ReasoningEffort,
    /// Human-readable display name (the wire string, e.g. "high").
    pub name: String,
    /// Short human description (e.g. "High effort").
    pub description: String,
    /// Whether this is the currently resolved effort.
    pub is_active: bool,
    /// Theme for rendering.
    pub theme: Theme,
}

impl jinn_selection_widget::TreeItem for ReasoningEffortEntry {
    fn id(&self) -> &str {
        &self.name
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, _is_selected: bool) -> ratatui::text::Line<'static> {
        // Rows render through the spec's row hook via PickerEntry; this
        // impl only supplies tree structure (id/parent_id) and filter text.
        ratatui::text::Line::raw(self.display_label().to_owned())
    }

    fn render_row_with_highlight(
        &self,
        _is_selected: bool,
        _match_indices: &[std::ops::Range<usize>],
    ) -> ratatui::text::Line<'static> {
        ratatui::text::Line::raw(self.display_label().to_owned())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::single_range_in_vec_init,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::reasoning_effort_spec::reasoning_row;
    use crate::feat::theme::default_theme;
    use jinn_picker::RowCtx;
    use ratatui::style::Modifier;

    fn test_entry(is_active: bool) -> ReasoningEffortEntry {
        ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active,
            theme: default_theme(),
        }
    }

    fn row_ctx<'a>(ranges: &'a [std::ops::Range<usize>], is_selected: bool) -> RowCtx<'a> {
        RowCtx::flat(is_selected, ranges)
    }

    #[rstest::rstest]
    fn row_active_has_bold_arrow_marker() {
        let entry = test_entry(true);
        let ranges = Vec::new();
        let line = reasoning_row(&entry, &row_ctx(&ranges, false));
        let text = line.to_string();
        assert!(text.starts_with("> high"), "got: {text}");
        // Bold marker span comes first.
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[rstest::rstest]
    fn row_inactive_has_blank_marker() {
        let entry = test_entry(false);
        let ranges = Vec::new();
        let line = reasoning_row(&entry, &row_ctx(&ranges, false));
        let text = line.to_string();
        assert!(text.starts_with("  high"), "got: {text}");
    }

    #[rstest::rstest]
    fn row_renders_description_after_name() {
        let entry = test_entry(false);
        let ranges = Vec::new();
        let line = reasoning_row(&entry, &row_ctx(&ranges, false));
        let text = line.to_string();
        assert!(text.contains("High effort"));
    }

    #[rstest::rstest]
    fn row_highlights_matched_name_with_background() {
        let entry = test_entry(false);
        let ranges = vec![0..2]; // "hi"
        let line = reasoning_row(&entry, &row_ctx(&ranges, false));
        let highlighted = &line.spans[1];
        assert_eq!(highlighted.content.as_ref(), "hi");
        assert!(
            highlighted.style.bg.is_some(),
            "match span carries highlight bg"
        );
    }
}
