//! Task list tree entry type and rendering for the read-only task list picker.
//!
//! Implements [`TreeItem`] for the picker overlay opened via `s` from the
//! the sidebar task-list scope. Entries are flattened from
//! [`TaskList`](super::TaskList) into a two-level tree: phases are roots,
//! tasks are children of their owning phase. Postponed tasks are filtered
//! out by the loader, not by this type.

use std::ops::Range;

use ratatui::text::{Line, Span};
use ratatui::{style::Style, symbols};

use crate::feat::picker::style::{dim_style, selected_style};
use crate::feat::theme::Theme;
use jinn_tools_msg::TaskStatus;

use jinn_selection_widget::TreeItem;
use jinn_selection_widget::highlight_text_with_bg;

/// Visual row-kind marker for a [`TaskListTreeEntry`].
///
/// Phases have no status; tasks carry one of the [`TaskStatus`] values
/// (except `Postponed`, which the loader skips).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// A phase row (no task status).
    Phase,
    /// A task row carrying its [`TaskStatus`].
    Task(TaskStatus),
}

/// A single row in the task list picker tree.
///
/// IDs are pre-prefixed with `"phase:"` or `"task:"` to guarantee uniqueness
/// across the namespace even if a `PhaseId` and `TaskId` happen to collide
/// on their string contents.
#[derive(Debug, Clone)]
pub struct TaskListTreeEntry {
    /// Pre-computed full id string (e.g. `"phase:pabc"` or `"task:tabc"`).
    id_str: String,
    /// Pre-computed parent id string (the phase id) for tasks, `None` for phases.
    parent_id_str: Option<String>,
    /// Human-readable description (phase or task text).
    description: String,
    /// Row kind + status, used for rendering the indicator glyph.
    row_status: RowStatus,
    /// Theme reference for styled rendering.
    theme: Theme,
}

impl TaskListTreeEntry {
    /// Constructs a phase root entry.
    #[must_use]
    pub fn new_phase(id_str: String, description: String, theme: Theme) -> Self {
        Self {
            id_str,
            parent_id_str: None,
            description,
            row_status: RowStatus::Phase,
            theme,
        }
    }

    /// Constructs a task child entry. `parent_id_str` is the phase's id string.
    #[must_use]
    pub fn new_task(
        id_str: String,
        parent_id_str: Option<String>,
        description: String,
        status: TaskStatus,
        theme: Theme,
    ) -> Self {
        Self {
            id_str,
            parent_id_str,
            description,
            row_status: RowStatus::Task(status),
            theme,
        }
    }

    /// Returns the row's kind and status.
    #[must_use]
    pub const fn row_status(&self) -> RowStatus {
        self.row_status
    }

    /// The theme for styled rendering.
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }
}

impl TreeItem for TaskListTreeEntry {
    fn id(&self) -> &str {
        &self.id_str
    }

    fn parent_id(&self) -> Option<&str> {
        self.parent_id_str.as_deref()
    }

    fn display_label(&self) -> &str {
        &self.description
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_task_list_row(
            &self.description,
            self.row_status,
            is_selected,
            &[],
            &self.theme,
        )
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        render_task_list_row(
            &self.description,
            self.row_status,
            is_selected,
            match_indices,
            &self.theme,
        )
    }
}

/// Renders a task list picker row, optionally highlighting matched characters.
///
/// Match indices are byte offsets into `description` (the `display_label`).
/// Layout: `"{indicator} {description}"`. Phases use a triangular glyph and
/// bold styling to visually distinguish them from tasks.
pub(crate) fn render_task_list_row(
    description: &str,
    row_status: RowStatus,
    is_selected: bool,
    match_indices: &[Range<usize>],
    theme: &Theme,
) -> Line<'static> {
    let (glyph, glyph_style, text_style) = match row_status {
        RowStatus::Phase => (
            symbols::DOT,
            dim_style(is_selected, theme).add_modifier(ratatui::style::Modifier::BOLD),
            dim_style(is_selected, theme).add_modifier(ratatui::style::Modifier::BOLD),
        ),
        RowStatus::Task(status) => {
            let glyph = status.indicator();
            let text = match status {
                TaskStatus::Completed | TaskStatus::Cancelled => dim_style(is_selected, theme),
                TaskStatus::Pending | TaskStatus::Postponed => selected_style(is_selected, theme),
            };
            (glyph, text, text)
        }
    };

    let glyph_span = Span::styled(format!("{glyph} "), glyph_style);

    let desc_spans = if match_indices.is_empty() {
        vec![Span::styled(description.to_owned(), text_style)]
    } else {
        highlight_text_with_bg(
            description,
            text_style,
            match_indices,
            theme.picker_highlight_bg,
        )
    };

    let mut spans = vec![glyph_span];
    let _ = Style::default; // keep Style in scope for future styling tweaks
    spans.extend(desc_spans);
    Line::from(spans)
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
    fn phase_entry_has_no_parent() {
        // Given a phase entry.
        let entry = TaskListTreeEntry::new_phase(
            "phase:pabc".to_owned(),
            "Research".to_owned(),
            default_theme(),
        );

        // When inspecting identity and parent.
        // Then id is the prefixed string and parent is None.
        assert_eq!(entry.id(), "phase:pabc");
        assert!(entry.parent_id().is_none());
        assert_eq!(entry.display_label(), "Research");
        assert_eq!(entry.row_status(), RowStatus::Phase);
    }

    #[rstest::rstest]
    fn task_entry_has_phase_parent_and_status() {
        // Given a task entry with a parent.
        let entry = TaskListTreeEntry::new_task(
            "task:tabc".to_owned(),
            Some("phase:pabc".to_owned()),
            "Implement feature".to_owned(),
            TaskStatus::Pending,
            default_theme(),
        );

        // When inspecting identity and parent.
        // Then id and parent id are the prefixed strings, status carries through.
        assert_eq!(entry.id(), "task:tabc");
        assert_eq!(entry.parent_id(), Some("phase:pabc"));
        assert_eq!(entry.display_label(), "Implement feature");
        assert_eq!(entry.row_status(), RowStatus::Task(TaskStatus::Pending));
    }

    #[rstest::rstest]
    fn display_label_does_not_include_status_glyph() {
        // Given a task entry.
        let entry = TaskListTreeEntry::new_task(
            "task:tabc".to_owned(),
            Some("phase:pabc".to_owned()),
            "Write tests".to_owned(),
            TaskStatus::Completed,
            default_theme(),
        );

        // When calling display_label.
        // Then it returns only the description, no glyph prefix.
        assert_eq!(entry.display_label(), "Write tests");
        assert!(
            !entry
                .display_label()
                .contains(TaskStatus::Completed.indicator())
        );
    }

    #[rstest::rstest]
    fn render_row_for_phase_produces_two_spans() {
        // Given a phase entry.
        let entry = TaskListTreeEntry::new_phase(
            "phase:pabc".to_owned(),
            "Build".to_owned(),
            default_theme(),
        );

        // When rendering.
        let row = entry.render_row(false);

        // Then row contains glyph span + description span.
        assert_eq!(row.spans.len(), 2);
    }

    #[rstest::rstest]
    fn render_row_for_task_produces_two_spans() {
        // Given a task entry.
        let entry = TaskListTreeEntry::new_task(
            "task:tabc".to_owned(),
            Some("phase:pabc".to_owned()),
            "Write code".to_owned(),
            TaskStatus::Pending,
            default_theme(),
        );

        // When rendering.
        let row = entry.render_row(false);

        // Then row contains glyph span + description span.
        assert_eq!(row.spans.len(), 2);
    }

    #[rstest::rstest]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "range syntax is clearer than vec!"
    )]
    fn render_row_with_highlight_preserves_glyph_span() {
        // Given a task entry.
        let entry = TaskListTreeEntry::new_task(
            "task:tabc".to_owned(),
            Some("phase:pabc".to_owned()),
            "Write tests".to_owned(),
            TaskStatus::Pending,
            default_theme(),
        );

        // When rendering with highlight on indices [0..1) of "Write tests".
        let row = entry.render_row_with_highlight(false, &[0..1]);

        // Then glyph span is still first and unchanged; description spans follow.
        assert!(!row.spans[0].content.is_empty());
    }

    #[rstest::rstest]
    fn task_status_variants_propagate_into_row_status() {
        // Given entries for each task status variant.
        let theme = default_theme();
        for status in [
            TaskStatus::Pending,
            TaskStatus::Completed,
            TaskStatus::Postponed,
            TaskStatus::Cancelled,
        ] {
            let entry = TaskListTreeEntry::new_task(
                "task:t".to_owned(),
                Some("phase:p".to_owned()),
                "x".to_owned(),
                status,
                theme.clone(),
            );

            // When inspecting row_status.
            // Then it matches the input status exactly.
            assert_eq!(entry.row_status(), RowStatus::Task(status));
        }
    }
}
