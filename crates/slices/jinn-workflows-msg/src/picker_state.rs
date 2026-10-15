//! The workflows picker's state cell payload and its entry type.
//!
//! The picker is owned entirely by the workflows slice: this type is what
//! its cell holds, so no other crate names a field for it.

use jinn_picker::RowCtx;
use jinn_picker::picker_style::{active_marker, dim_style, selected_style};
use jinn_selection_widget::highlight_text_with_bg;
use jinn_theme::Theme;
use ratatui::text::{Line, Span};

use crate::workflow::Workflow;

/// A workflow entry ready for display in the workflows picker.
#[derive(Debug, Clone)]
pub struct WorkflowEntry {
    /// The workflow's display name.
    pub name: String,
    /// The workflow's frontmatter description.
    pub description: String,
    /// Theme for styling.
    pub theme: Theme,
}

/// Renders one workflows picker row: the cursor marker, the workflow name,
/// and the description after an em-dash separator — the lifecycle picker's
/// row shape, minus the args marker no workflow has.
#[must_use]
pub fn workflow_row(entry: &WorkflowEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let base_style = selected_style(ctx.is_selected, &entry.theme);
    let desc_style = dim_style(ctx.is_selected, &entry.theme);

    let mut spans = vec![active_marker(ctx.is_selected, &entry.theme)];

    if ctx.match_ranges.is_empty() {
        spans.push(Span::styled(entry.name.clone(), base_style));
    } else {
        spans.extend(highlight_text_with_bg(
            &entry.name,
            base_style,
            ctx.match_ranges,
            entry.theme.picker_highlight_bg,
        ));
    }

    spans.push(Span::styled(
        format!(" \u{2014} {}", entry.description),
        desc_style,
    ));

    Line::from(spans)
}

impl jinn_selection_widget::TreeItem for WorkflowEntry {
    fn id(&self) -> &str {
        &self.name
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, _is_selected: bool) -> Line<'static> {
        // Rows render through the spec's row hook via PickerEntry; this
        // impl only supplies tree structure (id/parent_id) and filter text.
        Line::raw(self.display_label().to_owned())
    }

    fn render_row_with_highlight(
        &self,
        _is_selected: bool,
        _match_indices: &[std::ops::Range<usize>],
    ) -> Line<'static> {
        Line::raw(self.display_label().to_owned())
    }
}

/// The workflows picker's complete state: the rows, the filter, and the
/// highlight.
#[derive(Debug)]
pub struct WorkflowPickerState {
    /// The selection/filter state backing the picker's rows.
    pub selection: jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<WorkflowEntry>>,
    /// How many result rows fit on screen, measured by the render pass.
    pub results_viewport: usize,
    /// The theme the rows were built with, so a repaint recolors them
    /// consistently instead of stranding them on a stale palette.
    pub theme: Theme,
}

impl Default for WorkflowPickerState {
    fn default() -> Self {
        Self {
            selection: jinn_selection_widget::SelectionState::new(),
            // Matches the other pickers' fallback, so the very first
            // keypress before any render behaves as it does everywhere.
            results_viewport: crate::picker_scope::RESULTS_VIEWPORT_FALLBACK,
            theme: jinn_theme::default_theme(),
        }
    }
}

impl WorkflowPickerState {
    /// Clears the filter and moves the highlight back to the top, ready for
    /// a fresh open.
    pub fn reset(&mut self) {
        self.selection.clear_filter();
        self.selection.move_up(usize::MAX);
    }
}

/// The row's search text: name plus description, so filtering on either
/// finds the workflow.
#[must_use]
pub fn workflow_search_text(entry: &WorkflowEntry) -> String {
    format!("{} {}", entry.name, entry.description)
}

/// Builds picker entries from the discovered workflows, sorted
/// case-insensitively by name.
#[must_use]
pub fn build_workflow_entries(
    discovered: &[Workflow],
    theme: &Theme,
) -> Vec<jinn_picker::PickerEntry<WorkflowEntry>> {
    let mut entries: Vec<WorkflowEntry> = discovered
        .iter()
        .map(|workflow| WorkflowEntry {
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            theme: theme.clone(),
        })
        .collect();
    entries.sort_by_key(|entry| entry.name.to_lowercase());

    jinn_picker::make_items_with_hooks(
        entries,
        jinn_picker::PickerItemHooks::new()
            .row(workflow_row)
            .search(workflow_search_text),
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::single_range_in_vec_init,
        reason = "test code"
    )]
    use super::*;
    use jinn_theme::default_theme;

    fn workflow(name: &str, description: &str) -> Workflow {
        Workflow {
            name: name.to_owned(),
            description: description.to_owned(),
            dir: std::path::PathBuf::from(format!("/cfg/workflows/{name}")),
        }
    }

    #[rstest::rstest]
    fn row_shows_name_and_description() {
        // Given one workflow entry.
        let entry = WorkflowEntry {
            name: "research-report".to_owned(),
            description: "Research a topic and deliver a cited brief.".to_owned(),
            theme: default_theme(),
        };

        // When rendering its row unselected with no filter matches.
        let line = workflow_row(&entry, &RowCtx::flat(false, &Vec::new()));

        // Then the row starts with the unselected padding and carries the
        // description after an em-dash.
        let text = line.to_string();
        assert!(text.starts_with("  research-report"), "got: {text}");
        assert!(text.contains("Research a topic"), "got: {text}");
    }

    #[rstest::rstest]
    fn build_entries_sorts_case_insensitively() {
        // Given discovered workflows in non-alphabetical order.
        let discovered = vec![workflow("zeta", "last"), workflow("Alpha", "first")];

        // When building the picker's entries.
        let items = build_workflow_entries(&discovered, &default_theme());

        // Then they are ordered case-insensitively by name.
        let names: Vec<&str> = items
            .iter()
            .map(jinn_selection_widget::TreeItem::id)
            .collect();
        assert_eq!(names, ["Alpha", "zeta"]);
    }

    #[rstest::rstest]
    fn search_text_matches_on_name_or_description() {
        // Given one entry.
        let entry = WorkflowEntry {
            name: "research".to_owned(),
            description: "deliver a brief".to_owned(),
            theme: default_theme(),
        };

        // When building its search text.
        let text = workflow_search_text(&entry);

        // Then it contains both the name and the description.
        assert_eq!(text, "research deliver a brief");
    }

    #[rstest::rstest]
    fn reset_clears_the_filter_and_returns_to_the_top() {
        // Given a picker state with a filter and a moved highlight.
        let mut state = WorkflowPickerState::default();
        state.selection.set_items(build_workflow_entries(
            &[workflow("a", "x"), workflow("b", "y")],
            &default_theme(),
        ));
        state.selection.insert_char('a');
        state.selection.move_down(state.results_viewport);

        // When resetting for a fresh open.
        state.reset();

        // Then the filter is empty and the highlight is back at the top.
        assert_eq!(state.selection.filter(), "");
        assert_eq!(
            state
                .selection
                .selected_item()
                .map(|i| i.entry().name.clone())
                .expect("reset picker still has rows"),
            "a"
        );
    }
}
