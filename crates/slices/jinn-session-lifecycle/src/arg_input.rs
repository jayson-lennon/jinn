//! Route actions and input editing for the lifecycle argument popup.
//!
//! The picker seeds this popup's cell, while this module owns everything after
//! entry: text editing, quote-aware argument parsing, validation against the
//! snapshotted command template, session creation, and close transitions.

use std::sync::Arc;

use jinn_session_lifecycle_msg::ArgInputState;
use jinn_session_lifecycle_msg::arg_input_scope;
use jinn_session_lifecycle_msg::command_template::parse_quoted_args;
use jinn_slices::KeyRoutes;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteId, RouteRow, ScopeSignal,
};

/// The lifecycle argument popup's cell.
pub type ArgInputCell = TypedCell<ArgInputState>;

/// Wraps a popup action that mutates the cell and may drive session creation.
fn action<F>(cell: &ArgInputCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &ArgInputCell) -> IntentResult + Send + Sync + 'static,
{
    let cell = cell.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &cell))
}

/// Builds a route row owned by the lifecycle argument popup.
fn row(action: &'static str, key: &'static str, display: &'static str, run: ActionFn) -> RouteRow {
    RouteRow {
        route_id: RouteId::new(action),
        scope: arg_input_scope(),
        key,
        category: "general",
        site: BindSite::OwnScope,
        feature: "session-lifecycle",
        outcome: jinn_slices::RouteOutcome::Action {
            action,
            display,
            run,
        },
    }
}

/// Attaches confirm, leave, and Ctrl-C actions to the shared route table.
pub fn attach_rows(routes: &KeyRoutes, cell: &ArgInputCell) {
    routes.attach(row(
        "confirm-lifecycle-args",
        "<enter>",
        "create the session with these arguments",
        action(cell, confirm_arg_input),
    ));
    routes.attach(row(
        "leave-lifecycle-args",
        "<esc>",
        "cancel lifecycle argument input",
        action(cell, |_ctx, cell| clear_and_leave(cell)),
    ));
    routes.attach(row(
        "clear-or-leave-lifecycle-args",
        "<c-c>",
        "clear the arguments, or leave when already empty",
        action(cell, |_ctx, cell| clear_or_leave(cell)),
    ));
}

/// Registers the popup's input hook. Every editing intent is consumed.
pub fn register_input_hook(routes: &KeyRoutes, cell: &ArgInputCell) {
    let cell = cell.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        let cell = cell.clone();
        match intent {
            EditIntent::InsertChar(ch) => {
                cell.update(|state| state.text.insert_char(*ch));
            }
            EditIntent::DeleteBackward => {
                cell.update(|state| state.text.delete());
            }
            EditIntent::DeleteForward => {
                cell.update(|state| state.text.delete_forward());
            }
            EditIntent::CursorLeft => {
                cell.update(|state| state.text.cursor_left());
            }
            EditIntent::CursorRight => {
                cell.update(|state| state.text.cursor_right());
            }
            EditIntent::CursorHome => {
                cell.update(|state| state.text.cursor_home());
            }
            EditIntent::CursorEnd => {
                cell.update(|state| state.text.cursor_end());
            }
            EditIntent::Paste(text) => {
                cell.update(|state| state.text.paste(text));
            }
        }
        Some(IntentResult::empty())
    });
    routes.register_input_hook(&arg_input_scope(), hook);
}

/// Parses the current text and validates it against the snapshotted template.
fn validated_args(state: &ArgInputState) -> Option<Vec<String>> {
    let args = if state.text.input.trim().is_empty() {
        Vec::new()
    } else {
        parse_quoted_args(&state.text.input)
    };
    (args.len() >= state.template.param_count()).then_some(args)
}

/// Confirms the popup by delegating valid input to the existing lifecycle
/// session-creation path. Invalid input is consumed while the popup stays open.
fn confirm_arg_input(ctx: &mut ActionCtx<'_>, cell: &ArgInputCell) -> IntentResult {
    let (lifecycle_name, args) = {
        let state = cell.read();
        let Some(args) = validated_args(&state) else {
            return IntentResult::empty();
        };
        (state.lifecycle_name.clone(), args)
    };

    cell.update(|state| state.text = jinn_slices::LineInput::new());
    let Some(app_state) = ctx
        .state
        .as_any_mut()
        .and_then(|state| state.downcast_mut::<jinn_kernel::AppState>())
    else {
        return IntentResult::empty();
    };

    jinn_kernel::session_lifecycle::intent::handle_session_lifecycle_setup(
        app_state,
        &lifecycle_name,
        &args,
        None,
        ctx.config,
    )
}

/// Clears the argument text and requests a conditional pop of the popup.
fn clear_and_leave(cell: &ArgInputCell) -> IntentResult {
    clear_text(cell);
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(arg_input_scope()))
}

/// Clears nonempty text while remaining open, or leaves when already empty.
fn clear_or_leave(cell: &ArgInputCell) -> IntentResult {
    let had_text = !cell.read().text.input.is_empty();
    clear_text(cell);
    if had_text {
        IntentResult::empty()
    } else {
        IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(arg_input_scope()))
    }
}

fn clear_text(cell: &ArgInputCell) {
    cell.update(|state| state.text = jinn_slices::LineInput::new());
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]

    use super::*;
    use jinn_session_lifecycle_msg::CommandTemplate;
    use jinn_session_lifecycle_msg::arg_input_slot;
    use jinn_slices::SliceActionState;

    struct FakeState {
        kernel: jinn_kernel::AppState,
    }

    impl Default for FakeState {
        fn default() -> Self {
            Self {
                kernel: jinn_kernel::AppState::default_with_scope_focus(),
            }
        }
    }

    impl SliceActionState for FakeState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            self.kernel.session.active_session_id().clone()
        }

        fn push_session_error(&mut self, _message: &str) {}

        fn active_session_cwd(&self) -> std::path::PathBuf {
            self.kernel.active_session().cwd().to_path_buf()
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> jinn_slices::PublishClosure {
            Box::new(|_sink| {})
        }

        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(&mut self.kernel as &mut dyn std::any::Any)
        }
    }

    fn action_ctx<'a>(
        state: &'a mut FakeState,
        slices: &'a jinn_slices::Slices,
        config: &'a jinn_config::ConfigLayer,
    ) -> ActionCtx<'a> {
        ActionCtx {
            state,
            slices,
            config,
            key_bytes: Vec::new(),
        }
    }

    fn cell_with(template: &str, input: &str) -> (jinn_slices::Slices, ArgInputCell) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register(
                arg_input_slot(),
                ArgInputState::new("research".to_owned(), CommandTemplate::parse(template)),
            )
            .expect("fresh registry has an empty lifecycle arg slot");
        if !input.is_empty() {
            cell.update(|state| state.text.set(input.to_owned()));
        }
        (slices, cell)
    }

    #[rstest::rstest]
    fn confirm_insufficient_arguments_preserves_popup_state() {
        // Given a popup with insufficient arguments and mutable kernel state.
        let (slices, cell) = cell_with("echo $1 $2", "only-one");
        let mut state = FakeState::default();
        let config = jinn_config::testutil::config_layer("");
        let mut ctx = action_ctx(&mut state, &slices, &config);

        // When confirmation runs.
        let result = confirm_arg_input(&mut ctx, &cell);

        // Then the input is consumed as invalid and remains available for correction.
        assert_eq!(result.message_names, [] as [&str; 0]);
        assert_eq!(cell.read().text.input, "only-one");
    }

    #[rstest::rstest]
    fn confirm_quoted_arguments_creates_lifecycle_session() {
        // Given a popup with enough quote-aware arguments and a scripted lifecycle preference.
        let (slices, cell) = cell_with("echo $1 $2", "\"two words\" tail");
        let mut state = FakeState::default();
        let config = jinn_config::testutil::config_layer(
            "[[session_lifecycle.script]]\nname = \"research\"\nsetup_command = \"echo $1 $2\"\n",
        );
        let original_session_count = state.kernel.session.session_count();
        let mut ctx = action_ctx(&mut state, &slices, &config);

        // When confirmation runs.
        let result = confirm_arg_input(&mut ctx, &cell);

        // Then session creation emits the existing ordered setup messages and stores parsed args.
        assert_eq!(result.message_names.len(), 4);
        assert_eq!(
            state.kernel.session.session_count(),
            original_session_count + 1
        );
        assert_eq!(
            state.kernel.active_session().lifecycle_args(),
            &["two words".to_owned(), "tail".to_owned()]
        );
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn rows_bind_confirm_leave_and_clear_in_input_scope() {
        // Given an empty shared route table and a popup cell.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell_with("echo $1", "value");

        // When the lifecycle popup rows are attached.
        attach_rows(&routes, &cell);

        // Then confirm, Escape, and Ctrl-C resolve as actions in the dynamic scope.
        assert!(routes.rows().iter().any(|row| {
            row.scope == arg_input_scope()
                && row.key == "<enter>"
                && matches!(&row.outcome, jinn_slices::RouteOutcome::Action { action, .. } if *action == "confirm-lifecycle-args")
        }));
        assert!(routes.rows().iter().any(|row| {
            row.scope == arg_input_scope()
                && row.key == "<esc>"
                && matches!(&row.outcome, jinn_slices::RouteOutcome::Action { action, .. } if *action == "leave-lifecycle-args")
        }));
        assert!(routes.rows().iter().any(|row| {
            row.scope == arg_input_scope()
                && row.key == "<c-c>"
                && matches!(&row.outcome, jinn_slices::RouteOutcome::Action { action, .. } if *action == "clear-or-leave-lifecycle-args")
        }));
    }

    #[rstest::rstest]
    fn input_hook_consumes_home_end_and_paste() {
        // Given a popup containing Unicode text with the cursor at its start.
        let routes = KeyRoutes::new();
        let (_slices, cell) = cell_with("echo $1", "héllo");
        cell.update(|state| state.text.cursor_home());
        register_input_hook(&routes, &cell);
        let hook = routes
            .input_hook(&arg_input_scope())
            .expect("lifecycle argument hook is registered");

        // When Home, End, and paste intents run.
        let home = hook(&EditIntent::CursorHome);
        let end = hook(&EditIntent::CursorEnd);
        let paste = hook(&EditIntent::Paste("🙂".to_owned()));

        // Then all intents are consumed and the edits are applied.
        assert!(home.is_some());
        assert!(end.is_some());
        assert!(paste.is_some());
        assert_eq!(cell.read().text.input, "héllo🙂");
    }

    #[rstest::rstest]
    fn quoted_arguments_satisfy_template() {
        // Given a two-parameter template and one quoted argument plus a bare argument.
        let (slices, cell) = cell_with("echo $1 $2", "\"two words\" tail");
        drop(slices);

        // When validation parses the popup text.
        let args = validated_args(&cell.read());

        // Then the quote-aware parser returns both required arguments.
        assert_eq!(args, Some(vec!["two words".to_owned(), "tail".to_owned()]));
    }

    #[rstest::rstest]
    fn insufficient_arguments_keep_popup_open() {
        // Given a popup with fewer arguments than its template requires.
        let (_slices, cell) = cell_with("echo $1 $2", "only-one");

        // When a state is inspected through confirmation validation.
        let args = validated_args(&cell.read());

        // Then validation rejects the input.
        assert!(args.is_none());
    }

    #[rstest::rstest]
    fn clear_or_leave_with_text_clears_and_stays_open() {
        // Given a popup with argument text.
        let (_slices, cell) = cell_with("echo $1", "value");

        // When Ctrl-C is requested.
        let result = clear_or_leave(&cell);

        // Then the text clears without requesting a scope pop.
        assert!(result.scope_signal.is_none());
        assert_eq!(cell.read().text.input, "");
    }

    #[rstest::rstest]
    fn clear_or_leave_with_empty_text_leaves() {
        // Given an empty popup.
        let (_slices, cell) = cell_with("echo $1", "");

        // When Ctrl-C is requested.
        let result = clear_or_leave(&cell);

        // Then the popup requests a conditional pop.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::PopIf(arg_input_scope()))
        );
    }

    #[rstest::rstest]
    fn escape_clears_and_leaves() {
        // Given a popup with argument text.
        let (_slices, cell) = cell_with("echo $1", "value");

        // When Escape is requested.
        let result = clear_and_leave(&cell);

        // Then the text clears and the popup requests a conditional pop.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::PopIf(arg_input_scope()))
        );
        assert_eq!(cell.read().text.input, "");
    }
}
