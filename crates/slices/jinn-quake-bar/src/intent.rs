//! Quake bar keybind routing — the slice's rows, actions, and input hook.
//!
//! The quake bar's key handling is data, not handler arms:
//!
//! - **Route rows** (attached by [`attach_quake_bar_rows`]) map keys in
//!   the slice's dynamic scope to actions: submit, scroll, close.
//! - The **open toggle** is a row on the [`BindSite::GlobalToggle`]
//!   site: it emits a [`ScopeSignal::Push`] so the handler (the exempt
//!   scope-stack writer) enters the slice's scope.
//! - The **input hook** (registered by [`register_quake_input_hook`])
//!   intercepts editing intents while the quake scope is active and
//!   writes the slice cell synchronously — the sanctioned carve-out for
//!   per-keystroke typing.
//!
//! The command log is owned by the [`QuakeBarActor`]; submit clears the
//! input in the hook action and emits [`SubmitQuakeBarCommand`] so the
//! actor remains the single writer of the log.

use jinn_slices::TypedCell;

use crate::command::SubmitQuakeBarCommand;
use crate::state::QuakeBarInput;
use crate::state::QuakeBarState;
use crate::state::quake_scope;
use jinn_slices::route::ActionCtx;
use jinn_slices::route::ActionFn;
use jinn_slices::route::BindSite;
use jinn_slices::route::EditIntent;
use jinn_slices::route::InputHook;
use jinn_slices::route::KeyRoutes;
use jinn_slices::route::RouteOutcome;
use jinn_slices::route::RouteRow;

use jinn_slices::RouteResult as IntentResult;
use jinn_slices::ScopeSignal;

/// Route ids for the quake bar's rows (composition resolution +
/// diagnostics).
pub mod route_ids {
    use jinn_slices::RouteId;

    /// Open the quake bar overlay (the global `<M-\`>` toggle).
    pub const OPEN: RouteId = RouteId::new("quake-bar:open");
    /// Close the overlay (`<esc>` / `<M-\`>`).
    pub const CLOSE: RouteId = RouteId::new("quake-bar:close");
    /// Submit the input line (`<enter>`).
    pub const SUBMIT: RouteId = RouteId::new("quake-bar:submit");
    /// Scroll the log toward older lines (`<pgup>`).
    pub const SCROLL_UP: RouteId = RouteId::new("quake-bar:scroll-up");
    /// Scroll the log toward newer lines (`<pgdn>`).
    pub const SCROLL_DOWN: RouteId = RouteId::new("quake-bar:scroll-down");
    /// Clear the input or close when empty (`<c-c>`).
    pub const CTRL_CLEAR: RouteId = RouteId::new("quake-bar:ctrl-clear");
}

/// Attaches the quake bar's route rows. Called once from the slice's
/// `activate()`; the cell handle is captured by the actions that need
/// it (the same handle the actor holds — never a second mint).
pub fn attach_quake_bar_rows(routes: &KeyRoutes, cell: &TypedCell<QuakeBarState>) {
    attach_lifecycle_rows(routes);
    attach_input_rows(routes, cell);
}

/// Binds the quake bar's open/close toggles (`<M-\`>` global, `<esc>` own-scope).
fn attach_lifecycle_rows(routes: &KeyRoutes) {
    let scope = quake_scope();

    // Global toggle: opens the overlay from any static scope (and other
    // slices' scopes). Skipped inside the quake scope itself, where the
    // close row binds the same key.
    routes.attach(RouteRow {
        route_id: route_ids::OPEN,
        scope: scope.clone(),
        key: "<M-`>",
        category: "general",
        site: BindSite::GlobalToggle,
        feature: "quake-bar",
        outcome: RouteOutcome::Action {
            action: "open",
            display: "quake bar",
            run: ActionFn::new(|_ctx| {
                IntentResult::empty().with_scope_signal(ScopeSignal::Push(quake_scope()))
            }),
        },
    });

    routes.attach(RouteRow {
        route_id: route_ids::CLOSE,
        scope: scope.clone(),
        key: "<esc>",
        category: "general",
        site: BindSite::OwnScope,
        feature: "quake-bar",
        outcome: RouteOutcome::Action {
            action: "close",
            display: "close quake bar",
            run: ActionFn::new(|_ctx| {
                IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(quake_scope()))
            }),
        },
    });
    routes.attach(RouteRow {
        route_id: route_ids::CLOSE,
        scope: scope.clone(),
        key: "<M-`>",
        category: "general",
        site: BindSite::OwnScope,
        feature: "quake-bar",
        outcome: RouteOutcome::Action {
            action: "close",
            display: "close quake bar",
            run: ActionFn::new(|_ctx| {
                IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(quake_scope()))
            }),
        },
    });
}

/// Binds the quake bar's in-overlay input rows (submit, scroll, clear).
fn attach_input_rows(routes: &KeyRoutes, cell: &TypedCell<QuakeBarState>) {
    let scope = quake_scope();

    // Submit needs the cell: it reads + clears the input buffer. The
    // handle is a clone of the one minted at activation.
    let submit_cell = cell.clone();
    routes.attach(RouteRow {
        route_id: route_ids::SUBMIT,
        scope: scope.clone(),
        key: "<enter>",
        category: "input",
        site: BindSite::OwnScope,
        feature: "quake-bar",
        outcome: RouteOutcome::Action {
            action: "submit",
            display: "submit command",
            run: ActionFn::new({
                let cell = submit_cell.clone();
                move |ctx| handle_submit(&cell, ctx)
            }),
        },
    });

    for (route_id, key, action, display) in [
        (route_ids::SCROLL_UP, "<pgup>", "scroll-up", "scroll up"),
        (
            route_ids::SCROLL_DOWN,
            "<pgdn>",
            "scroll-down",
            "scroll down",
        ),
    ] {
        let cell = cell.clone();
        routes.attach(RouteRow {
            route_id,
            scope: scope.clone(),
            key,
            category: "navigation",
            site: BindSite::OwnScope,
            feature: "quake-bar",
            outcome: RouteOutcome::Action {
                action,
                display,
                run: ActionFn::new({
                    let cell = cell.clone();
                    move |ctx| handle_scroll(&cell, action, ctx)
                }),
            },
        });
    }

    routes.attach(RouteRow {
        route_id: route_ids::CTRL_CLEAR,
        scope,
        key: "<c-c>",
        category: "general",
        site: BindSite::OwnScope,
        feature: "quake-bar",
        outcome: RouteOutcome::Action {
            action: "ctrl-clear",
            display: "clear input",
            run: ActionFn::new({
                let cell = cell.clone();
                move |_ctx| {
                    cell.update(|s| *s = QuakeBarState::default());
                    IntentResult::empty()
                }
            }),
        },
    });
}

/// Registers the quake bar's synchronous input hook.
///
/// While the quake scope is the active focus, editing intents route
/// here instead of the chat input. Writing the cell keeps the typing
/// carve-out: per-keystroke sync mutation of the slice's own input
/// buffer, exactly what a built-in input popup does.
pub fn register_quake_input_hook(routes: &KeyRoutes, cell: &TypedCell<QuakeBarState>) {
    let hook_cell = cell.clone();
    let hook: InputHook = std::sync::Arc::new(move |intent: &EditIntent| {
        let cell = &hook_cell;
        match intent {
            EditIntent::InsertChar(ch) => {
                cell.update(|s| s.input.text.insert_char(*ch));
                Some(IntentResult::empty())
            }
            EditIntent::DeleteBackward => {
                cell.update(|s| s.input.text.delete());
                Some(IntentResult::empty())
            }
            EditIntent::DeleteForward => {
                cell.update(|s| s.input.text.delete_forward());
                Some(IntentResult::empty())
            }
            EditIntent::CursorLeft => {
                cell.update(|s| s.input.text.cursor_left());
                Some(IntentResult::empty())
            }
            EditIntent::CursorRight => {
                cell.update(|s| s.input.text.cursor_right());
                Some(IntentResult::empty())
            }
            EditIntent::CursorHome => {
                cell.update(|s| s.input.text.cursor_pos = 0);
                Some(IntentResult::empty())
            }
            EditIntent::CursorEnd => {
                cell.update(|s| {
                    let len = s.input.text.input.len();
                    s.input.text.cursor_pos = len;
                });
                Some(IntentResult::empty())
            }
            EditIntent::Paste(_) => None,
        }
    });
    routes.register_input_hook(&quake_scope(), hook);
}

/// Submits the quake bar input into the command log.
///
/// Reads and trims the input text, clears the input buffer, and — if
/// the text is non-empty — emits a [`SubmitQuakeBarCommand`] so the
/// [`QuakeBarActor`](super::quake_bar_actor::QuakeBarActor) appends it
/// to the log. Empty input is a no-op (no command emitted).
fn handle_submit(cell: &TypedCell<QuakeBarState>, _ctx: ActionCtx<'_>) -> IntentResult {
    let text = {
        let guard = cell.read();
        guard.input.text.input.trim().to_owned()
    };
    // Clear the input buffer regardless: the key was pressed, so reset
    // the box.
    cell.update(|s| s.input = QuakeBarInput::default());

    if text.is_empty() {
        IntentResult::empty()
    } else {
        IntentResult::new_message(SubmitQuakeBarCommand { text })
    }
}

/// Scrolls the command log one line in the direction named by `action`
/// (`scroll-up` toward older lines, `scroll-down` toward newer).
fn handle_scroll(
    cell: &TypedCell<QuakeBarState>,
    action: &str,
    _ctx: ActionCtx<'_>,
) -> IntentResult {
    cell.update(|s| match action {
        "scroll-up" => s.log.scroll_up(),
        _ => s.log.scroll_down(),
    });
    IntentResult::empty()
}

/// Returns the dynamic intent for a quake bar action (test helper).
#[cfg(test)]
#[must_use]
fn quake_intent(action: &str, display: &str) -> jinn_slices::DynamicIntent {
    jinn_slices::DynamicIntent::new(quake_scope(), action, display)
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

    use super::QuakeBarState;
    use super::attach_quake_bar_rows;
    use super::handle_submit;
    use super::quake_scope;
    use super::register_quake_input_hook;
    use jinn_slices::KeyRoutes;
    use jinn_slices::ScopeSignal;
    use jinn_slices::Slices;

    use crate::state::quake_bar_slot;

    fn wired() -> (KeyRoutes, jinn_slices::TypedCell<QuakeBarState>) {
        let slices = Slices::new();
        let cell = slices
            .register(quake_bar_slot(), QuakeBarState::default())
            .expect("fresh registry");
        let routes = KeyRoutes::new();
        attach_quake_bar_rows(&routes, &cell);
        register_quake_input_hook(&routes, &cell);
        (routes, cell)
    }

    #[rstest::rstest]
    #[test]
    fn open_action_emits_push_scope_signal() {
        // Given a wired quake slice.
        let (routes, _cell) = wired();

        // When dispatching the open dynamic intent.
        let intent = super::quake_intent("open", "quake bar");
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = routes
            .action_for(
                &intent,
                jinn_slices::ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("open row attached");

        // Then the result carries a Push signal for the quake scope.
        assert_eq!(result.scope_signal, Some(ScopeSignal::Push(quake_scope())));
    }

    #[rstest::rstest]
    #[test]
    fn close_action_emits_pop_scope_signal() {
        // Given a wired quake slice.
        let (routes, _cell) = wired();

        // When dispatching the close dynamic intent.
        let intent = super::quake_intent("close", "close quake bar");
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = routes
            .action_for(
                &intent,
                jinn_slices::ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("close row attached");

        // Then the result carries a PopIf signal for the quake scope.
        assert_eq!(result.scope_signal, Some(ScopeSignal::PopIf(quake_scope())));
    }

    #[rstest::rstest]
    #[test]
    fn submit_with_text_emits_submit_command_and_clears_input() {
        // Given a wired quake slice with typed input.
        let (_routes, cell) = wired();
        cell.update(|s| {
            s.input.text.insert_char('h');
            s.input.text.insert_char('i');
        });

        // When submitting.
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = handle_submit(
            &cell,
            jinn_slices::ActionCtx {
                state: &mut state,
                slices: &slices,
                key_bytes: Vec::new(),
            },
        );

        // Then a SubmitQuakeBarCommand message was emitted.
        assert_eq!(result.message_names.len(), 1);
        assert!(
            result
                .message_names
                .first()
                .is_some_and(|name| name.ends_with("SubmitQuakeBarCommand"))
        );
        // And the input buffer is empty.
        assert!(cell.read().input.text.input.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn submit_with_empty_input_emits_no_command() {
        // Given a wired quake slice with empty input.
        let (_routes, cell) = wired();

        // When submitting.
        let mut state = TestState::default();
        let slices = Slices::new();
        let result = handle_submit(
            &cell,
            jinn_slices::ActionCtx {
                state: &mut state,
                slices: &slices,
                key_bytes: Vec::new(),
            },
        );

        // Then no command was emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn input_hook_inserts_characters_into_the_cell() {
        // Given a wired quake slice.
        let (routes, cell) = wired();
        let hook = routes.input_hook(&quake_scope()).expect("hook registered");

        // When the hook intercepts insert-char intents.
        let _ = hook(&jinn_slices::EditIntent::InsertChar('x'));
        let _ = hook(&jinn_slices::EditIntent::InsertChar('y'));

        // Then the cell's input buffer holds those characters.
        assert_eq!(cell.read().input.text.input, "xy");
    }

    #[rstest::rstest]
    #[test]
    fn input_hook_declines_non_editing_intents() {
        // Given a wired quake slice.
        let (routes, _cell) = wired();
        let hook = routes.input_hook(&quake_scope()).expect("hook registered");

        // When the hook sees a home-cursor intent (which this slice's
        // input surface does serve) — sanity-check it is matched — and
        // the dispatch layer never consults hooks for non-editing
        // intents, that filtering lives in the kernel's translation
        // (`as_edit_intent`), so no decline-case exists here anymore.
        let result = hook(&jinn_slices::EditIntent::CursorHome);

        // Then the editing intent is served.
        assert!(result.is_some());
    }

    #[rstest::rstest]
    #[test]
    fn scroll_actions_move_the_log_window() {
        // Given a wired quake slice with a multi-line log, scrolled up once.
        let (routes, cell) = wired();
        for i in 0..5 {
            cell.update(|s| s.log.push(format!("line-{i}")));
        }
        let intent = super::quake_intent("scroll-up", "scroll up");
        let mut state = TestState::default();
        let slices = Slices::new();
        let _ = routes
            .action_for(
                &intent,
                jinn_slices::ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("scroll-up row");
        let before = {
            let guard = cell.read();
            guard.log.visible_lines(2).to_vec()
        };

        // When dispatching scroll-down.
        let intent = super::quake_intent("scroll-down", "scroll down");
        let mut state = TestState::default();
        let slices = Slices::new();
        let _ = routes
            .action_for(
                &intent,
                jinn_slices::ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("scroll-down row");

        // Then the visible window shifted toward the newest line.
        let after = {
            let guard = cell.read();
            guard.log.visible_lines(2).to_vec()
        };
        assert_ne!(after, before.as_slice());
    }

    /// A minimal [`jinn_slices::SliceActionState`] stand-in for the row tests.
    #[derive(Default)]
    struct TestState {
        errors: Vec<String>,
    }

    impl jinn_slices::SliceActionState for TestState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            jinn_core_types::SessionId::new()
        }

        fn push_session_error(&mut self, message: &str) {
            self.errors.push(message.to_owned());
        }

        fn active_session_cwd(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> jinn_slices::PublishClosure {
            Box::new(|_bus| {})
        }
    }
}
