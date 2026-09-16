//! Cwd popup route actions and input hook.
//!
//! Open seeds the popup from the active session's cwd and pushes the popup's
//! dynamic scope; confirm resolves the typed path with the shared resolver
//! and publishes the kernel's `SetSessionCwd` through the
//! [`SliceActionState`] capability; leave discards. Editing lands in the
//! cell through a route-table input hook — the same pattern as the rename
//! popup.

use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, PublishClosure, RouteId, RouteRow,
    ScopeSignal,
};
use jinn_slices::{CwdInputState, CwdResolution, SliceScopeId, resolve_cwd_input, shorten_path};
use std::sync::Arc;

/// The cwd popup's dynamic scope (input-capturing).
#[must_use]
pub fn cwd_scope() -> SliceScopeId {
    SliceScopeId::new("cwd", "input")
}

/// The state the actions and hook touch: the popup's single cell.
type CwdCell = TypedCell<CwdInputState>;

/// Wraps a synchronous cwd action (which also touches the cell) into an
/// [`ActionFn`].
fn action<F>(cell: &CwdCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &CwdCell) -> IntentResult + Send + Sync + 'static,
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
        scope: cwd_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "cwd",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: action_name,
            display,
            run,
        },
    }
}

/// Attaches the popup's route rows: confirm (`<enter>`) and leave (`<esc>`).
///
/// The opener row (`<leader>cd` in Normal) is attached here too — it is an
/// `OwnScope` binding on the *base* scope is not expressible, so it uses the
/// `StaticScopes` site on `["Normal"]` and pushes the popup scope itself.
pub fn attach_cwd_rows(routes: &jinn_slices::KeyRoutes, cell: &CwdCell) {
    // Opener: seed + push scope. Site: static Normal scope (the popup scope
    // does not exist yet when the key is pressed).
    routes.attach(RouteRow {
        route_id: RouteId::new("cwd:open"),
        scope: cwd_scope(),
        key: "<leader>cd",
        category: "session",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "cwd",
        outcome: jinn_slices::route::RouteOutcome::Action {
            action: "open-cwd-input",
            display: "change session cwd (type a path)",
            run: action(cell, |ctx, cell| open_cwd_input(ctx.state, cell)),
        },
    });

    routes.attach(row(
        "confirm-cwd-input",
        "<enter>",
        "session",
        "resolve the path and change the session cwd",
        action(cell, confirm_cwd_input),
    ));
    routes.attach(row(
        "leave-cwd-input",
        "<esc>",
        "session",
        "cancel without changing the cwd",
        action(cell, |_ctx, cell| {
            leave_cwd_input(cell);
            IntentResult {
                messages: Vec::new(),
                message_names: Vec::new(),
                scope_signal: Some(ScopeSignal::PopIf(cwd_scope())),
            }
        }),
    ));
}

/// Registers the popup's editing hook: every editing intent lands in the
/// cell; non-editing intents pass through (`None`).
pub fn register_cwd_input_hook(routes: &jinn_slices::KeyRoutes, cell: &CwdCell) {
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
    routes.register_input_hook(&cwd_scope(), hook);
}

/// Opens the popup: seeds the input with the active session's cwd
/// (tilde-compressed, cursor at end) and pushes the popup scope.
fn open_cwd_input(state: &mut dyn jinn_slices::SliceActionState, cell: &CwdCell) -> IntentResult {
    let seeded = shorten_path(&state.active_session_cwd());
    cell.update(|s| {
        let mut text = jinn_slices::LineInput::new();
        text.set(seeded);
        *s = CwdInputState { text };
    });
    IntentResult {
        messages: Vec::new(),
        message_names: Vec::new(),
        scope_signal: Some(ScopeSignal::Push(cwd_scope())),
    }
}

/// Confirms the popup: resolves the typed path against the active session
/// cwd; on success publishes `SetSessionCwd` (via the capability) and pops
/// the scope, clearing the cell. On failure stays open (the render footer
/// shows the inline error) and consumes the key.
fn confirm_cwd_input(ctx: &mut ActionCtx<'_>, cell: &CwdCell) -> IntentResult {
    let raw = cell.read().text.input.trim().to_owned();
    let current_cwd = ctx.state.active_session_cwd();
    match resolve_cwd_input(&raw, &current_cwd) {
        CwdResolution::Ok(path) => {
            let session_id = ctx.state.active_session_id();
            let publish: PublishClosure = ctx.state.publish_session_cwd(session_id, path);
            leave_cwd_input(cell);
            IntentResult {
                messages: vec![publish],
                message_names: vec!["SetSessionCwd"],
                scope_signal: Some(ScopeSignal::PopIf(cwd_scope())),
            }
        }
        CwdResolution::Empty | CwdResolution::NotADir(_) => IntentResult::empty(),
    }
}

/// Cancels the popup: clears the cell (the scope pop rides the route
/// result's `PopIf` when confirming; leave is dispatched from the popup
/// scope, so it pops explicitly).
fn leave_cwd_input(cell: &CwdCell) {
    cell.update(|s| *s = CwdInputState::default());
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::cwds_slot;
    use jinn_slices::KeyRoutes;
    use jinn_slices::SliceActionState;
    use jinn_slices::route::ActionCtx;

    /// A minimal kernel-free [`SliceActionState`] double.
    #[derive(Default)]
    struct FakeState {
        cwd: std::path::PathBuf,
        session_id: jinn_core_types::SessionId,
    }

    impl SliceActionState for FakeState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            self.session_id.clone()
        }

        fn push_session_error(&mut self, _message: &str) {}

        fn active_session_cwd(&self) -> std::path::PathBuf {
            self.cwd.clone()
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> PublishClosure {
            Box::new(|_bus| {})
        }
    }

    fn ctx<'a>(state: &'a mut FakeState, slices: &'a jinn_slices::Slices) -> ActionCtx<'a> {
        ActionCtx { state, slices }
    }

    /// Mints the popup cell in a fresh registry; returns both (the cell is a
    /// clone handle, so they can be used concurrently).
    fn cell() -> (jinn_slices::Slices, CwdCell) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(cwds_slot(), CwdInputState::default())
            .expect("fresh test registry has the slot free");
        (slices, cell)
    }

    #[rstest::rstest]
    fn open_seeds_input_from_session_cwd_and_pushes_scope() {
        // Given a fake state whose session cwd is an absolute path.
        let mut state = FakeState::default();
        state.cwd = std::path::PathBuf::from("/tmp/some-project");
        let (slices, cell) = cell();
        let cx = ctx(&mut state, &slices);

        // When the opener action runs.
        let result = open_cwd_input(cx.state, &cell);

        // Then the cell is seeded with the cwd (no tilde: not under $HOME in
        // the test env) and the result pushes the popup scope.
        assert_eq!(cell.read().text.input, "/tmp/some-project");
        assert_eq!(cell.read().text.cursor_pos, "/tmp/some-project".len());
        assert!(matches!(result.scope_signal, Some(ScopeSignal::Push(_))));
    }

    #[rstest::rstest]
    fn open_seeds_tilde_compressed_path_when_cwd_under_home() {
        // Given a session cwd under $HOME.
        let mut state = FakeState::default();
        let home = dirs::home_dir().expect("home dir exists");
        state.cwd = home.join("projects/my-app");
        let (slices, cell) = cell();
        let cx = ctx(&mut state, &slices);

        // When the opener action runs.
        open_cwd_input(cx.state, &cell);

        // Then the seeded input is the tilde-compressed form.
        assert_eq!(cell.read().text.input, "~/projects/my-app");
    }

    #[rstest::rstest]
    fn confirm_valid_relative_dir_publishes_and_pops() {
        // Given a real tempdir next to the session cwd, with the popup seeded
        // with the tempdir's basename.
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path();
        let mut state = FakeState::default();
        state.cwd = target.parent().expect("parent").to_path_buf();
        let (slices, cell) = cell();
        cell.update(|s| {
            s.text.set(
                target
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .to_string(),
            )
        });
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then one SetSessionCwd is published and the scope pops.
        assert_eq!(result.message_names, vec!["SetSessionCwd"]);
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(result.scope_signal, Some(ScopeSignal::PopIf(_))));
        // And the cell is cleared.
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn confirm_nonexistent_path_stays_open_unchanged() {
        // Given a popup seeded with a path that does not exist.
        let mut state = FakeState::default();
        state.cwd = std::path::PathBuf::from("/tmp");
        let (slices, cell) = cell();
        cell.update(|s| s.text.set("/this/does/not/exist".to_owned()));
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then nothing is published, no scope signal fires, and the input
        // stays for the footer to show the inline error.
        assert!(result.messages.is_empty());
        assert!(result.scope_signal.is_none());
        assert_eq!(cell.read().text.input, "/this/does/not/exist");
    }

    #[rstest::rstest]
    fn confirm_empty_input_is_noop() {
        // Given a popup with empty input.
        let mut state = FakeState::default();
        state.cwd = std::path::PathBuf::from("/tmp");
        let (slices, cell) = cell();
        let mut cx = ctx(&mut state, &slices);

        // When confirming.
        let result = confirm_cwd_input(&mut cx, &cell);

        // Then nothing is published and the popup stays open.
        assert!(result.messages.is_empty());
        assert!(result.scope_signal.is_none());
    }

    #[rstest::rstest]
    fn leave_clears_the_cell() {
        // Given a popup with typed text.
        let state = FakeState::default();
        let (slices, cell) = cell();
        cell.update(|s| s.text.set("/some/path".to_owned()));

        // When leaving.
        leave_cwd_input(&cell);

        // Then the cell is cleared.
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn rows_bind_open_confirm_and_leave() {
        // Given a route table with the cwd rows attached.
        let routes = KeyRoutes::new();
        let (slices, cell) = cell();
        attach_cwd_rows(&routes, &cell);

        // When enumerating the rows.
        let rows = routes.rows();

        // Then the opener binds <leader>cd on the static Normal scopes and
        // confirm/leave bind <enter>/<esc> in the popup's own scope.
        let open = rows
            .iter()
            .find(|row| row.route_id.as_str() == "cwd:open")
            .expect("opener row");
        assert_eq!(open.key, "<leader>cd");
        assert!(matches!(open.site, BindSite::StaticScopes(_)));
        let confirm = rows
            .iter()
            .find(|row| row.route_id.as_str() == "confirm-cwd-input")
            .expect("confirm row");
        assert_eq!(confirm.key, "<enter>");
        assert!(matches!(confirm.site, BindSite::OwnScope));
        let leave = rows
            .iter()
            .find(|row| row.route_id.as_str() == "leave-cwd-input")
            .expect("leave row");
        assert_eq!(leave.key, "<esc>");
    }

    #[rstest::rstest]
    fn input_hook_edits_the_cell_and_ignores_non_edit_intents() {
        // Given a route table with the input hook registered.
        let routes = KeyRoutes::new();
        let (slices, cell) = cell();
        register_cwd_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&cwd_scope())
            .expect("hook registered for the cwd scope");

        // When editing intents flow through the hook.
        let _ = hook(&EditIntent::InsertChar('a'));
        let _ = hook(&EditIntent::InsertChar('b'));
        let _ = hook(&EditIntent::CursorLeft);
        let _ = hook(&EditIntent::InsertChar('x'));

        // Then the cell reflects the edits (xab with cursor before b).
        assert_eq!(cell.read().text.input, "axb");

        // And non-edit intents pass through untouched.
        assert!(hook(&EditIntent::CursorHome).is_none());
    }
}
