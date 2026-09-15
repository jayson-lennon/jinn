//! Session lifecycle picker entry - one row in the lifecycle selection picker.

use crate::feat::theme::Theme;

/// A lifecycle recipe shown in the session lifecycle picker.
#[derive(Debug, Clone)]
pub struct SessionLifecycleEntry {
    /// The lifecycle name (or "blank" for the implicit default).
    pub name: String,
    /// Optional description shown below the name.
    pub description: Option<String>,
    /// Whether this lifecycle requires user-provided args (`$1`, `$2`, etc.).
    pub has_args: bool,
    /// Theme for rendering.
    pub theme: Theme,
}

impl jinn_selection_widget::TreeItem for SessionLifecycleEntry {
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
        reason = "test code"
    )]
    use super::*;
    use crate::feat::picker::session_lifecycle_spec::lifecycle_row;
    use crate::feat::theme::default_theme;
    use jinn_picker::RowCtx;

    fn test_entry(name: &str, description: Option<&str>, has_args: bool) -> SessionLifecycleEntry {
        SessionLifecycleEntry {
            name: name.to_owned(),
            description: description.map(String::from),
            has_args,
            theme: default_theme(),
        }
    }

    fn row_ctx(ranges: &[std::ops::Range<usize>], is_selected: bool) -> RowCtx<'_> {
        RowCtx::flat(is_selected, ranges)
    }

    fn no_matches() -> Vec<std::ops::Range<usize>> {
        Vec::new()
    }

    #[rstest::rstest]
    fn row_unselected_has_spaces() {
        let entry = test_entry("blank", None, false);
        let ranges = no_matches();
        let ctx = row_ctx(&ranges, false);
        let line = lifecycle_row(&entry, &ctx);
        let text = line.to_string();
        assert!(text.starts_with("  blank"));
    }

    #[rstest::rstest]
    fn row_selected_has_arrow() {
        let entry = test_entry("blank", None, false);
        let ranges = no_matches();
        let ctx = row_ctx(&ranges, true);
        let line = lifecycle_row(&entry, &ctx);
        let text = line.to_string();
        assert!(text.starts_with("> blank"));
    }

    #[rstest::rstest]
    fn row_shows_args_indicator() {
        let entry = test_entry("fossil branch", None, true);
        let ranges = no_matches();
        let ctx = row_ctx(&ranges, false);
        let line = lifecycle_row(&entry, &ctx);
        let text = line.to_string();
        assert!(text.contains('*'));
    }

    #[rstest::rstest]
    fn row_shows_description() {
        let entry = test_entry("fossil branch", Some("Open a fossil branch"), false);
        let ranges = no_matches();
        let ctx = row_ctx(&ranges, false);
        let line = lifecycle_row(&entry, &ctx);
        let text = line.to_string();
        assert!(text.contains("Open a fossil branch"));
    }
}
