//! Project-add popup route actions and input hook.
//!
//! Open (bound in the static `Picker(project)` scope) seeds the popup from
//! the active session's cwd and pushes the popup's dynamic scope; confirm
//! resolves the typed path with the shared resolver, writes the optimistic
//! `projects` append into the app state through
//! [`jinn_slices::SliceActionState::as_any_mut`], and persists through the
//! configuration layer's `put_list`; leave discards. Editing lands in the
//! cell through a route-table input hook — the same pattern as the cwd
//! popup.

use jinn_cwd_msg::{CwdResolution, resolve_cwd_input};
use jinn_preferences_config::schemas::ProjectConfig;
use jinn_project_msg::ProjectAddInputState;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::SliceScopeId;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteId, RouteRow, ScopeSignal,
};
use std::sync::Arc;

/// The project-add popup's dynamic scope (input-capturing).
///
/// Also the popup's identity in the which-key/which-scope surfaces.
#[must_use]
pub fn project_add_scope() -> SliceScopeId {
    SliceScopeId::new("project", "project_add")
}

/// The state the actions and hook touch: the popup's single cell.
type ProjectAddCell = TypedCell<ProjectAddInputState>;

/// Wraps a synchronous project-add action (which also touches the cell)
/// into an [`ActionFn`].
fn action<F>(cell: &ProjectAddCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &ProjectAddCell) -> IntentResult + Send + Sync + 'static,
{
    let cell = cell.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &cell))
}

/// Builds one `Action` row binding `key` in the popup scope.
fn row(
    action_name: &'static str,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        // Route ids are only diagnostics/composition keys here; actions
        // dispatch by (scope, action).
        route_id: RouteId::new(action_name),
        scope: project_add_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "project",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: action_name,
            display,
            run,
        },
    }
}

/// Attaches the popup's route rows: confirm (`<enter>`) and leave (`<esc>`),
/// plus the opener (`<c-n>` in the static `Picker(project)` scope — the
/// popup scope does not exist yet when that key is pressed, so the opener
/// binds there via the `StaticScopes` site and pushes the popup scope
/// itself).
pub fn attach_project_add_rows(routes: &jinn_slices::KeyRoutes, cell: &ProjectAddCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("project-add:open"),
        scope: project_add_scope(),
        key: "<c-n>",
        category: "project",
        site: BindSite::StaticScopes(&["Picker(project)"]),
        feature: "project",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: "open-project-add",
            display: "add dir",
            run: action(cell, |ctx, cell| {
                open_project_add(ctx.state, cell);
                IntentResult::empty().with_scope_signal(ScopeSignal::Push(project_add_scope()))
            }),
        },
    });

    routes.attach(row(
        "confirm-project-add",
        "<enter>",
        "project",
        "register the typed directory as a project",
        action(cell, confirm_project_add),
    ));
    routes.attach(row(
        "leave-project-add",
        "<esc>",
        "project",
        "cancel without registering a project",
        action(cell, |_ctx, cell| {
            leave_project_add(cell);
            IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(project_add_scope()))
        }),
    ));
}

/// Registers the popup's editing hook: every editing intent lands in the
/// cell; non-editing intents pass through (`None`).
pub fn register_project_add_input_hook(routes: &jinn_slices::KeyRoutes, cell: &ProjectAddCell) {
    let cell = cell.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        let cell = cell.clone();
        let result = match intent {
            EditIntent::InsertChar(ch) => {
                cell.update(|s| s.text.insert_char(*ch));
                IntentResult::empty()
            }
            EditIntent::DeleteBackward => {
                cell.update(|s| s.text.delete());
                IntentResult::empty()
            }
            EditIntent::DeleteForward => {
                cell.update(|s| s.text.delete_forward());
                IntentResult::empty()
            }
            EditIntent::CursorLeft => {
                cell.update(|s| s.text.cursor_left());
                IntentResult::empty()
            }
            EditIntent::CursorRight => {
                cell.update(|s| s.text.cursor_right());
                IntentResult::empty()
            }
            EditIntent::Paste(text) => {
                cell.update(|s| s.text.paste(text));
                IntentResult::empty()
            }
            EditIntent::CursorHome | EditIntent::CursorEnd => return None,
        };
        Some(result)
    });
    routes.register_input_hook(&project_add_scope(), hook);
}

/// Opens the popup: seeds the input with the active session's cwd
/// (tilde-compressed, cursor at end). The scope push rides the route
/// result; seeding happens here so the cell is populated before the
/// first render.
pub(super) fn open_project_add(
    state: &mut dyn jinn_slices::SliceActionState,
    cell: &ProjectAddCell,
) -> IntentResult {
    let seeded = jinn_common::shorten_path(&state.active_session_cwd());
    cell.update(|s| {
        let mut text = jinn_slices::LineInput::new();
        text.set(seeded);
        *s = ProjectAddInputState { text };
    });
    IntentResult::empty().with_scope_signal(ScopeSignal::Push(project_add_scope()))
}

/// Confirms the popup: resolves the typed path against the active session
/// cwd; on success appends the path to the `[[project.entry]]` section
/// through the configuration layer, then pops the scope and clears the
/// cell. On failure stays open (the render footer shows the inline error)
/// and consumes the key.
pub(super) fn confirm_project_add(ctx: &mut ActionCtx<'_>, cell: &ProjectAddCell) -> IntentResult {
    let raw = cell.read().text.input.trim().to_owned();
    let current_cwd = ctx.state.active_session_cwd();
    match resolve_cwd_input(&raw, &current_cwd) {
        CwdResolution::Ok(path) => {
            add_project(ctx.config, path);
            leave_project_add(cell);
            IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(project_add_scope()))
        }
        CwdResolution::Empty | CwdResolution::NotADir(_) => IntentResult::empty(),
    }
}

/// Appends a project to the `[[project.entry]]` section, deduping by
/// path so a repeated add is a no-op rather than a duplicate entry.
///
/// A failed write is logged, not surfaced: the popup has already
/// confirmed, and refusing to close over a disk error would strand the
/// user in an input they can no longer act on.
fn add_project(config: &jinn_preferences_config::ConfigLayer, path: std::path::PathBuf) {
    let mut projects = config.get_list::<ProjectConfig>().unwrap_or_default();
    if projects.iter().any(|project| project.path == path) {
        return;
    }
    projects.push(ProjectConfig { path });
    if let Err(error) = config.put_list::<ProjectConfig>(&projects) {
        tracing::warn!(err = ?error, "failed to persist the added project to jinn.toml");
    }
}

/// Cancels the popup: clears the cell (the scope pop rides the route
/// result's `PopIf` when confirming; leave is dispatched from the popup
/// scope, so it pops explicitly).
pub(super) fn leave_project_add(cell: &ProjectAddCell) {
    cell.update(|s| *s = ProjectAddInputState::default());
}
