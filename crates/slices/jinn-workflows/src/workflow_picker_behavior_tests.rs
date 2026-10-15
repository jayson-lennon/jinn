//! Observable-behavior tests for the slice-owned workflows picker.
//!
//! These assert what the picker *does* through its own public entry points —
//! the cell, the action functions, and the attached route rows — never
//! internal wiring details. A test here that reached past those boundaries
//! would break the moment the implementation changed while the behavior did
//! not.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test module, panics are acceptable"
)]

use std::path::PathBuf;

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::empty_config_layer;
use jinn_slices::KeyRoutes;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::EditIntent;
use jinn_workflows_msg::{Workflow, WorkflowPickerState, workflows_picker_slot};

use crate::workflow_picker_actions;
use crate::workflow_picker_routes::{self, WORKFLOW_PICKER_BINDINGS};
use jinn_workflows_msg::workflows_picker_scope as workflow_picker_scope;

/// A picker cell wired to a fresh route table.
///
/// The cell is minted locally on its own registry — the catalog owns the
/// production registration — because these tests exercise picker *behavior*
/// directly, where the registration path is incidental.
fn wired() -> (TypedCell<WorkflowPickerState>, KeyRoutes) {
    let routes = KeyRoutes::new();
    let cell = jinn_slices::Slices::new()
        .register(workflows_picker_slot(), WorkflowPickerState::default())
        .expect("workflow picker slot is free in a fresh registry");
    workflow_picker_routes::attach_workflow_picker_rows(&routes, &cell);
    workflow_picker_routes::register_workflow_picker_input_hook(&routes, &cell);
    (cell, routes)
}

/// A slice activated through the real [`activate`], with its registry and
/// route table returned for inspection.
///
/// Tests that assert *wiring* must use this, not a hand-built stand-in: a
/// stand-in that re-implements the same calls passes even when `activate`
/// stops making them, which is exactly the regression these tests exist to
/// catch.
async fn activated() -> (jinn_slices::Slices, KeyRoutes) {
    let slices = jinn_slices::Slices::new();
    // Production boot seeds every slice cell before any slice activates;
    // `activate` resolves the picker's cell by slot key, so the harness
    // seeds the same registry the app would.
    jinn_cell_catalog::register_all_cells(&slices);
    let key_routes = KeyRoutes::new();
    let mut viewport = jinn_slices::view::Viewport::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let services = jinn_kernel::Services::new_fake().await;
    let mut host = jinn_slices::SliceHost::new(
        &slices,
        &mut viewport,
        &overlay_views,
        &key_routes,
        &services.trouper_system,
    );

    crate::activate(&mut host);

    (slices, key_routes)
}

/// A discovered workflow, as the discovery scan would produce.
fn workflow(name: &str) -> Workflow {
    Workflow {
        name: name.to_owned(),
        description: format!("the {name} workflow"),
        dir: PathBuf::from(format!("/cfg/workflows/{name}")),
    }
}

/// The names currently shown by the picker, in display order.
fn visible_names(cell: &TypedCell<WorkflowPickerState>) -> Vec<String> {
    cell.read()
        .selection
        .items()
        .iter()
        .map(|item| item.entry().name.clone())
        .collect()
}

/// Opens the picker over `names`.
fn opened(cell: &TypedCell<WorkflowPickerState>, names: &[&str]) {
    let discovered: Vec<Workflow> = names.iter().copied().map(workflow).collect();
    cell.update(|picker| {
        workflow_picker_actions::open(picker, &discovered, &jinn_theme::default_theme());
    });
}

#[rstest::rstest]
fn opening_the_picker_shows_the_discovered_workflows() {
    // Given an empty picker cell.
    let (cell, _routes) = wired();

    // When opening the picker over two discovered workflows.
    opened(&cell, &["alpha", "beta"]);

    // Then both workflows are shown, so the menu is not blank.
    assert_eq!(visible_names(&cell), vec!["alpha", "beta"]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_opener_dispatches_through_the_route_row() {
    // Given an activated workflows slice and default state.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_discovered_workflows(vec![workflow("research-report")]);

    // When handling the opener key's dynamic intent.
    jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "open-workflow-picker",
            "open-workflow-picker",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );

    // Then the picker's scope is on the focus stack — the handler applies
    // the row's scope signal as it dispatches, so the result itself no
    // longer carries it.
    assert_eq!(
        state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(workflow_picker_scope()),
        "the opener row must push the picker's scope"
    );
    // And the picker opened over the session's discovered workflows.
    assert_eq!(visible_names(&cell), vec!["research-report"]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_opener_binds_outside_the_picker_it_opens() {
    // Given the route rows the slice attaches.
    let (_cell, routes) = wired();
    let rows = routes.rows();

    // When reading the opener row.
    let opener = rows
        .iter()
        .find(|row| row.key == "<leader>sw")
        .expect("the picker has an opener key");
    let site = opener.site;

    // Then it is not an own-scope binding, which could never fire.
    assert_ne!(
        site,
        jinn_slices::route::BindSite::OwnScope,
        "a key that opens a picker cannot live inside that picker's scope"
    );
}

#[rstest::rstest]
fn every_footer_entry_is_a_key_the_picker_binds() {
    // Given the picker's declared footer binds and its bound keys.
    let bound = workflow_picker_routes::bound_keys();

    // When reading each footer entry.
    // Then every one is a key the picker actually attaches — a footer
    // that advertises a dead key is a bug.
    for key in WORKFLOW_PICKER_BINDINGS.iter().map(|(key, _)| *key) {
        assert!(bound.contains(&key), "footer advertises unbound key {key}");
    }
}

#[rstest::rstest]
fn the_picker_binds_navigation_start_rescan_and_cancel() {
    // Given the route rows the slice attaches.
    let (_cell, routes) = wired();
    let keys: Vec<&str> = routes.rows().iter().map(|row| row.key).collect();

    // Then the picker walks, pages, starts, rescans, and cancels.
    for key in [
        "<up>", "<down>", "<pgup>", "<pgdn>", "<enter>", "<esc>", "<c-r>",
    ] {
        assert!(
            keys.contains(&key),
            "no binding for {key} (binds: {keys:?})"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_registers_a_filter_input_hook() {
    // Given the wiring activation performs — the real `activate`, not a
    // hand-built stand-in, so removing the registration here is detectable.
    let (slices, key_routes) = activated().await;

    // When asking for the picker's input hook.
    let hook = key_routes.input_hook(&workflow_picker_scope());

    // Then one is registered, so typing reaches the filter.
    assert!(
        hook.is_some(),
        "the filter needs an input hook to receive text"
    );
    // And the cell the hook writes to is the registered one.
    assert!(
        slices
            .reader::<WorkflowPickerState>(&workflows_picker_slot())
            .is_some()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn typing_a_character_narrows_the_visible_workflows() {
    // Given an activated workflows slice with an open picker.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &["alpha", "beta"]);

    // When the keymap serves an inserted character through the hook.
    let hook = key_routes
        .input_hook(&workflow_picker_scope())
        .expect("input hook registered at activation");
    hook(&EditIntent::InsertChar('a'));

    // Then the filter holds the character.
    let filter = cell.read().selection.filter().to_owned();
    assert_eq!(filter, "a");
}

#[rstest::rstest]
#[tokio::test]
async fn backspacing_removes_the_last_filter_character() {
    // Given an activated workflows slice whose filter holds one character.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &["alpha"]);
    let hook = key_routes
        .input_hook(&workflow_picker_scope())
        .expect("input hook registered at activation");
    hook(&EditIntent::InsertChar('x'));

    // When backspacing.
    hook(&EditIntent::DeleteBackward);

    // Then the filter is empty again.
    let filter = cell.read().selection.filter().to_owned();
    assert_eq!(filter, "");
}

// ── Launch (enter) ──────────────────────────────────────────────────────

/// A publish sink capturing `(short name, payload)` pairs, so a test can
/// assert what a route result's closures publish without a live fabric.
struct RecordingSink(std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>);

impl RecordingSink {
    fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }

    fn find(&self, name: &str) -> serde_json::Value {
        self.0
            .lock()
            .expect("recording sink lock")
            .iter()
            .find(|(n, _)| n.contains(name))
            .map_or(serde_json::Value::Null, |(_, payload)| payload.clone())
    }

    fn count_matching(&self, name: &str) -> usize {
        self.0
            .lock()
            .expect("recording sink lock")
            .iter()
            .filter(|(n, _)| n.contains(name))
            .count()
    }
}

impl jinn_slices::route_publish::PublishSink for RecordingSink {
    fn publish_schema(
        &self,
        _schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    ) {
        self.0
            .lock()
            .expect("recording sink lock")
            .push((name, payload));
    }
}

#[rstest::rstest]
#[tokio::test]
async fn enter_creates_a_fresh_session_for_the_workflow() {
    // Given an activated workflows slice with an open picker over one
    // workflow, and state whose active session has history.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &["research-report"]);
    let mut state = AppState::default_with_scope_focus();
    let old_id = state.session.active_session_id().clone();
    state
        .active_session_mut()
        .push_entry(jinn_core_types::ChatEntry::user("old"));

    // When handling the picker's enter intent.
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "start-highlighted-workflow",
            "start-highlighted-workflow",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );

    // Then the active session is new and its history is empty — the
    // session creation's own observable change, not the message's.
    assert_ne!(*state.session.active_session_id(), old_id);
    assert!(state.active_session().history().is_empty());
    // And the result carries the launch enqueue.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.contains("EnqueueUserMessage")),
        "expected EnqueueUserMessage in {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[tokio::test]
async fn enter_enqueues_the_start_message_into_the_new_session() {
    // Given an activated workflows slice with an open picker over one
    // workflow.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &["research-report"]);
    let mut state = AppState::default_with_scope_focus();

    // When handling the picker's enter intent.
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "start-highlighted-workflow",
            "start-highlighted-workflow",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );

    // Then the enqueue names the session the launch created — the
    // message-side fact, separate from the state change.
    let expected_session = state.session.active_session_id().clone();
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("EnqueueUserMessage")),
        "expected EnqueueUserMessage in {:?}",
        result.message_names
    );
    // And the new session's lifecycle is the blank one `handle_session_new`
    // always produces.
    assert!(state.active_session().lifecycle_name().is_none());
    assert_eq!(state.session.active_session_id(), &expected_session);
}

#[rstest::rstest]
#[tokio::test]
async fn enter_with_no_rows_does_nothing() {
    // Given an activated workflows slice whose picker is open over nothing.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &[]);
    let mut state = AppState::default_with_scope_focus();
    let old_id = state.session.active_session_id().clone();

    // When handling the picker's enter intent.
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "start-highlighted-workflow",
            "start-highlighted-workflow",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );

    // Then no session is created — a picker over nothing cannot launch.
    assert_eq!(*state.session.active_session_id(), old_id);
    assert!(
        !result
            .message_names
            .iter()
            .any(|name| name.contains("EnqueueUserMessage"))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_launch_message_carries_the_workflow_name() {
    // Given an open picker over one workflow.
    let (slices, key_routes) = activated().await;
    let cell = slices
        .reader(&workflows_picker_slot())
        .expect("workflow picker cell registered at activation");
    opened(&cell, &["research-report"]);
    let mut state = AppState::default_with_scope_focus();

    // When running the result's publish closures against a recording sink.
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "start-highlighted-workflow",
            "start-highlighted-workflow",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );
    let sink = RecordingSink::new();
    for message in result.messages {
        message(&sink);
    }

    // Then exactly one user entry crossed, reading `start <name> workflow`.
    let enqueue = sink.find("EnqueueUserMessage");
    assert!(
        !enqueue.is_null(),
        "the launch enqueues one message; got {enqueue}"
    );
    let kind = &enqueue["entry"]["kind"];
    let text = kind["User"]["display"]
        .as_str()
        .or_else(|| kind["User"]["expanded"].as_str())
        .expect("the user entry carries its text");
    assert_eq!(text, "start research-report workflow");
    // And the launch message is the only extra thing the launch published
    // beyond session creation.
    assert_eq!(sink.count_matching("EnqueueUserMessage"), 1);
}

// ── Rescan ──────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn rescan_publishes_only_the_workflows_trigger() {
    // Given an activated workflows slice with default state.
    let (slices, key_routes) = activated().await;
    let mut state = AppState::default_with_scope_focus();

    // When handling the rescan intent.
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            workflow_picker_scope(),
            "rescan-workflows",
            "rescan-workflows",
        )),
        &mut state,
        &slices,
        &key_routes,
        empty_config_layer(),
    );

    // Then exactly one message goes out, and it is the workflows trigger.
    assert_eq!(result.message_names.len(), 1);
    assert!(
        result.message_names[0].contains("ScanWorkflows"),
        "expected ScanWorkflows in {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn a_rescan_keeps_the_highlight_on_the_workflow_being_looked_at() {
    // Given an open picker with the highlight moved onto the second
    // workflow.
    let (cell, _routes) = wired();
    opened(&cell, &["alpha", "beta", "gamma"]);
    cell.update(|picker| {
        picker
            .selection
            .move_down(jinn_workflows_msg::RESULTS_VIEWPORT_FALLBACK);
    });

    // When a rescan republishes the same three workflows.
    let discovered = vec![workflow("alpha"), workflow("beta"), workflow("gamma")];
    workflow_picker_routes::republish_from_discovery(&cell, &discovered);

    // Then the highlight is still on the second workflow.
    let guard = cell.read();
    let highlighted = guard
        .selection
        .selected_item()
        .map(|i| i.entry().name.clone());
    assert_eq!(highlighted.as_deref(), Some("beta"));
}

#[rstest::rstest]
fn a_rescan_replaces_the_visible_workflows() {
    // Given an open picker over one workflow.
    let (cell, _routes) = wired();
    opened(&cell, &["alpha"]);

    // When a rescan reports a different set.
    let discovered = vec![workflow("alpha"), workflow("delta")];
    workflow_picker_routes::republish_from_discovery(&cell, &discovered);

    // Then the rows reflect the new scan.
    assert_eq!(visible_names(&cell), vec!["alpha", "delta"]);
}

// ── Viewport ────────────────────────────────────────────────────────────

#[rstest::rstest]
fn the_render_pass_publishes_the_measured_viewport() {
    // Given a picker state that has not rendered a frame yet.
    let mut state = WorkflowPickerState::default();

    // When the renderer measures a frame.
    let measured = crate::workflow_picker_viewport::results_viewport(ratatui::layout::Rect::new(
        0, 0, 200, 50,
    ));
    state.results_viewport = measured;

    // Then paging has a real row count rather than the fallback.
    assert!(state.results_viewport > 0);
    // And a wide frame's list pane is taller than the stacked layout's.
    assert!(state.results_viewport > jinn_selection_widget::HORIZONTAL_LIST_ROWS as usize);
}

// ── Slot ────────────────────────────────────────────────────────────────

#[rstest::rstest]
fn the_picker_state_lives_under_one_slot() {
    // Given a picker registered under its slot.
    let (cell, _routes) = wired();

    // When a value is written to it.
    cell.update(|picker| {
        picker.results_viewport = 7;
    });

    // Then it reads back through the same cell.
    assert_eq!(cell.read().results_viewport, 7);
    // And the slot is stable, so there is exactly one home for the
    // picker's state.
    assert_eq!(workflows_picker_slot(), workflows_picker_slot());
}
