//! Tool picker entry type.
//!
//! The rendering lives in the tool picker's spec (`feat::picker::tool_spec`);
//! this struct is the plain domain data the spec wraps.

use crate::feat::theme::Theme;

/// A tool entry ready for display in the tool picker.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Tool name (unique identifier, e.g., "bash", "edit").
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// Whether the tool is currently enabled for this session.
    pub enabled: bool,
    /// Theme for styling.
    pub theme: Theme,
}

impl jinn_selection_widget::TreeItem for ToolEntry {
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
