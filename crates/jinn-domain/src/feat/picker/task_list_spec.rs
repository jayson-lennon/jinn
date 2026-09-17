//! The task-list picker's spec — behavior authored once in the builder.
//!
//! A read-only tree browser: the active session's task list with phases as
//! roots and tasks as children. Postponed tasks are hidden, matching the
//! sidebar. Enter is a no-op — task management happens through the task
//! tools; ESC/Q close.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use jinn_selection_widget::TreeItem;

use crate::feat::picker::task_list_picker_entry::TaskListTreeEntry;
use crate::feat::picker::task_list_picker_entry::render_task_list_row;
use crate::feat::ui::picker_states::PickerExt;
use jinn_tools_msg::TaskStatus;

/// Builds the task-list picker's spec.
#[must_use]
pub fn task_list_spec() -> PickerSpec<TaskListTreeEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::TASK_LIST_ID))
        .title(" Task List ")
        .widget(jinn_picker::PickerWidget::Tree)
        .row(task_list_row)
        .search(|entry| entry.display_label().to_owned())
        .on_open(open_task_list)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

// ── Rendering ──────────────────────────���─────────────────────────────────

/// Renders one picker row: the status-colored task row with the widget's
/// tree connector prepended for children (the legacy row layout).
pub fn task_list_row(entry: &TaskListTreeEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let mut line = render_task_list_row(
        entry.display_label(),
        entry.row_status(),
        ctx.is_selected,
        ctx.match_ranges,
        entry.theme(),
    );
    if !ctx.tree_prefix.is_empty() {
        let mut spans = vec![Span::styled(ctx.tree_prefix.to_owned(), ctx.tree_style)];
        spans.append(&mut line.spans);
        line = Line::from(spans);
    }
    line
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the task-list browser: fresh filter + selection, then rebuild the
/// phase/task tree from the active session's task list. Postponed tasks are
/// filtered out, matching the sidebar. Empty task lists are fine.
fn open_task_list(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.task_list_picker_mut().reset();

    let theme = state.frontend.theme.clone();
    let entries: Vec<TaskListTreeEntry> = state
        .active_session()
        .task_list()
        .phases()
        .iter()
        .flat_map(|phase| {
            let phase_id_str = format!("phase:{}", phase.id());
            let phase_entry = TaskListTreeEntry::new_phase(
                phase_id_str.clone(),
                phase.description().to_owned(),
                theme.clone(),
            );
            let task_entries: Vec<TaskListTreeEntry> = phase
                .tasks()
                .iter()
                .filter(|task| task.status() != TaskStatus::Postponed)
                .map(|task| {
                    TaskListTreeEntry::new_task(
                        format!("task:{}", task.id()),
                        Some(phase_id_str.clone()),
                        task.description().to_owned(),
                        task.status(),
                        theme.clone(),
                    )
                })
                .collect();
            std::iter::once(phase_entry).chain(task_entries)
        })
        .collect();

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::TASK_LIST_ID, entries)
            .unwrap_or_default()
    };
    state.frontend.task_list_picker_mut().set_items(wrapped);
    PickerOutcome::empty()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::intent::handle_open_picker;
    use jinn_tools_msg::TaskList;

    /// State with a two-phase task list (one phase holding a postponed task).
    fn state_with_task_list() -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let mut list = TaskList::default();
        list.add_phase("Build the parser");
        list.add_phase("Write docs");
        {
            let session = state.active_session_mut();
            *session.task_list_mut() = list;
            let phases = &mut session.task_list_mut().phases;
            phases[0].tasks.push(jinn_tools_msg::Task {
                id: jinn_tools_msg::TaskId::new_for_test("t1"),
                description: "Tokenize input".to_owned(),
                status: TaskStatus::Pending,
            });
            phases[0].tasks.push(jinn_tools_msg::Task {
                id: jinn_tools_msg::TaskId::new_for_test("t2"),
                description: "Skip postponed work".to_owned(),
                status: TaskStatus::Postponed,
            });
            phases[1].tasks.push(jinn_tools_msg::Task {
                id: jinn_tools_msg::TaskId::new_for_test("t3"),
                description: "Draft README".to_owned(),
                status: TaskStatus::Pending,
            });
        }
        state
    }

    #[rstest::rstest]
    fn open_builds_phase_task_tree_and_filters_postponed() {
        // Given a session with two phases and a postponed task.
        let mut state = state_with_task_list();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then the tree holds phase roots and visible (non-postponed) tasks.
        let entries = state.frontend.task_list_picker().items();
        let roots: Vec<&str> = entries
            .iter()
            .filter(|item| item.parent_id().is_none())
            .map(jinn_selection_widget::TreeItem::display_label)
            .collect();
        assert_eq!(roots.len(), 2, "two phase roots");
        let child_labels: Vec<&str> = entries
            .iter()
            .filter(|item| item.parent_id().is_some())
            .map(jinn_selection_widget::TreeItem::display_label)
            .collect();
        assert_eq!(
            child_labels,
            vec!["Tokenize input", "Draft README"],
            "postponed task filtered out"
        );
    }

    #[rstest::rstest]
    fn open_with_empty_task_list_opens_empty() {
        // Given a session with no tasks.
        let mut state = AppState::default_with_scope_focus();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then the picker holds zero entries.
        assert!(state.frontend.task_list_picker().items().is_empty());
    }

    #[rstest::rstest]
    fn child_row_prepends_the_tree_connector() {
        // Given a child entry and a row context carrying a tree prefix.
        let entry = TaskListTreeEntry::new_task(
            "task:1".to_owned(),
            Some("phase:0".to_owned()),
            "Tokenize input".to_owned(),
            TaskStatus::Pending,
            crate::feat::theme::default_theme(),
        );
        let ctx = RowCtx {
            is_selected: false,
            match_ranges: &[],
            tree_prefix: "\u{251c} ",
            tree_style: ratatui::style::Style::default(),
        };

        // When rendering the row.
        let line = task_list_row(&entry, &ctx);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();

        // Then the connector precedes the row content.
        assert!(
            text.starts_with("\u{251c} "),
            "connector prepended: {text:?}"
        );
        assert!(text.contains("Tokenize input"));
    }

    #[rstest::rstest]
    fn root_row_has_no_connector() {
        // Given a root entry and an empty tree prefix.
        let entry = TaskListTreeEntry::new_phase(
            "phase:0".to_owned(),
            "Build the parser".to_owned(),
            crate::feat::theme::default_theme(),
        );
        let ctx = RowCtx::flat(false, &[]);

        // When rendering the row.
        let line = task_list_row(&entry, &ctx);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();

        // Then the row starts directly with its content.
        assert!(text.contains("Build the parser"));
    }
}
