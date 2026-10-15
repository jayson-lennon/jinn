//! The workflows picker's actions — pure functions over the picker's own
//! cell.
//!
//! Each action is a snapshot-transform, act, drop sequence: read what it
//! needs, mutate the cell, release the guard. Nothing here borrows across an
//! await point, and nothing reaches into the kernel — the session-facing
//! effects are returned as values the route layer turns into messages.

use jinn_workflows_msg::{Workflow, WorkflowPickerState, build_workflow_entries};

/// Opening the picker: fresh filter and selection, capture the theme, and
/// load the rows from the session's discovered workflows.
pub fn open(state: &mut WorkflowPickerState, discovered: &[Workflow], theme: &jinn_theme::Theme) {
    state.reset();
    state.theme = theme.clone();
    state
        .selection
        .set_items(build_workflow_entries(discovered, theme));
}

/// The highlighted workflow's name, if any row is selected.
#[must_use]
pub fn highlighted_name(state: &WorkflowPickerState) -> Option<String> {
    state
        .selection
        .selected_item()
        .map(|item| item.entry().name.clone())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use jinn_theme::default_theme;

    fn workflow(name: &str) -> Workflow {
        Workflow {
            name: name.to_owned(),
            description: format!("the {name} workflow"),
            dir: std::path::PathBuf::from(format!("/cfg/workflows/{name}")),
        }
    }

    #[rstest::rstest]
    fn open_seeds_the_rows_from_the_discovered_workflows() {
        // Given a default picker state and two discovered workflows.
        let mut state = WorkflowPickerState::default();

        // When opening the picker over them.
        open(
            &mut state,
            &[workflow("alpha"), workflow("beta")],
            &default_theme(),
        );

        // Then both are shown in display order.
        let names: Vec<String> = state
            .selection
            .items()
            .iter()
            .map(|item| item.entry().name.clone())
            .collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[rstest::rstest]
    fn open_resets_the_filter_and_the_highlight() {
        // Given a picker opened once, filtered, and moved off the top.
        let mut state = WorkflowPickerState::default();
        open(
            &mut state,
            &[workflow("alpha"), workflow("beta")],
            &default_theme(),
        );
        state.selection.insert_char('a');
        state.selection.move_down(state.results_viewport);

        // When reopening it.
        open(
            &mut state,
            &[workflow("alpha"), workflow("beta")],
            &default_theme(),
        );

        // Then the filter is gone and the highlight is back on the first
        // row, so a fresh open never inherits the last one's narrowing.
        assert_eq!(state.selection.filter(), "");
        assert_eq!(highlighted_name(&state).as_deref(), Some("alpha"));
    }

    #[rstest::rstest]
    fn highlighted_name_is_none_without_rows() {
        // Given a picker that was never opened.
        let state = WorkflowPickerState::default();

        // When asking for the highlighted name.
        let name = highlighted_name(&state);

        // Then there is none.
        assert_eq!(name, None);
    }
}
