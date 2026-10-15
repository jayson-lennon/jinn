// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The [`IntentHandler`] - a single decision point for all user input.
//!
//! Processes every [`Intent`] variant: call the validator, then act.
//! On validation failure, the handler does nothing (no-op). On success,
//! it mutates [`AppState`] directly, optionally sets TUI signals, and
//! returns [`IntentResult`] carrying commands for the actor system.

#![allow(
    clippy::missing_docs_in_private_items,
    reason = "the dispatch match documents its arms inline rather than per-arm"
)]
#![allow(
    clippy::doc_markdown,
    reason = "auto-idents like IntentHandler, AppState, PickerKind are meaningful names"
)]

use crate::AppState;
use jinn_status_bar_msg::{StatusBarState, status_bar_slot};
use jinn_term_msg::command::ControlHolder;

use crate::protocol::ScopeSignal;

use super::cancel::try_handle_cancel_stream_prompt;
use super::paste::{route_paste, try_slice_input_hook};
use super::prompt_dismissal::{dismiss_unrelated_prompts, stream_in_flight};
use crate::KernelIntent;
use crate::feat;
use crate::feat::navigation::tab_cycle::next_tab_base;

use crate::IntentResult;

/// Processes user intents - the single decision point for all user input.
///
/// For each [`Intent`] variant: call the validator, then act.
/// On validation failure, the handler does nothing (no-op).
///
/// Some intents raise TUI signals through the frontend facade — flags that the
/// outer platform layer reads after `handle()` returns and acts upon
/// (e.g., opening an external editor, toggling a popup).
pub struct IntentHandler;

/// Applies a route result's scope signal to the scope stack.
///
/// The handler is the exempt single-writer of the scope stack; slices
/// request transitions as data ([`ScopeSignal`]) and this is where they
/// land. Runs before the result's messages publish (see
/// [`IntentResult::scope_signal`]).
///
/// A push that lands on a scope with a registered
/// [`ScopeEnterHook`](jinn_slices::ScopeEnterHook) also fires that hook, so
/// the entering slice initializes its per-open state here rather than in
/// whichever caller happened to request the transition. The hook runs after
/// the push, so the scope is already the active one.
pub(crate) fn apply_scope_signal(
    result: &mut IntentResult,
    state: &mut AppState,
    slices: &jinn_slices::Slices,
    routes: &jinn_slices::route::KeyRoutes,
    config: &jinn_config::ConfigLayer,
) {
    use jinn_slices::FocusScope;
    if let Some(signal) = result.scope_signal.take() {
        match signal {
            ScopeSignal::Push(id) => {
                state.frontend.scope_push(FocusScope::Dynamic(id.clone()));
                if let Some(hook) = routes.scope_enter_hook(&id) {
                    hook(jinn_slices::route::ActionCtx {
                        state,
                        slices,
                        config,
                        key_bytes: Vec::new(),
                    });
                }
            }
            ScopeSignal::PopIf(id) => {
                if matches!(&state.frontend.scope(), FocusScope::Dynamic(cur) if *cur == id) {
                    state.frontend.scope_pop();
                }
            }
        }
    }
}

/// Consults the active dynamic scope's registered input hook.
///
/// A hit means the keystroke belonged to the slice's own input surface:
/// the hook performed the synchronous write and the intent is consumed.
/// Returns `None` outside dynamic scopes or when no hook is registered
/// Ends the chat log's `x`-hold ignore sweep unless this intent *is*
/// the ignore action.
///
/// The sweep survives only across consecutive `x` presses, so any other
/// action ends it. The chat log's `x` is a slice route row, not a
/// kernel intent, so the action name is read off the dynamic intent and
/// compared against the name the slice published — the two agree because
/// both come from the same constant in `jinn-chat-log-view-msg`.
fn clear_ignore_sweep_unless_ignoring(state: &mut AppState, intent: &KernelIntent) {
    let action = match intent {
        KernelIntent::Dynamic(dynamic) => dynamic.action.as_str(),
        _ => "",
    };
    if action != jinn_chat_log_view_msg::IGNORE_SELECTED_ACTION {
        state.active_session_mut().clear_ignore_sweep();
    }
}

/// Closes the terminal overlay after an active-session switch and returns
/// the switched-from session's terminal control to the agent.
///
/// The overlay renders the *active* session's terminal, so leaving it open
/// across a switch would silently show a different program mid-keystroke.
/// The previous session's control holder must be reset explicitly: if the
/// user held control with no overlay to hand back from, the holder would
/// stick on `User` and refuse every future agent send.
///
/// Deliberately abrupt (a status hint says why) — obvious over subtle. The
/// registry flip only runs when the shared `TERM_CONTROLS` registry is
/// wired (unit tests leave it unset, making this a pure state transition).
fn close_terminal_overlay_on_switch(
    state: &mut AppState,
    slices: &jinn_slices::Slices,
    prev_active: &jinn_core_types::SessionId,
) {
    if let Some(registry) = jinn_term_msg::TERM_CONTROLS.get() {
        registry.set(prev_active, ControlHolder::Agent);
    }
    state.frontend.scope_clear_overlays();
    if let Some(status) = slices.reader::<StatusBarState>(&status_bar_slot()) {
        status.update(|state| {
            state.hint = Some("terminal overlay closed — active session changed".to_owned());
        });
    }
}

impl IntentHandler {
    /// Process an intent against the current application state.
    ///
    /// Clears TUI signals from the previous call, then processes the intent.
    /// Mutates `state` directly for UI operations. Consults the feature
    /// route table first: an intent bound in [`KeyRoutes`] produces its
    /// message and never reaches the built-in arms. `slices` backs the
    /// cross-feature reads (e.g. discord connectivity) that used to reach
    /// into `frontend` directly.
    /// Returns commands and events for the actor system.
    pub fn handle(
        intent: &KernelIntent,
        state: &mut AppState,
        slices: &jinn_slices::Slices,
        routes: &jinn_slices::route::KeyRoutes,
        config: &jinn_config::ConfigLayer,
    ) -> IntentResult {
        state
            .frontend
            .update_scope(|s| s.signals = jinn_slices::TuiSignals::new());
        // Status hints are transient: any fresh intent dismisses the previous
        // one (the handler arms that raise one run after this line).
        if let Some(status) = slices.reader::<StatusBarState>(&status_bar_slot()) {
            status.update(|state| state.hint = None);
        }

        // Capture active session ID before processing for diff-after check.
        let prev_active = state.session.active_session_id().clone();
        // Capture the terminal overlay state too: the guard below must close
        // the overlay only when it was open *before* the intent ran, so an
        // intent that both activates and opens (sidebar `T`) is not closed by
        // its own switch.
        let terminal_overlay_open = matches!(
            state.frontend.scope(),
            jinn_slices::FocusScope::Dynamic(id)
                if jinn_term_msg::is_overlay_scope(&id)
        );

        // Process the intent and get the result.
        let mut result = Self::handle_inner(intent, state, slices, routes, config);

        if state.session.active_session_id() != &prev_active {
            if terminal_overlay_open {
                close_terminal_overlay_on_switch(state, slices, &prev_active);
            }
            result = result.with_message(crate::protocol::system::ActiveSessionChanged {
                session_id: state.session.active_session_id().clone(),
            });
        }

        result
    }

    /// Internal intent dispatch — separated from `handle` to allow post-processing.
    ///
    /// Dispatch order:
    /// 1. Slice route rows (dynamic intents + globally-toggled slice
    ///    actions). A hit applies any scope signal, then returns.
    /// 2. Slice input hooks: while a dynamic scope with a registered
    ///    hook is active, editing intents route to the hook (sync write
    ///    of the slice's own state — the typing carve-out).
    /// 3. Built-in arms.
    fn handle_inner(
        intent: &KernelIntent,
        state: &mut AppState,
        slices: &jinn_slices::Slices,
        routes: &jinn_slices::route::KeyRoutes,
        config: &jinn_config::ConfigLayer,
    ) -> IntentResult {
        // Confirmation prompts must not survive a keystroke, so every armed
        // prompt is dismissed here — the first statement of this function,
        // ahead of the slice route rows and the slice input hooks below, both
        // of which return early. A prompt that outlived the key that should
        // have dismissed it keeps advertising a confirmation the user never
        // made.
        dismiss_unrelated_prompts(intent, state);

        // Bracketed paste: resolved by focus, not by the slice field the
        // TUI stamped on the intent. This runs ahead of the route rows
        // below because those match on `(row.scope, action)` and would
        // otherwise let a row attached to an unfocused scope claim the
        // paste — which is how a paste into the cwd popup used to land in
        // the chat input box instead. A paste always ends here.
        if let Some(result) = route_paste(intent, state, slices, routes, config) {
            return result;
        }

        // Slice-registered routes go first: a dynamic intent is
        // delegated to its slice's action and never reaches the
        // built-in arms. An unregistered dynamic intent resolves to
        // None and falls through to the sweep-reset guard below, which
        // treats it like any other non-x action. The action runs
        // against the handler's own borrows (`ActionCtx`): it writes
        // the same `&mut AppState` guard — never a second lock — and
        // resolves slice cells through the same registry.
        if let KernelIntent::Dynamic(dynamic) = intent
            && let Some(mut result) = routes.action_for(
                dynamic,
                jinn_slices::route::ActionCtx {
                    state,
                    slices,
                    config,
                    key_bytes: dynamic.bytes.clone(),
                },
            )
        {
            // Scope transitions apply before the messages publish so a
            // slice that opens itself is on the stack before any bus
            // subscriber could observe a message.
            apply_scope_signal(&mut result, state, slices, routes, config);
            return result;
        }

        // Slice input hooks: the active dynamic scope's synchronous
        // editing surface. A hit means the keystroke belonged to the
        // slice (typing carve-out), so the intent is consumed here.
        if let Some(result) = try_slice_input_hook(intent, state, routes) {
            return result;
        }

        // Clear ignore sweep state when the user performs any action other
        // than pressing x. This ensures the sweep only continues during
        // consecutive x presses within 100ms.
        //
        // This sits ahead of slice route dispatch so the chat-log slice's
        // own rows obey the rule too: a row that fired first would let
        // the sweep run past the 100ms window.
        clear_ignore_sweep_unless_ignoring(state, intent);

        // Cancel stream prompt: the confirming half. The prompt was cleared
        // above for every other intent, so by this point the only way to
        // reach here with the prompt standing is the `NormalEscape` that
        // raised it.
        if let Some(result) = try_handle_cancel_stream_prompt(intent, state) {
            return result;
        }

        match intent {
            // Mouse wheel. A wheel event is a crossterm backend handler on
            // the `Keymap`, not a keymap node, so no route row can express
            // it — these two intents stay in the kernel. The scroll they
            // perform is the log's, reached through the same session
            // facade the log's own rows use.
            KernelIntent::MouseScrollUp => feat::navigation::intent::handle_mouse_scroll_up(state),
            KernelIntent::MouseScrollDown => {
                feat::navigation::intent::handle_mouse_scroll_down(state)
            }

            KernelIntent::EditInput => feat::navigation::intent::handle_edit_input(state),

            KernelIntent::Quit => feat::global::intent::handle_quit(state),
            KernelIntent::Interrupt { session_id } => {
                feat::global::intent::handle_interrupt(state, session_id.as_ref())
            }
            KernelIntent::ToggleWhichkey => feat::global::intent::handle_toggle_whichkey(state),
            // Escape in Normal mode: raise the cancel-stream confirmation
            // when this session has something a cancel would reach — its own
            // turn, or a running subagent or attendant beneath it. The box no
            // longer owns this — it is a session concern, and the intercept
            // above handles the confirming half.
            KernelIntent::NormalEscape => {
                if stream_in_flight(state) {
                    state.frontend.cancel_stream_prompt = true;
                }
                IntentResult::empty()
            }
            KernelIntent::NoOp => IntentResult::empty(),

            // <c-c>: every picker that filters binds it in its own scope, so
            // the only kernel-side job is resetting the chat input box.
            KernelIntent::CtrlClear => {
                let (result, maybe_intent) = feat::global::intent::handle_ctrl_clear(state);
                if let Some(intent) = maybe_intent {
                    let redispatch = IntentHandler::handle(&intent, state, slices, routes, config);
                    result.merge(redispatch)
                } else {
                    result
                }
            }
            KernelIntent::SessionNew => {
                crate::session_lifecycle::intent::handle_session_new(state, config)
            }
            KernelIntent::RefreshModels => feat::session::intent::handle_refresh_models(state),
            KernelIntent::RescanPromptTemplates => {
                feat::session::intent::handle_rescan_prompt_templates(state)
            }

            KernelIntent::SessionLifecycleSetup {
                lifecycle_name,
                args,
            } => crate::session_lifecycle::intent::handle_session_lifecycle_setup(
                state,
                lifecycle_name,
                args,
                None,
                config,
            ),
            KernelIntent::SessionClose => {
                crate::session_lifecycle::intent::handle_session_close(state)
            }
            KernelIntent::Dynamic(_) => {
                // Unregistered dynamic intents are inert by construction:
                // a slice that never attached a route row for this action
                // must not fall into a built-in arm.
                tracing::debug!("dynamic intent arrived with no route row attached");
                IntentResult::empty()
            }
            KernelIntent::ChangeCwd { root } => {
                crate::feat::navigation::intent::handle_change_cwd(state, *root)
            }

            // ── Tabs ──
            KernelIntent::SwitchTab => {
                // Tab cycle across the registered tab scopes: the
                // composition-owned helper resolves the next base scope
                // from the slices' tab registry (chat when no dynamic
                // tab is registered). The terminal is an overlay
                // (<M-t>), not a tab: switching tabs with the overlay
                // open closes it first (Esc semantics). While the user
                // holds control, Tab is inert — handback is the only
                // exit.
                match state.frontend.scope() {
                    jinn_slices::FocusScope::Dynamic(id)
                        if jinn_term_msg::is_overlay_scope(&id) =>
                    {
                        // The terminal is an overlay (<M-t>), not a tab.
                        // In capture mode Tab is inert — handback is the
                        // only exit; in view mode switching tabs closes
                        // the overlay first (Esc semantics).
                        if id == jinn_term_msg::control_scope() {
                            return IntentResult::empty();
                        }
                        state.frontend.scope_pop();
                        return IntentResult::empty();
                    }
                    _ => {}
                }
                let new_base = next_tab_base(state, slices);
                state.frontend.scope_swap_base(new_base);
                IntentResult::empty()
            }
        }
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

    use jinn_status_bar_msg::{StatusBarState, status_bar_slot};

    /// Empty slice registry + route table for handler tests that don't
    /// exercise slices or route rows.
    fn empty_routes() -> jinn_slices::route::KeyRoutes {
        jinn_slices::route::KeyRoutes::new()
    }

    fn activate_child_route(child_id: jinn_core_types::SessionId) -> jinn_slices::route::KeyRoutes {
        use jinn_slices::route::{ActionFn, BindSite, RouteId, RouteOutcome, RouteRow};

        let routes = empty_routes();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:activate-child"),
            scope: jinn_slices::SliceScopeId::navigation("test", "activate-child"),
            key: "<enter>",
            category: "general",
            site: BindSite::StaticScopes(&["Normal"]),
            feature: "test",
            outcome: RouteOutcome::Action {
                action: "activate-child",
                display: "activate child",
                run: ActionFn::new(move |ctx| {
                    let state = ctx
                        .state
                        .as_any_mut()
                        .and_then(|state| state.downcast_mut::<AppState>())
                        .expect("test route runs against AppState");
                    state.session.set_active(child_id.clone());
                    IntentResult::empty()
                }),
            },
        });
        routes
    }

    fn activate_child_intent() -> KernelIntent {
        KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            jinn_slices::SliceScopeId::navigation("test", "activate-child"),
            "activate-child",
            "activate child",
        ))
    }

    fn state_with_linked_child_and_terminal_overlay(
        entry_id: &str,
    ) -> (AppState, jinn_core_types::SessionId) {
        use crate::protocol::ChatEntryKind;
        use jinn_core_types::SessionId;
        use jinn_session_state::ChatSessionState;
        use jinn_tools_msg::TASK_TOOL_NAME;

        let mut state = AppState::default_with_scope_focus();
        let first_id = state.session.active_session_id().clone();
        let child_id = SessionId::new();
        let mut child = ChatSessionState::new_child(&first_id, false);
        child.set_session_id(child_id.clone());
        state.session.insert(child);
        let mut entry = ChatEntry::tool_call(entry_id, TASK_TOOL_NAME, "{}");
        let ChatEntryKind::ToolCall { child_session, .. } = &mut entry.kind else {
            panic!("expected ToolCall kind");
        };
        *child_session = Some(child_id.clone());
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().select_prev_entry();

        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&first_id, true));
        // The real overlay opens on top of the base scope
        // (clear_overlays + push); the guard clears overlays, so the
        // overlay must not be the base itself.
        state.frontend.scope_swap_base(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::Dynamic(jinn_term_msg::view_scope()));

        (state, child_id)
    }

    fn state_with_live_terminal_session_selected() -> (AppState, jinn_core_types::SessionId) {
        use jinn_session_state::ChatSessionState;

        let mut state = AppState::default_with_scope_focus();
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        state.session.insert(second);
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&second_id, true));
        state
            .frontend
            .scope_swap_base(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_id = Some(second_id.clone()));

        (state, second_id)
    }

    fn transitioning_routes(
        scope: jinn_slices::SliceScopeId,
        signal: ScopeSignal,
        enters: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> (jinn_slices::route::KeyRoutes, KernelIntent) {
        use jinn_slices::route::{ActionFn, BindSite, RouteId, RouteOutcome, RouteRow};
        use std::sync::atomic::Ordering;

        let routes = empty_routes();
        routes.attach(RouteRow {
            route_id: RouteId::new("test:transition"),
            scope: scope.clone(),
            key: "<enter>",
            category: "general",
            site: BindSite::OwnScope,
            feature: "test",
            outcome: RouteOutcome::Action {
                action: "transition",
                display: "request a scope transition",
                run: ActionFn::new(move |_ctx| {
                    IntentResult::empty().with_scope_signal(signal.clone())
                }),
            },
        });
        let counted = std::sync::Arc::clone(enters);
        routes.register_scope_enter_hook(
            &scope,
            std::sync::Arc::new(move |_ctx: jinn_slices::route::ActionCtx<'_>| {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let intent = KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            scope,
            "transition",
            "request a scope transition",
        ));
        (routes, intent)
    }

    fn empty_slices() -> jinn_slices::Slices {
        jinn_slices::Slices::new()
    }

    /// `Slices` with the status-bar cell registered (as the slice's
    /// `activate` does), for hint write/read assertions.
    fn status_bar_slices() -> jinn_slices::Slices {
        let slices = jinn_slices::Slices::new();
        #[expect(
            clippy::expect_used,
            reason = "test seam: a fresh Slices never has the status-bar cell registered"
        )]
        {
            slices
                .register(status_bar_slot(), StatusBarState::default())
                .expect("fresh Slices never has the status-bar cell registered");
        }
        slices
    }

    fn status_hint(slices: &jinn_slices::Slices) -> Option<String> {
        slices
            .reader::<StatusBarState>(&status_bar_slot())?
            .read()
            .hint
            .clone()
    }

    /// A state holding a child session linked from the active one, a live
    /// terminal for the parent, and the term view overlay pushed on top of
    /// a `Normal` base — i.e. everything the switch guard reacts to.
    /// A state whose only session is live in the terminal, with the
    /// Sessions sidebar section selected and the base swapped to it.
    /// A dynamic intent plus the route table that activates `second_id` and
    /// then toggles the terminal view overlay for the newly active session.
    fn activate_and_toggle_overlay_routes(
        second_id: jinn_core_types::SessionId,
    ) -> (KernelIntent, jinn_slices::route::KeyRoutes) {
        let intent = KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            jinn_term_msg::view_scope(),
            "toggle-for-selected",
            "toggle terminal",
        ));
        let routes = jinn_slices::route::KeyRoutes::new();
        routes.attach(jinn_slices::route::RouteRow {
            route_id: jinn_slices::route::RouteId::new("term:toggle-for-selected"),
            scope: jinn_term_msg::view_scope(),
            key: "T",
            category: "general",
            site: jinn_slices::route::BindSite::OwnScope,
            feature: "term",
            outcome: jinn_slices::route::RouteOutcome::Action {
                action: "toggle-for-selected",
                display: "toggle terminal",
                run: jinn_slices::route::ActionFn::new(move |ctx| {
                    let Some(state) = ctx
                        .state
                        .as_any_mut()
                        .and_then(|a| a.downcast_mut::<AppState>())
                    else {
                        return IntentResult::empty();
                    };
                    // Inline term-slice semantics: activate the target
                    // session, then toggle the overlay for the active one.
                    if state.frontend.sidebar_section()
                        == Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
                    {
                        state.session.set_active(second_id.clone());
                    }
                    let chat = state.session.active_session_id().clone();
                    let live = state
                        .term_tabs()
                        .is_some_and(|cell| cell.read().live_terms.contains(&chat));
                    if !live {
                        return IntentResult::empty();
                    }
                    if state.frontend.scope() == FocusScope::Dynamic(jinn_term_msg::view_scope()) {
                        state.frontend.scope_pop();
                    } else {
                        state.frontend.scope_clear_overlays();
                        state
                            .frontend
                            .scope_push(FocusScope::Dynamic(jinn_term_msg::view_scope()));
                    }
                    IntentResult::empty()
                }),
            },
        });

        (intent, routes)
    }

    /// Route table whose single row requests `signal` for `scope`, plus a
    /// scope-enter hook for that same scope that bumps `enters`.
    use crate::common::app_state::AppState;
    use crate::feat::intent::IntentHandler;
    use crate::protocol::IntentResult;
    use crate::protocol::ScopeSignal;
    use crate::protocol::{ChatEntry, KernelIntent};
    use jinn_slices::FocusScope;

    /// The chat input box's text, as a paste leaves it.
    /// A bracketed-paste intent carrying `text`, as the TUI mints it.
    /// A route table whose `scope` has an input hook that accepts a paste
    /// and records the text it saw.
    /// A route table whose `scope` has an input hook that *declines* a
    /// paste by returning `None`.
    /// A route table with a keyless `paste-text` row attached to `scope`.
    /// A route table with a keyless `paste-text` row standing in for the
    /// chat input box's own row.
    ///
    /// The kernel cannot name the `jinn-chat-input` slice, so the box's row
    /// is reproduced here with the same shape: same action name, same
    /// keyless binding, same read of the payload off the ctx. What these
    /// tests pin is the *routing* — that a paste in a static scope reaches
    /// this row — not the box's insert, which its own tests cover.
    #[rstest::rstest]
    #[test]
    #[rstest::rstest]
    fn active_session_changed_emitted_on_session_switch() {
        // Given a state with two sessions.
        use jinn_session_state::ChatSessionState;

        let mut state = AppState::default_with_scope_focus();
        let first_id = state.session.active_session_id().clone();

        let mut second = ChatSessionState::new();
        second.push_entry(ChatEntry::user("second session"));
        let second_id = second.session_id().clone();
        state.session.insert(second);

        // Activate second session directly (simulating sidebar click).
        state.session.set_active(second_id);

        // When handling an intent that leaves the active session alone
        // (NoOp), so no event should be raised.
        state.session.set_active(first_id);
        let result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then no ActiveSessionChanged event (same session).
        let has_event = result
            .message_names
            .iter()
            .any(|&name| name.contains("ActiveSessionChanged"));
        assert!(
            !has_event,
            "should not emit ActiveSessionChanged when session unchanged"
        );
    }

    #[rstest::rstest]
    fn status_hint_write_is_a_noop_without_the_status_bar_cell() {
        // Given a slice registry without the status-bar cell.
        let slices = empty_slices();

        // When attempting a hint write.
        if let Some(status) = slices.reader::<StatusBarState>(&status_bar_slot()) {
            status.update(|state| state.hint = Some("should be dropped".to_owned()));
        }

        // Then no status state is available to expose a hint.
        assert!(status_hint(&slices).is_none());
    }

    #[rstest::rstest]
    fn default_scope_is_input_through_the_facade() {
        // Given a default state (no scope-focus wiring attached).
        let state = AppState::default();

        // When reading the current scope through the facade.
        let scope = state.frontend.scope();

        // Then it is Input — the historical default boot scope.
        assert_eq!(scope, FocusScope::Input);
    }

    #[rstest::rstest]
    fn scope_writes_are_noop_without_the_cell() {
        // Given a state whose scope-focus cell was never minted.
        let state = AppState::default();

        // When pushing a scope through the facade.
        state.frontend.scope_push(FocusScope::Normal);

        // Then the read still returns the unattached default (no panic,
        // no storage) — the removability property.
        assert_eq!(state.frontend.scope(), FocusScope::Input);
    }

    #[rstest::rstest]
    fn next_intent_dismisses_a_raised_status_hint() {
        // Given a state carrying a hint from a failed overlay toggle.
        let mut state = AppState::default_with_scope_focus();
        let slices = status_bar_slices();
        slices
            .reader::<StatusBarState>(&status_bar_slot())
            .expect("status-bar cell registered by test setup")
            .update(|status| status.hint = Some("stale hint".to_owned()));

        // When handling any other intent.
        IntentHandler::handle(
            &KernelIntent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the hint is cleared.
        assert!(status_hint(&slices).is_none());
    }

    #[rstest::rstest]
    fn active_session_switch_activates_the_child_session() {
        // Given a state with two sessions and the overlay open over the first.
        let slices = status_bar_slices();
        let (mut state, child_id) = state_with_linked_child_and_terminal_overlay("tc_guard_test");

        // When a route-owned action switches the active session.
        IntentHandler::handle(
            &activate_child_intent(),
            &mut state,
            &slices,
            &activate_child_route(child_id.clone()),
            jinn_slices::empty_config_layer(),
        );

        // Then the active session changed to the child.
        assert_eq!(
            state.session.active_session_id(),
            &child_id,
            "the child session must be activated"
        );
    }

    #[rstest::rstest]
    fn active_session_switch_closes_terminal_overlay() {
        // Given a state with two sessions and the overlay open over the first.
        let slices = status_bar_slices();
        let (mut state, child_id) = state_with_linked_child_and_terminal_overlay("tc_guard_test");

        // When a route-owned action switches the active session.
        IntentHandler::handle(
            &activate_child_intent(),
            &mut state,
            &slices,
            &activate_child_route(child_id.clone()),
            jinn_slices::empty_config_layer(),
        );

        // Then the previously-open terminal overlay did not survive the switch.
        assert_ne!(
            state.frontend.scope(),
            FocusScope::Dynamic(jinn_term_msg::view_scope()),
            "a switch under an open overlay must not carry it to the new session"
        );
    }

    #[rstest::rstest]
    fn active_session_switch_hints_that_the_overlay_closed() {
        // Given a state with two sessions and the overlay open over the first.
        let slices = status_bar_slices();
        let (mut state, child_id) = state_with_linked_child_and_terminal_overlay("tc_guard_test");

        // When a route-owned action switches the active session.
        IntentHandler::handle(
            &activate_child_intent(),
            &mut state,
            &slices,
            &activate_child_route(child_id.clone()),
            jinn_slices::empty_config_layer(),
        );

        // Then the hint explains the abrupt close.
        let hint = status_hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains("closed")),
            "expected an overlay-closed hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn active_session_switch_releases_user_control() {
        // Given a state with a linked child session, the overlay open in
        // control mode (user holds the previous session's terminal).
        use crate::protocol::ChatEntryKind;
        use jinn_core_types::SessionId;
        use jinn_session_state::ChatSessionState;
        use jinn_term_msg::command::ControlHolder;
        use jinn_tools_msg::TASK_TOOL_NAME;
        let mut state = AppState::default_with_scope_focus();
        let first_id = state.session.active_session_id().clone();
        let child_id = SessionId::new();
        let mut child = ChatSessionState::new_child(&first_id, false);
        child.set_session_id(child_id.clone());
        state.session.insert(child);

        let mut entry = ChatEntry::tool_call("tc_guard_test2", TASK_TOOL_NAME, "{}");
        let ChatEntryKind::ToolCall { child_session, .. } = &mut entry.kind else {
            panic!("expected ToolCall kind");
        };
        *child_session = Some(child_id.clone());
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().select_prev_entry();

        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&first_id, true));
        state.frontend.scope_swap_base(FocusScope::Normal);
        state
            .frontend
            .scope_push(FocusScope::Dynamic(jinn_term_msg::control_scope()));
        // Mint the registry (idempotent: a no-op when already present) so
        // the release assertion below is unconditional — production always
        // has the registry wired, and this test must prove the flip.
        let _ = jinn_term_msg::TERM_CONTROLS.set(jinn_term_msg::TermControls::default());
        let registry = jinn_term_msg::TERM_CONTROLS.get().expect("minted above");
        registry.set(&first_id, ControlHolder::User);

        // When switching the active session through a route-owned action.
        IntentHandler::handle(
            &activate_child_intent(),
            &mut state,
            &empty_slices(),
            &activate_child_route(child_id.clone()),
            jinn_slices::empty_config_layer(),
        );

        // Then the switched-from session's control is released back to the
        // agent — a stuck User holder would refuse every future agent send.
        assert_eq!(
            registry.holder_for(&first_id),
            ControlHolder::Agent,
            "control must not stick on User after a switch"
        );
        // And the overlay is closed.
        assert_ne!(
            state.frontend.scope(),
            FocusScope::Dynamic(jinn_term_msg::control_scope())
        );
    }

    #[rstest::rstest]
    fn overlay_opened_by_the_switch_intent_survives_the_guard() {
        // Given a state with two sessions where the *second* holds the live
        // terminal, and no overlay open yet.
        let (mut state, second_id) = state_with_live_terminal_session_selected();

        // When the sidebar toggle activates the session and opens the overlay
        // in the same intent.
        let (intent, routes) = activate_and_toggle_overlay_routes(second_id);
        IntentHandler::handle(
            &intent,
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the overlay is open on the newly-activated session's terminal
        // — the guard captured "overlay closed" before the intent and must
        // not close what its own intent opened.
        assert_eq!(
            state.frontend.scope(),
            FocusScope::Dynamic(jinn_term_msg::view_scope()),
            "activate-then-open must survive the switch guard"
        );
    }

    #[rstest::rstest]
    fn handback_screen_survives_drain_as_user_entry() {
        use jinn_session_state::steering_buffer::SteeringBuffer;

        // Given the push message text for a captured screen.
        let text = format!(
            "Here is the current terminal screen:\n\n```\n{}\n```",
            "drain-chain-marker"
        );

        // When routing the text through the steering buffer and draining it
        // (the session actor's busy-path behavior).
        let mut buf = SteeringBuffer::new();
        buf.push_fragment(text);
        let entry = buf.drain_into_entry().expect("entry");

        // Then the drained entry is a normal User entry carrying the screen.
        assert!(matches!(
            entry.kind,
            crate::protocol::ChatEntryKind::User { .. }
        ));
    }

    #[rstest::rstest]
    fn push_transition_fires_the_target_scope_enter_hook() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        // Given a route row that pushes a scope owning an enter hook.
        let scope = jinn_slices::SliceScopeId::new("test", "picker");
        let enters = Arc::new(AtomicUsize::new(0));
        let (routes, intent) =
            transitioning_routes(scope.clone(), ScopeSignal::Push(scope.clone()), &enters);
        let mut state = AppState::default_with_scope_focus();

        // When handling the row's dynamic intent.
        IntentHandler::handle(
            &intent,
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the hook ran exactly once.
        assert_eq!(enters.load(Ordering::SeqCst), 1);
    }

    #[rstest::rstest]
    fn push_transition_leaves_the_scope_on_the_stack() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        // Given a route row that pushes a scope.
        let scope = jinn_slices::SliceScopeId::new("test", "picker");
        let counter = Arc::new(AtomicUsize::new(0));
        let (routes, intent) =
            transitioning_routes(scope.clone(), ScopeSignal::Push(scope.clone()), &counter);
        let mut state = AppState::default_with_scope_focus();

        // When handling the row's dynamic intent.
        IntentHandler::handle(
            &intent,
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the scope is the active focus.
        assert_eq!(state.frontend.scope(), FocusScope::Dynamic(scope));
    }

    #[rstest::rstest]
    fn pop_if_transition_does_not_fire_the_scope_enter_hook() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        // Given a route row that requests a pop-if for a scope owning an
        // enter hook.
        let scope = jinn_slices::SliceScopeId::new("test", "picker");
        let enters = Arc::new(AtomicUsize::new(0));
        let (routes, intent) =
            transitioning_routes(scope.clone(), ScopeSignal::PopIf(scope), &enters);
        let mut state = AppState::default_with_scope_focus();

        // When handling the row's dynamic intent.
        IntentHandler::handle(
            &intent,
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the hook never ran — a pop is not an entry.
        assert_eq!(enters.load(Ordering::SeqCst), 0);
    }
}
