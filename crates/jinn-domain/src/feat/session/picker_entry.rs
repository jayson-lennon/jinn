//! Session tree entry type and rendering for the tree-structured session picker.

use std::ops::Range;

use crate::feat::picker::style::{dim_style, selected_style};
use crate::feat::session::chat_session::SessionState;
use crate::feat::theme::Theme;
use crate::protocol::SessionId;
use jinn_selection_widget::TreeItem;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// A saved session entry ready for display in the tree-structured picker.
///
/// Implements [`TreeItem`] for tree-aware fuzzy filtering and rendering.
/// ID fields are pre-computed as strings to satisfy the `&str` return types
/// on [`TreeItem::id`] and [`TreeItem::parent_id`].
#[derive(Debug, Clone)]
pub struct SessionTreeEntry {
    /// The session's unique identifier.
    pub session_id: SessionId,
    /// Pre-computed string representation of `session_id` for `TreeItem::id`.
    pub id_str: String,
    /// Human-readable title (derived from first user message).
    pub title: String,
    /// When this session was last modified.
    pub updated_at: jiff::Timestamp,
    /// Theme for rendering.
    pub theme: Theme,
    /// Whether this session is loaded in memory or archived.
    pub session_state: SessionState,
    /// Parent session ID - `None` for root sessions.
    pub parent_id: Option<SessionId>,
    /// Pre-computed string representation of `parent_id` for `TreeItem::parent_id`.
    pub parent_id_str: Option<String>,
    /// Project directory association, if the session was created from the
    /// projects UI.
    pub project: Option<std::path::PathBuf>,
    /// Pre-computed project column text: the leaf directory name (e.g.
    /// `~/code/jinn` → `jinn`), or empty when the session has no project.
    pub project_display: String,
    /// Display width of the project column, shared by every entry in one
    /// picker load (the longest project name across loaded entries).
    pub project_width: usize,
}

impl SessionTreeEntry {
    /// Creates a new session tree entry.
    pub fn new(
        session_id: SessionId,
        title: String,
        updated_at: jiff::Timestamp,
        theme: Theme,
        session_state: SessionState,
        parent_id: Option<SessionId>,
        project: Option<std::path::PathBuf>,
    ) -> Self {
        let id_str = session_id.to_string();
        let parent_id_str = parent_id.as_ref().map(std::string::ToString::to_string);
        let project_display = project_display_name(project.as_deref());
        Self {
            session_id,
            id_str,
            title,
            updated_at,
            theme,
            session_state,
            parent_id,
            parent_id_str,
            project,
            project_display,
            project_width: 0,
        }
    }
}

/// Returns the project column text for a project path: the leaf directory
/// name, falling back to the full path display when `file_name()` is `None`
/// (e.g. a bare root path).
fn project_display_name(project: Option<&std::path::Path>) -> String {
    match project {
        None => String::new(),
        Some(path) => path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

/// Pads every entry's project column to the longest project name across the
/// list, so the columns align when rows render side by side.
pub fn apply_project_column_width(entries: &mut [SessionTreeEntry]) {
    let width = entries
        .iter()
        .map(|e| e.project_display.width())
        .max()
        .unwrap_or(0);
    for entry in entries.iter_mut() {
        entry.project_width = width;
    }
}

impl TreeItem for SessionTreeEntry {
    fn id(&self) -> &str {
        &self.id_str
    }

    fn parent_id(&self) -> Option<&str> {
        self.parent_id_str.as_deref()
    }

    fn display_label(&self) -> &str {
        &self.title
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        self.render_row_impl(is_selected, &[], "", Style::default())
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        self.render_row_impl(is_selected, match_indices, "", Style::default())
    }

    fn render_row_with_tree(
        &self,
        is_selected: bool,
        match_ranges: &[Range<usize>],
        tree_prefix: &str,
        tree_style: Style,
    ) -> Line<'static> {
        self.render_row_impl(is_selected, match_ranges, tree_prefix, tree_style)
    }
}

/// Renders a session picker row: date, project, then the title with an
/// optional tree connector placed directly before the title text.
///
/// Match indices are byte offsets into the title (the `display_label`) and are
/// only ever applied to the title spans, never the date, project, or connector
/// spans. Format: `{YYYY-MM-DD HH:MM}  {project:<width}  [{connector}]{title}`.
/// Archived sessions use dimmed styling to visually distinguish them from
/// loaded sessions.
impl SessionTreeEntry {
    pub(crate) fn render_row_impl(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
        tree_prefix: &str,
        tree_style: Style,
    ) -> Line<'static> {
        let base_style = if self.session_state == SessionState::Archived {
            dim_style(is_selected, &self.theme)
        } else {
            selected_style(is_selected, &self.theme)
        };

        let meta_style = dim_style(is_selected, &self.theme);

        let datetime = self.updated_at.to_zoned(jiff::tz::TimeZone::UTC).datetime();
        let date_str = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            datetime.year(),
            datetime.month() as u8,
            datetime.day(),
            datetime.hour(),
            datetime.minute(),
        );

        // Pad by display width (unicode-width), never by chars or bytes.
        let pad = self
            .project_width
            .saturating_sub(self.project_display.width());
        let mut project_padded = String::with_capacity(self.project_display.len() + pad);
        project_padded.push_str(&self.project_display);
        project_padded.extend(std::iter::repeat_n(' ', pad));

        let title_spans = if match_indices.is_empty() {
            vec![Span::styled(self.title.clone(), base_style)]
        } else {
            highlight_text_with_bg(
                &self.title,
                base_style,
                match_indices,
                self.theme.picker_highlight_bg,
            )
        };

        let mut spans = vec![
            Span::styled(date_str, meta_style),
            Span::styled("  ".to_owned(), base_style),
            Span::styled(project_padded, meta_style),
            Span::styled("  ".to_owned(), base_style),
        ];
        if !tree_prefix.is_empty() {
            spans.push(Span::styled(tree_prefix.to_owned(), tree_style));
        }
        spans.extend(title_spans);
        Line::from(spans)
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
    fn archived_entry_renders_with_dimmed_foreground() {
        // Given an archived session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Archived Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Archived,
            None,
            None,
        );

        // When rendering (not selected).
        let row = entry.render_row(false);

        // Then the title span has muted_text foreground.
        let title_span = &row.spans[4];
        assert_eq!(title_span.style.fg, Some(default_theme().muted_text));
    }

    #[rstest::rstest]
    fn loaded_entry_renders_with_normal_foreground() {
        // Given a loaded session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Active Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When rendering (not selected).
        let row = entry.render_row(false);

        // Then the title span has default foreground (no explicit fg).
        let title_span = &row.spans[4];
        assert_eq!(title_span.style.fg, None);
    }

    #[rstest::rstest]
    fn archived_selected_entry_has_contrast_foreground() {
        // Given an archived session tree entry.
        let theme = default_theme();
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Archived Chat".to_owned(),
            jiff::Timestamp::now(),
            theme.clone(),
            SessionState::Archived,
            None,
            None,
        );

        // When rendering (selected).
        let row = entry.render_row(true);

        // Then the title span has contrast-adjusted foreground on selected bg.
        let title_span = &row.spans[4];
        let expected_fg = crate::feat::theme::contrast::ensure_contrast(
            theme.muted_text,
            theme.picker_selected_bg,
        );
        assert_eq!(title_span.style.fg, Some(expected_fg));
        assert_eq!(title_span.style.bg, Some(theme.picker_selected_bg));
    }

    #[rstest::rstest]
    fn tree_item_id_returns_session_id_string() {
        // Given a session tree entry.
        let id = SessionId::new();
        let entry = SessionTreeEntry::new(
            id.clone(),
            "Test".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When calling id().
        // Then it returns the string representation of the session ID.
        assert_eq!(entry.id(), id.to_string());
        assert!(entry.parent_id().is_none());
    }

    #[rstest::rstest]
    fn tree_item_parent_id_returns_string() {
        // Given a session tree entry with a parent.
        let parent_id = SessionId::new();
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Child".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            Some(parent_id.clone()),
            None,
        );

        // When calling parent_id().
        // Then it returns the string representation of the parent ID.
        assert_eq!(entry.parent_id(), Some(parent_id.to_string().as_str()));
    }

    #[rstest::rstest]
    fn entry_without_project_renders_blank_project_column() {
        // Given a session tree entry with no project.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "No Project".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When rendering.
        let row = entry.render_row(false);

        // Then the row is date, gap, blank project, gap, title (5 spans).
        assert_eq!(row.spans.len(), 5);
        // And the project span is blank.
        assert_eq!(row.spans[2].content, "");
        // And the title span still carries the title.
        assert_eq!(row.spans[4].content, "No Project");
    }

    #[rstest::rstest]
    fn entry_with_project_renders_leaf_directory_name() {
        // Given a session tree entry with a project path.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Deep Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/home/user/code/jinn")),
        );

        // When rendering.
        let row = entry.render_row(false);

        // Then the project span shows only the leaf directory name.
        assert_eq!(row.spans[2].content, "jinn");
    }

    #[rstest::rstest]
    fn project_display_falls_back_to_path_without_file_name() {
        // Given a project path with no file_name component (a bare root).
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Root Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/")),
        );

        // When computing the project display.
        // Then it falls back to the full path display string.
        assert_eq!(entry.project_display, "/");
    }

    #[rstest::rstest]
    fn project_columns_pad_to_max_width_across_entries() {
        // Given two entries whose project names differ in length, with the
        // column width applied across the list.
        let short = SessionTreeEntry::new(
            SessionId::new(),
            "Short".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/code/a/jinn")),
        );
        let long = SessionTreeEntry::new(
            SessionId::new(),
            "Long".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/code/b/my-project")),
        );
        let mut entries = vec![short, long];
        apply_project_column_width(&mut entries);

        // When rendering both.
        let row_short = entries[0].render_row(false);
        let row_long = entries[1].render_row(false);

        // Then the short project span is padded to the longest project width
        // (11 = width of "my-project") and both title spans start at the
        // same content offset.
        let width_of = |span: &Span<'_>| span.content.width();
        assert_eq!(width_of(&row_short.spans[2]), 10);
        assert_eq!(width_of(&row_long.spans[2]), 10);
        let prefix_len = |row: &Line<'_>| {
            row.spans[..4]
                .iter()
                .map(|s| s.content.width())
                .sum::<usize>()
        };
        assert_eq!(prefix_len(&row_short), prefix_len(&row_long));
    }

    #[rstest::rstest]
    fn apply_project_column_width_is_zero_without_projects() {
        // Given entries with no projects.
        let a = SessionTreeEntry::new(
            SessionId::new(),
            "A".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );
        let b = SessionTreeEntry::new(
            SessionId::new(),
            "B".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );
        let mut entries = vec![a, b];

        // When applying the column width.
        apply_project_column_width(&mut entries);

        // Then the width is zero for every entry.
        assert!(entries.iter().all(|e| e.project_width == 0));
    }

    #[rstest::rstest]
    fn highlight_applies_only_to_title_column() {
        // Given an entry whose title matches a fuzzy query starting at the
        // title's first byte, with the column width applied.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Fix bug".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/code/jinn")),
        );
        let mut entries = vec![entry];
        apply_project_column_width(&mut entries);

        // When rendering with a match covering "Fix" (bytes 0..3 of title).
        let row = entries[0].render_row_with_highlight(false, std::slice::from_ref(&(0..3)));

        // Then the date and project spans carry no highlight background.
        assert_eq!(row.spans[0].style.bg, None);
        assert_eq!(row.spans[2].style.bg, None);
        // And the first title span carries the highlight background.
        assert_eq!(
            row.spans[4].style.bg,
            Some(default_theme().picker_highlight_bg)
        );
    }

    #[rstest::rstest]
    fn child_row_places_connector_directly_before_title_span() {
        // Given a session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Fix bug".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );
        let tree_style = Style::default();

        // When rendering with a tree connector (as the widget hands children).
        let row = entry.render_row_with_tree(false, &[], "└─ ", tree_style);

        // Then the connector span sits immediately before the title span.
        assert_eq!(row.spans.len(), 6);
        assert_eq!(row.spans[4].content, "└─ ");
        assert_eq!(row.spans[4].style, tree_style);
        assert_eq!(row.spans[5].content, "Fix bug");
    }

    #[rstest::rstest]
    fn root_row_renders_without_connector_span() {
        // Given a session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Root Chat".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When rendering with an empty tree connector (as the widget hands roots).
        let row = entry.render_row_with_tree(false, &[], "", Style::default());

        // Then the row shape is unchanged: date, gap, project, gap, title.
        assert_eq!(row.spans.len(), 5);
        assert_eq!(row.spans[4].content, "Root Chat");
    }

    #[rstest::rstest]
    fn highlight_lands_only_on_title_text_after_connector() {
        // Given an entry with a project column applied.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Fix bug".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            Some(std::path::PathBuf::from("/code/jinn")),
        );
        let mut entries = vec![entry];
        apply_project_column_width(&mut entries);

        // When rendering with a connector and a match covering "Fix"
        // (bytes 0..3 of the bare title).
        let row = entries[0].render_row_with_tree(
            false,
            std::slice::from_ref(&(0..3)),
            "├─ ",
            Style::default(),
        );

        // Then the date, project, and connector spans carry no highlight.
        assert_eq!(row.spans[0].style.bg, None);
        assert_eq!(row.spans[2].style.bg, None);
        assert_eq!(row.spans[4].style.bg, None);
        // And the first title span carries the highlight background.
        assert_eq!(
            row.spans[5].style.bg,
            Some(default_theme().picker_highlight_bg)
        );
    }

    #[rstest::rstest]
    fn display_label_excludes_tree_connectors() {
        // Given a session tree entry.
        let entry = SessionTreeEntry::new(
            SessionId::new(),
            "Fix bug".to_owned(),
            jiff::Timestamp::now(),
            default_theme(),
            SessionState::Loaded,
            None,
            None,
        );

        // When reading the display label used for fuzzy matching.
        let label = entry.display_label();

        // Then it is the bare title with no tree glyphs.
        assert_eq!(label, "Fix bug");
        assert!(!label.contains('├') && !label.contains('└') && !label.contains('│'));
    }
}
