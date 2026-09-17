//! Picker entry for the project picker - one row per curated directory.

use jinn_selection_widget::PickerItem;
use ratatui::text::{Line, Span};

use crate::common::path_display::shorten_path;
use crate::feat::picker::style::{active_marker, selected_style};
use crate::feat::theme::Theme;

/// A curated project directory shown in the project picker.
///
/// `display` is the tilde-compressed form (precomputed once at load so filtering
/// and sorting operate on what the user sees); `path` is the canonical dir used
/// to set the new session's CWD on confirm.
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    /// The canonical directory path (what the new session's CWD is set to).
    pub path: std::path::PathBuf,
    /// The tilde-compressed display string (precomputed from `path`).
    pub display: String,
    /// Path as a string, cached for the `TreeItem` identity contract.
    pub id_string: String,
    /// Theme for rendering.
    pub theme: Theme,
}

impl ProjectEntry {
    /// Build an entry from a directory path, precomputing the display string.
    #[must_use]
    pub fn new(path: std::path::PathBuf, theme: Theme) -> Self {
        let display = shorten_path(&path);
        Self {
            id_string: path.to_string_lossy().into_owned(),
            path,
            display,
            theme,
        }
    }
}

impl PickerItem for ProjectEntry {
    fn display_label(&self) -> &str {
        &self.display
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_project_row(&self.display, is_selected, &[], &self.theme)
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[std::ops::Range<usize>],
    ) -> Line<'static> {
        render_project_row(&self.display, is_selected, match_indices, &self.theme)
    }
}

/// Renders a project picker row with selection styling.
pub(crate) fn render_project_row(
    display: &str,
    is_selected: bool,
    match_indices: &[std::ops::Range<usize>],
    theme: &Theme,
) -> Line<'static> {
    let base_style = selected_style(is_selected, theme);
    let mut spans = vec![active_marker(is_selected, theme)];

    if match_indices.is_empty() {
        spans.push(Span::styled(display.to_owned(), base_style));
    } else {
        spans.extend(jinn_selection_widget::highlight_text_with_bg(
            display,
            base_style,
            match_indices,
            theme.picker_highlight_bg,
        ));
    }

    Line::from(spans)
}

/// Build entries for the project picker from the curated project list.
///
/// Entries are sorted by their display string so the list is stable and
/// alphabetical regardless of `jinn.toml` ordering.
pub fn project_entries(
    projects: &[crate::feat::project::ProjectConfig],
    theme: &Theme,
) -> Vec<ProjectEntry> {
    let mut entries: Vec<_> = projects
        .iter()
        .map(|p| ProjectEntry::new(p.path.clone(), theme.clone()))
        .collect();
    entries.sort_by(|a, b| a.display.cmp(&b.display));
    entries
}

impl jinn_selection_widget::TreeItem for ProjectEntry {
    fn id(&self) -> &str {
        // Paths are unique in `projects`; lossy is fine — this is identity only.
        &self.id_string
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.display
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        PickerItem::render_row(self, is_selected)
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[std::ops::Range<usize>],
    ) -> Line<'static> {
        PickerItem::render_row_with_highlight(self, is_selected, match_indices)
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
    use crate::feat::theme::default_theme;

    #[rstest::rstest]
    fn display_label_is_tilde_compressed_path() {
        // Given a project entry whose path sits under the home directory.
        let home = dirs::home_dir().expect("home dir available in test");
        let entry = ProjectEntry::new(home.join("code").join("jinn"), default_theme());

        // Then the display label is tilde-compressed.
        assert_eq!(entry.display_label(), "~/code/jinn");
    }

    #[rstest::rstest]
    fn render_row_unselected_has_leading_spaces() {
        // Given a project entry.
        let entry = ProjectEntry::new(std::path::PathBuf::from("/tmp/proj"), default_theme());

        // When rendering unselected.
        let text = entry.render_row(false).to_string();

        // Then it begins with the two-space inactive marker.
        assert!(text.starts_with("  /tmp/proj"));
    }

    #[rstest::rstest]
    fn render_row_selected_has_arrow() {
        // Given a project entry.
        let entry = ProjectEntry::new(std::path::PathBuf::from("/tmp/proj"), default_theme());

        // When rendering selected.
        let text = entry.render_row(true).to_string();

        // Then it begins with the active arrow marker.
        assert!(text.starts_with("> /tmp/proj"));
    }

    #[rstest::rstest]
    fn project_entries_sorted_by_display() {
        // Given projects in a non-alphabetical order.
        let projects = vec![
            crate::feat::project::ProjectConfig {
                path: std::path::PathBuf::from("/zzz"),
                command_policy: Vec::new(),
            },
            crate::feat::project::ProjectConfig {
                path: std::path::PathBuf::from("/aaa"),
                command_policy: Vec::new(),
            },
        ];

        // When building entries.
        let entries = project_entries(&projects, &default_theme());

        // Then they are sorted by display string.
        assert_eq!(entries[0].display, "/aaa");
        assert_eq!(entries[1].display, "/zzz");
    }

    #[rstest::rstest]
    fn project_entries_empty_input_returns_empty() {
        // Given no projects.
        // When building entries.
        let entries = project_entries(&[], &default_theme());

        // Then the result is empty.
        assert!(entries.is_empty());
    }
}
