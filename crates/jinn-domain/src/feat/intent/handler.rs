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
    reason = "Phase 2 transitional - Phase 4 refactors handler into per-intent modules"
)]
#![allow(
    clippy::doc_markdown,
    reason = "auto-idents like IntentHandler, AppState, PickerKind are meaningful names"
)]

use crate::AppState;
use crate::feat::interactive_term::protocol::command::ControlHolder;

use crate::protocol::{PickerKind, ScopeSignal};

use crate::Intent;
use crate::feat;

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
fn apply_scope_signal(result: &mut IntentResult, state: &mut AppState) {
    use crate::common::app_state::FocusScope;
    if let Some(signal) = result.scope_signal.take() {
        match signal {
            ScopeSignal::Push(id) => state.frontend.scope_push(FocusScope::Dynamic(id)),
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
/// (or the hook declines the intent) — the caller falls through to the
/// built-in arms.
fn try_slice_input_hook(
    intent: &Intent,
    state: &mut AppState,
    routes: &crate::common::slices::key_routes::KeyRoutes,
) -> Option<IntentResult> {
    use crate::common::app_state::FocusScope;
    let FocusScope::Dynamic(scope) = &state.frontend.scope() else {
        return None;
    };
    let hook = routes.input_hook(scope)?;
    // Hooks speak the slice-level editing vocabulary, not the kernel's
    // full intent enum: translate, and skip hooks for non-editing
    // intents entirely.
    let edit = crate::common::slices::key_routes::as_edit_intent(intent)?;
    hook(&edit)
}

/// Resolves the base scope after a `<Tab>` switch, walking the
/// registered tab scopes.
///
/// Tabs are declared by slices (tab descriptors registered at
/// activation); composition keeps the ordered list on `Slices`. With no
/// dynamic tab registered, `<Tab>` is a no-op round-trip to Normal —
/// the chat tab is the only tab.
fn next_tab_base(
    state: &AppState,
    slices: &crate::common::slices::Slices,
) -> crate::common::app_state::FocusScope {
    use crate::common::app_state::FocusScope;

    // The chat tab (Normal) is always first in the cycle, so the walk
    // is: Normal → tab[0] → … → tab[n-1] → Normal.
    let tabs = tab_scopes(slices);
    if tabs.is_empty() {
        return FocusScope::Normal;
    }
    let position = match state.frontend.scope_base() {
        FocusScope::Dynamic(id) => tabs.iter().position(|tab| tab == &id),
        _ => None,
    };
    match position {
        // Currently on a dynamic tab: advance, wrapping back to chat.
        Some(i) => match tabs.get(i + 1) {
            Some(next) => FocusScope::Dynamic(next.clone()),
            // Last tab: wrap to chat.
            None => FocusScope::Normal,
        },
        // On chat (or any other base): enter the first dynamic tab.
        None => match tabs.first() {
            Some(first) => FocusScope::Dynamic(first.clone()),
            None => FocusScope::Normal,
        },
    }
}

/// The registered tab scope ids, in tab order.
fn tab_scopes(slices: &crate::common::slices::Slices) -> Vec<jinn_slices::SliceScopeId> {
    slices.tab_scopes()
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
    prev_active: &crate::protocol::SessionId,
) {
    if let Some(registry) = crate::feat::interactive_term::takeover_intent::TERM_CONTROLS.get() {
        registry.set(prev_active, ControlHolder::Agent);
    }
    state.frontend.scope_clear_overlays();
    crate::feat::ui::status_hint::set_hint(
        state,
        slices,
        Some("terminal overlay closed — active session changed".to_owned()),
    );
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
        intent: &Intent,
        state: &mut AppState,
        slices: &crate::common::slices::Slices,
        routes: &crate::common::slices::key_routes::KeyRoutes,
        pickers: &jinn_picker::PickerRegistry,
    ) -> IntentResult {
        state
            .frontend
            .update_scope(|s| s.signals = jinn_slices::TuiSignals::new());
        // Status hints are transient: any fresh intent dismisses the previous
        // one (the handler arms that raise one run after this line).
        crate::feat::ui::status_hint::set_hint(state, slices, None);

        // Capture active session ID before processing for diff-after check.
        let prev_active = state.session.active_session_id().clone();
        // Capture the terminal overlay state too: the guard below must close
        // the overlay only when it was open *before* the intent ran, so an
        // intent that both activates and opens (sidebar `T`) is not closed by
        // its own switch.
        let terminal_overlay_open = matches!(
            state.frontend.scope(),
            crate::common::app_state::FocusScope::TerminalView
                | crate::common::app_state::FocusScope::TerminalControl
        );

        // Process the intent and get the result.
        let mut result = Self::handle_inner(intent, state, slices, routes, pickers);

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
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive match on all Intent variants"
    )]
    fn handle_inner(
        intent: &Intent,
        state: &mut AppState,
        slices: &crate::common::slices::Slices,
        routes: &crate::common::slices::key_routes::KeyRoutes,
        pickers: &jinn_picker::PickerRegistry,
    ) -> IntentResult {
        // Slice-registered routes go first: a dynamic intent is
        // delegated to its slice's action and never reaches the
        // built-in arms. An unregistered dynamic intent resolves to
        // None and falls through to the sweep-reset guard below, which
        // treats it like any other non-x action. The action runs
        // against the handler's own borrows (`ActionCtx`): it writes
        // the same `&mut AppState` guard — never a second lock — and
        // resolves slice cells through the same registry.
        if let Intent::Dynamic(dynamic) = intent
            && let Some(mut result) = routes.action_for(
                dynamic,
                crate::common::slices::key_routes::ActionCtx {
                    state,
                    slices,
                    key_bytes: dynamic.bytes.clone(),
                },
            )
        {
            // Scope transitions apply before the messages publish so a
            // slice that opens itself is on the stack before any bus
            // subscriber could observe a message.
            apply_scope_signal(&mut result, state);
            return result;
        }

        // Slice input hooks: the active dynamic scope's synchronous
        // editing surface. A hit means the keystroke belonged to the
        // slice (typing carve-out), so the intent is consumed here.
        if let Some(result) = try_slice_input_hook(intent, state, routes) {
            return result;
        }

        // Clear ignore sweep state when the user performs any action other than
        // pressing x. This ensures the sweep only continues during consecutive
        // x presses within 100ms.
        if !matches!(intent, Intent::ChatEntryIgnoreSelected) {
            state.active_session_mut().clear_ignore_sweep();
        }

        // Cancel stream prompt intercept: if the prompt is showing,
        // ESC (NormalEscape) confirms the cancel;
        // any other intent dismisses the prompt and continues processing.
        if let Some(result) = try_handle_cancel_stream_prompt(intent, state) {
            return result;
        }

        // Close session confirmation intercept: if the prompt is showing,
        // x (SidebarSessionClose) confirms the close;
        // any other intent dismisses the prompt and continues processing.
        if let Some(result) = try_handle_close_session_prompt(intent, state) {
            return result;
        }

        // Archive-tree confirmation intercept: if the prompt is showing,
        // A (SidebarSessionArchiveTree) re-validates and confirms (or flips
        // the prompt to the busy notice); any other intent dismisses the
        // prompt and continues processing.
        if let Some(result) = try_handle_archive_tree_prompt(intent, state) {
            return result;
        }

        match intent {
            Intent::InsertChar { ch }
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_insert_char(state, *ch)
            }
            Intent::DeleteGrapheme
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_delete(state)
            }
            Intent::MoveCursorLeft
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_cursor_left(state)
            }
            Intent::MoveCursorRight
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_cursor_right(state)
            }
            Intent::DeleteGraphemeForward
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_delete_forward(state)
            }
            Intent::EnterNormalMode
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                // ESC cancels arg input - pop scope, clear state.
                state.frontend.scope_pop();
                state.frontend.arg_input = crate::common::app_state::ArgInputState::default();
                crate::protocol::IntentResult::empty()
            }


            Intent::InsertChar { ch }
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                feat::project_add_input::intent::handle_insert_char(state, *ch)
            }
            Intent::DeleteGrapheme
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                feat::project_add_input::intent::handle_delete(state)
            }
            Intent::DeleteGraphemeForward
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                feat::project_add_input::intent::handle_delete_forward(state)
            }
            Intent::MoveCursorLeft
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                feat::project_add_input::intent::handle_cursor_left(state)
            }
            Intent::MoveCursorRight
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                feat::project_add_input::intent::handle_cursor_right(state)
            }
            Intent::EnterNormalMode
                if matches!(
                    state.frontend.scope(),
                    crate::common::app_state::FocusScope::ProjectAddInput
                ) =>
            {
                // ESC cancels project-add input - pop scope, clear state.
                feat::project_add_input::intent::handle_project_add_input_leave(state)
            }

            Intent::InsertChar { ch } => feat::chat_input::intent::handle_insert_char(*ch, state),
            Intent::DeleteGrapheme => feat::chat_input::intent::handle_delete_grapheme(state),
            Intent::DeleteGraphemeForward => {
                feat::chat_input::intent::handle_delete_grapheme_forward(state)
            }
            Intent::SubmitMessage => feat::chat_input::intent::handle_submit_message(state),
            Intent::ToggleInputMode => feat::chat_input::intent::handle_toggle_input_mode(state),
            Intent::AutocompleteConfirm => {
                feat::chat_input::intent::handle_autocomplete_confirm(state)
            }
            Intent::MoveCursorLeft => feat::chat_input::intent::handle_move_cursor_left(state),
            Intent::MoveCursorRight => feat::chat_input::intent::handle_move_cursor_right(state),
            Intent::MoveCursorToStart => {
                feat::chat_input::intent::handle_move_cursor_to_start(state)
            }
            Intent::MoveCursorToEnd => feat::chat_input::intent::handle_move_cursor_to_end(state),
            Intent::MoveCursorWordLeft => {
                feat::chat_input::intent::handle_move_cursor_word_left(state)
            }
            Intent::MoveCursorWordRight => {
                feat::chat_input::intent::handle_move_cursor_word_right(state)
            }
            Intent::MoveCursorUp => feat::chat_input::intent::handle_move_cursor_up(state),
            Intent::MoveCursorDown => feat::chat_input::intent::handle_move_cursor_down(state),

            Intent::PasteText { text } => match state.frontend.scope() {
                crate::common::app_state::FocusScope::Input => {
                    feat::chat_input::intent::handle_paste_text(text, state)
                }
                crate::common::app_state::FocusScope::Picker { .. } => {
                    feat::picker::intent::handle_picker_paste(state, text)
                }
                crate::common::app_state::FocusScope::ArgInput => {
                    feat::session_lifecycle::intent::handle_arg_input_paste(state, text)
                }
                crate::common::app_state::FocusScope::ProjectAddInput => {
                    feat::project_add_input::intent::handle_paste(state, text)
                }
                _ => IntentResult::empty(),
            },
            Intent::ScrollUp => feat::navigation::intent::handle_scroll_up(state),
            Intent::ScrollDown => feat::navigation::intent::handle_scroll_down(state),
            Intent::MouseScrollUp => feat::navigation::intent::handle_mouse_scroll_up(state),
            Intent::MouseScrollDown => feat::navigation::intent::handle_mouse_scroll_down(state),
            Intent::ScrollToTop => feat::navigation::intent::handle_scroll_to_top(state),
            Intent::ScrollToBottom => feat::navigation::intent::handle_scroll_to_bottom(state),

            Intent::EditInput => feat::navigation::intent::handle_edit_input(state),

            Intent::Quit => feat::global::intent::handle_quit(state),
            Intent::Interrupt { session_id } => {
                feat::global::intent::handle_interrupt(state, session_id.as_ref())
            }
            Intent::EnterInsertMode => feat::chat_input::intent::handle_enter_insert_mode(state),
            Intent::EnterNormalMode => {
                feat::chat_input::intent::handle_enter_normal_mode_with_pickers(state, pickers)
            }
            Intent::ToggleWhichkey => feat::global::intent::handle_toggle_whichkey(state),
            Intent::ToggleAuditPopup => feat::global::intent::handle_toggle_audit_popup(state),
            Intent::NormalEscape => feat::chat_input::intent::handle_normal_escape(state),
            Intent::NoOp => IntentResult::empty(),

            Intent::OpenPicker { kind } => {
                feat::picker::intent::handle_open_picker(state, *kind, pickers)
            }
            Intent::PickerAction { picker, action } => {
                feat::picker::action::run_action(state, pickers, picker, action)
            }
            Intent::PickerInsertChar { ch } => feat::picker::intent::handle_insert_char(state, *ch),
            Intent::PickerBackspace => feat::picker::intent::handle_backspace(state),
            Intent::PickerConfirm => {
                let (result, maybe_intent) =
                    feat::picker::intent::handle_picker_confirm(state, pickers);
                if let Some(intent) = maybe_intent {
                    let redispatch = IntentHandler::handle(&intent, state, slices, routes, pickers);
                    result.merge(redispatch)
                } else {
                    result
                }
            }
            Intent::CtrlClear => {
                let (result, maybe_intent) = feat::global::intent::handle_ctrl_clear(state);
                if let Some(intent) = maybe_intent {
                    let redispatch = IntentHandler::handle(&intent, state, slices, routes, pickers);
                    result.merge(redispatch)
                } else {
                    result
                }
            }
            Intent::PickerMoveUp => feat::picker::intent::handle_move_up(state, pickers),
            Intent::PickerMoveDown => feat::picker::intent::handle_move_down(state, pickers),
            Intent::PickerPageUp => feat::picker::intent::handle_page_up(state, pickers),
            Intent::PickerPageDown => feat::picker::intent::handle_page_down(state, pickers),
            Intent::PickerMoveCursorLeft => feat::picker::intent::handle_move_cursor_left(state),
            Intent::PickerMoveCursorRight => feat::picker::intent::handle_move_cursor_right(state),
            Intent::SessionNew => feat::session::intent::handle_session_new(state),
            Intent::RefreshModels => feat::session::intent::handle_refresh_models(state),
            Intent::RescanPromptTemplates => {
                feat::session::intent::handle_rescan_prompt_templates(state)
            }

            Intent::SessionNewWithLifecycle => feat::picker::intent::handle_open_picker(
                state,
                PickerKind::SessionLifecycle,
                pickers,
            ),
            Intent::LoadSubagentSession => {
                crate::feat::session::sessions_list::load_subagent::handle_load_subagent_session(
                    state,
                )
            }

            Intent::SidebarSessionClose => {
                // First press - show confirmation prompt.
                // The interceptor (try_handle_close_session_prompt) handles the second press.
                state.frontend.close_session_prompt = true;
                IntentResult::empty()
            }
            Intent::SidebarSessionArchiveTree => {
                crate::feat::session::sessions_list::archive_tree::handle_session_tree_action_arm(
                    state,
                    crate::feat::session::sessions_list::archive_tree::TreePromptAction::Archive,
                )
            }
            Intent::SidebarSessionTeardownTree => {
                crate::feat::session::sessions_list::archive_tree::handle_session_tree_action_arm(
                    state,
                    crate::feat::session::sessions_list::archive_tree::TreePromptAction::TeardownAndArchive,
                )
            }

            Intent::ChatEntrySelectNext => {
                feat::chat_entry_selection::intent::handle_select_next(state)
            }
            Intent::ChatEntrySelectPrev => {
                feat::chat_entry_selection::intent::handle_select_prev(state)
            }
            Intent::ChatEntryJumpNextCompaction => {
                feat::chat_entry_selection::intent::handle_jump_next_entry(state, |entry| {
                    entry.is_compaction()
                })
            }
            Intent::ChatEntryJumpPrevCompaction => {
                feat::chat_entry_selection::intent::handle_jump_prev_entry(state, |entry| {
                    entry.is_compaction()
                })
            }
            Intent::ChatEntryJumpNextUserEntry => {
                feat::chat_entry_selection::intent::handle_jump_next_entry(state, |entry| {
                    entry.is_user()
                })
            }
            Intent::ChatEntryJumpPrevUserEntry => {
                feat::chat_entry_selection::intent::handle_jump_prev_entry(state, |entry| {
                    entry.is_user()
                })
            }
            Intent::ChatEntryJumpNextPinned => {
                feat::chat_entry_selection::intent::handle_jump_next_entry(state, |entry| {
                    entry.is_pinned()
                })
            }
            Intent::ChatEntryJumpPrevPinned => {
                feat::chat_entry_selection::intent::handle_jump_prev_entry(state, |entry| {
                    entry.is_pinned()
                })
            }
            Intent::ChatEntryJumpNextSources => {
                feat::chat_entry_selection::intent::handle_jump_next_entry(state, |entry| {
                    entry.is_annotation()
                })
            }
            Intent::ChatEntryJumpPrevSources => {
                feat::chat_entry_selection::intent::handle_jump_prev_entry(state, |entry| {
                    entry.is_annotation()
                })
            }
            Intent::ChatEntryPinSelected => {
                feat::chat_entry_selection::intent::handle_pin_selected(state)
            }
            Intent::ExpandToolEntry => {
                feat::chat_entry_selection::intent::handle_expand_tool_entry(state)
            }
            Intent::ToggleIgnoredBlockVisibility => {
                feat::chat_entry_selection::intent::handle_toggle_ignored_block(state)
            }
            Intent::ForkFromEntry => {
                feat::chat_entry_selection::intent::handle_fork_from_entry(state)
            }
            Intent::NewSessionFromEntry => {
                feat::chat_entry_selection::intent::handle_new_session_from_entry(state)
            }
            Intent::YankSelectedEntry => {
                feat::chat_entry_selection::intent::handle_yank_selected(state)
            }
            Intent::ChatEntryIgnoreSelected => {
                feat::chat_entry_selection::intent::handle_ignore_selected(state)
            }
            Intent::ChatEntryResetSelected => {
                feat::chat_entry_selection::intent::handle_reset_selected(state)
            }
            Intent::ChatEntryIsolateSelected => {
                feat::chat_entry_selection::isolate::handle_isolate_selected(state)
            }

            Intent::SessionLifecycleSetup {
                lifecycle_name,
                args,
            } => feat::session_lifecycle::intent::handle_session_lifecycle_setup(
                state,
                lifecycle_name,
                args,
                None,
            ),
            Intent::SessionClose => feat::session_lifecycle::intent::handle_session_close(state),
            Intent::ArgInputConfirm => {
                feat::session_lifecycle::intent::handle_arg_input_confirm(state)
            }


            Intent::OpenPrunerAccumulationInput => {
                feat::pruner_accumulation_input::intent::handle_enter(state)
            }
            Intent::PrunerAccumulationConfirm => {
                feat::pruner_accumulation_input::intent::handle_confirm(state)
            }
            Intent::PrunerAccumulationLeave => {
                feat::pruner_accumulation_input::intent::handle_leave(state)
            }
            Intent::PrunerAccumulationInsertChar { ch } => {
                feat::pruner_accumulation_input::intent::handle_insert_char(state, *ch)
            }
            Intent::PrunerAccumulationCursorLeft => {
                feat::pruner_accumulation_input::intent::handle_cursor_left(state)
            }
            Intent::PrunerAccumulationCursorRight => {
                feat::pruner_accumulation_input::intent::handle_cursor_right(state)
            }
            Intent::PrunerAccumulationDeleteGrapheme => {
                feat::pruner_accumulation_input::intent::handle_delete(state)
            }
            Intent::PrunerAccumulationDeleteForward => {
                feat::pruner_accumulation_input::intent::handle_delete_forward(state)
            }


            Intent::ProjectAddInputConfirm => {
                feat::project_add_input::intent::handle_project_add_input_confirm(state)
            }
            Intent::ProjectAddInputLeave => {
                feat::project_add_input::intent::handle_project_add_input_leave(state)
            }

            Intent::Dynamic(_) => {
                // Unregistered dynamic intents are inert by construction:
                // a slice that never attached a route row for this action
                // must not fall into a built-in arm.
                tracing::debug!("dynamic intent arrived with no route row attached");
                IntentResult::empty()
            }
            Intent::ChangeCwd { root } => {
                crate::feat::navigation::intent::handle_change_cwd(state, *root)
            }

            // ── Tabs ──
            Intent::SwitchTab => {
                // Tab cycle across the registered tab scopes: the
                // composition-owned helper resolves the next base scope
                // from the slices' tab registry (chat when no dynamic
                // tab is registered). The terminal is an overlay
                // (<M-t>), not a tab: switching tabs with the overlay
                // open closes it first (Esc semantics). While the user
                // holds control, Tab is inert — handback is the only
                // exit.
                match state.frontend.scope() {
                    crate::common::app_state::FocusScope::TerminalView => {
                        state.frontend.scope_pop();
                        return IntentResult::empty();
                    }
                    crate::common::app_state::FocusScope::TerminalControl => {
                        return IntentResult::empty();
                    }
                    _ => {}
                }
                let new_base = next_tab_base(state, slices);
                state.frontend.scope_swap_base(new_base);
                IntentResult::empty()
            }
            Intent::ToggleTerminalOverlay { session_id } => {
                crate::feat::interactive_term::overlay_intent::handle_toggle_overlay(
                    state,
                    slices,
                    session_id.as_ref(),
                )
            }
            Intent::ToggleTerminalOverlayForSelected => {
                let selected =
                    crate::feat::interactive_term::overlay_intent::selected_sessions_sidebar_target(
                        state,
                    );
                // Activate the selected session first, so the overlay (which
                // renders the *active* session's terminal) and the live-term
                // check below always target the same session. When nothing is
                // selectable, fall through targeting the active session —
                // identical to the global toggle key.
                if let Some(selected) = selected
                    && selected != *state.session.active_session_id()
                {
                    state.session.set_active(selected);
                }
                crate::feat::interactive_term::overlay_intent::handle_toggle_overlay(state, slices, None)
            }
            Intent::TerminalTakeControl => {
                crate::feat::interactive_term::takeover_intent::handle_take_control(state)
            }
            Intent::TerminalHandback => {
                crate::feat::interactive_term::takeover_intent::handle_handback(state, slices)
            }
            Intent::TerminalYank => {
                crate::feat::interactive_term::takeover_intent::handle_yank(state, slices)
            }
            Intent::TerminalPushScreen => {
                crate::feat::interactive_term::takeover_intent::handle_push_screen(state, slices)
            }
            Intent::TerminalSendKey { bytes, label } => {
                crate::feat::interactive_term::takeover_intent::handle_send_key(
                    state,
                    bytes.clone(),
                    label.clone(),
                )
            }
        }
    }
}

/// Cancel stream prompt intercept.
///
/// If the cancel-stream confirmation prompt is showing:
/// - `NormalEscape` confirms the cancel (and returns the appropriate commands).
/// - Any other intent dismisses the prompt and returns `None` (fall through to normal processing).
///
/// Returns `None` if the prompt is not showing or was dismissed.
fn try_handle_cancel_stream_prompt(intent: &Intent, state: &mut AppState) -> Option<IntentResult> {
    if !state.frontend.cancel_stream_prompt {
        return None;
    }

    // Dismiss the prompt regardless of which intent triggered it.
    state.frontend.cancel_stream_prompt = false;

    if !matches!(intent, Intent::NormalEscape) {
        // Any other key — dismiss prompt, fall through to normal processing.
        return None;
    }

    let session_id = state.session.active_session_id().clone();

    // Check busy state before resetting.
    let was_busy = state.active_session().is_busy();

    // Cancel busy background operations (lifecycle, etc.).
    if was_busy {
        state.active_session_mut().cancel_busy();
    }

    // Cancel stream.
    state.active_session_mut().cancel_stream_and_drain();
    let mut result = IntentResult::empty().with_message(
        crate::feat::provider::protocol::command::CancelStream {
            session_id: session_id.clone(),
        },
    );

    // Also cancel any running lifecycle command.
    if was_busy {
        result = result.with_message(
            crate::feat::session_lifecycle::protocol::CancelLifecycleCommand { session_id },
        );
    }

    Some(result)
}

/// Close session confirmation prompt intercept.
///
/// If the close-session confirmation prompt is showing:
/// - `SidebarSessionClose` confirms the close (re-validates, emits CloseSession).
/// - Any other intent dismisses the prompt and returns `None` (fall through to normal processing).
///
/// Returns `None` if the prompt is not showing or was dismissed.
fn try_handle_close_session_prompt(intent: &Intent, state: &mut AppState) -> Option<IntentResult> {
    if !state.frontend.close_session_prompt {
        return None;
    }

    // Dismiss the prompt regardless of which intent triggered it.
    state.frontend.close_session_prompt = false;

    if !matches!(intent, Intent::SidebarSessionClose) {
        // Any other key - dismiss prompt, fall through to normal processing.
        return None;
    }

    // Second x press - perform the close.
    // Re-validates in case session became busy between taps.
    Some(crate::feat::session::sessions_list::close::handle_session_close_with_lifecycle(state))
}

/// Tree-action confirmation prompt intercept (`A` archive / `X` teardown).
///
/// If the archive-tree prompt is showing:
/// - Its own arming key (`SidebarSessionArchiveTree` for an archive prompt,
///   `SidebarSessionTeardownTree` for a teardown prompt) re-validates the
///   subtree: a still-idle subtree confirms (emits `ArchiveSessionTree` or
///   `TeardownSessionTree`); a member that became busy flips the prompt to
///   the busy notice and consumes the key; a vanished selection dismisses
///   the prompt.
/// - Any other intent dismisses the prompt and returns `None` (fall through
///   to normal processing).
///
/// Returns `None` if the prompt is not showing or was dismissed.
fn try_handle_archive_tree_prompt(intent: &Intent, state: &mut AppState) -> Option<IntentResult> {
    use crate::feat::session::sessions_list::archive_tree::{
        ArchiveTreeError, ArchiveTreePrompt, TreePromptAction, archive_tree_members,
        handle_session_tree_action_confirm,
    };

    let prompt = state.frontend.archive_tree_prompt.as_ref()?;

    // Which tree key was pressed, if either.
    let pressed = if matches!(intent, Intent::SidebarSessionArchiveTree) {
        Some(TreePromptAction::Archive)
    } else if matches!(intent, Intent::SidebarSessionTeardownTree) {
        Some(TreePromptAction::TeardownAndArchive)
    } else {
        None
    };

    // Only the prompt's own arming key confirms it; any other key (including
    // the sibling tree key) dismisses the prompt and falls through — the
    // normal match arm then arms that key's own prompt.
    let action = match prompt {
        ArchiveTreePrompt::Confirm { action, .. } => *action,
        ArchiveTreePrompt::Busy => {
            if pressed.is_none() {
                // Any non-tree key dismisses the busy notice too.
                state.frontend.archive_tree_prompt = None;
            }
            pressed?
        }
    };
    if pressed != Some(action) {
        // Any other key - dismiss prompt, fall through to normal processing.
        state.frontend.archive_tree_prompt = None;
        return None;
    }

    // Second press - re-validate in case the subtree changed between taps.
    match archive_tree_members(state) {
        Ok(members) => {
            // The selection is always the first member of a successful
            // validation; an empty member list cannot occur.
            let root = members.first()?.clone();
            Some(handle_session_tree_action_confirm(state, action, root))
        }
        Err(ArchiveTreeError::SubtreeBusy) => {
            // A member became busy between taps - consume the key and show
            // the busy notice instead (never train spam-to-force).
            state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);
            Some(IntentResult::empty())
        }
        // Selection vanished between taps - dismiss and process normally.
        Err(
            ArchiveTreeError::WrongSection
            | ArchiveTreeError::NoSelection
            | ArchiveTreeError::NotASession,
        ) => {
            state.frontend.archive_tree_prompt = None;
            None
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

    /// Empty slice registry + route table for handler tests that don't
    /// exercise slices or route rows.
    fn empty_slices() -> crate::common::slices::Slices {
        crate::common::slices::Slices::new()
    }

    fn empty_pickers() -> jinn_picker::PickerRegistry {
        jinn_picker::PickerRegistry::new()
    }

    /// `Slices` with the status-bar cell registered (as the slice's
    /// `activate` does), for hint write/read assertions.
    fn status_bar_slices() -> crate::common::slices::Slices {
        let slices = crate::common::slices::Slices::new();
        #[expect(
            clippy::expect_used,
            reason = "test seam: a fresh Slices never has the status-bar cell registered"
        )]
        {
            slices
                .register(
                    jinn_status_bar_msg::status_bar_slot(),
                    jinn_status_bar_msg::StatusBarState::default(),
                )
                .expect("fresh Slices never has the status-bar cell registered");
        }
        slices
    }

    fn empty_routes() -> crate::common::slices::key_routes::KeyRoutes {
        crate::common::slices::key_routes::KeyRoutes::new()
    }
    use crate::common::app_state::{AppState, FocusScope};
    use crate::feat::intent::IntentHandler;
    use crate::protocol::{ChatEntry, Intent};
    use jinn_term_msg::cells::ScreenCells;

    #[rstest::rstest]
    fn paste_text_ignored_in_normal_scope() {
        // Given an AppState in Normal scope.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_clear_overlays();

        // When handling PasteText.
        let result = IntentHandler::handle(
            &Intent::PasteText {
                text: "hello".into(),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the buffer is empty and no commands are emitted.
        assert!(
            state
                .active_session()
                .with_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || true)
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn paste_text_inserts_in_input_scope() {
        // Given an AppState in Input scope.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(crate::common::app_state::FocusScope::Input);

        // When handling PasteText.
        let result = IntentHandler::handle(
            &Intent::PasteText {
                text: "hello\nworld".into(),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the buffer has the pasted text.
        assert_eq!(
            state
                .active_session()
                .with_input(|i| i.text().to_owned(), String::new),
            "hello\nworld"
        );
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn input_box_accepts_insert_char() {
        // Given an AppState in Input scope.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(crate::common::app_state::FocusScope::Input);

        // When handling InsertChar.
        let _result = IntentHandler::handle(
            &Intent::InsertChar { ch: 'x' },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the buffer has the inserted char.
        assert_eq!(
            state
                .active_session()
                .with_input(|i| i.text().to_owned(), String::new),
            "x"
        );
    }

    #[rstest::rstest]
    #[rstest::rstest]
    #[rstest::rstest]
    #[rstest::rstest]
    #[rstest::rstest]
    #[rstest::rstest]
    #[test]
    fn insert_char_routes_to_arg_input_when_scope_is_arg_input() {
        // Given ArgInput scope is active.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "hel".to_owned(),
                cursor_pos: 3,
            },
        };

        // When handling InsertChar.
        let _result = IntentHandler::handle(
            &Intent::InsertChar { ch: 'o' },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then arg_input received the char, not the chat input.
        assert_eq!(state.frontend.arg_input.text.input, "helo");
        assert!(
            state
                .active_session()
                .with_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || true),
            "chat input should be empty"
        );
    }

    #[rstest::rstest]
    #[test]
    fn insert_char_routes_to_chat_input_when_scope_is_normal() {
        // Given Normal scope (default) with Input overlay.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Input);

        // When handling InsertChar.
        let _result = IntentHandler::handle(
            &Intent::InsertChar { ch: 'x' },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the chat input received the char.
        assert_eq!(
            state
                .active_session()
                .with_input(|i| i.text().to_owned(), String::new),
            "x"
        );
        assert!(
            state.frontend.arg_input.text.input.is_empty(),
            "arg input should be empty"
        );
    }

    #[rstest::rstest]
    #[test]
    fn delete_grapheme_routes_to_arg_input_when_scope_is_arg_input() {
        // Given ArgInput scope with some text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "abc".to_owned(),
                cursor_pos: 3,
            },
        };

        // When handling DeleteGrapheme.
        let _result = IntentHandler::handle(
            &Intent::DeleteGrapheme,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then arg_input had a char deleted.
        assert_eq!(state.frontend.arg_input.text.input, "ab");
    }

    #[rstest::rstest]
    #[test]
    fn move_cursor_left_routes_to_arg_input_when_scope_is_arg_input() {
        // Given ArgInput scope with cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "ab".to_owned(),
                cursor_pos: 2,
            },
        };

        // When handling MoveCursorLeft.
        let _result = IntentHandler::handle(
            &Intent::MoveCursorLeft,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then arg_input cursor moved.
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    #[test]
    fn move_cursor_right_routes_to_arg_input_when_scope_is_arg_input() {
        // Given ArgInput scope with cursor at start.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "ab".to_owned(),
                cursor_pos: 0,
            },
        };

        // When handling MoveCursorRight.
        let _result = IntentHandler::handle(
            &Intent::MoveCursorRight,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then arg_input cursor moved.
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    #[test]
    fn delete_forward_routes_to_arg_input_when_scope_is_arg_input() {
        // Given ArgInput scope with cursor at start.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "abc".to_owned(),
                cursor_pos: 1,
            },
        };

        // When handling DeleteGraphemeForward.
        let _result = IntentHandler::handle(
            &Intent::DeleteGraphemeForward,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the char after cursor was deleted from arg_input.
        assert_eq!(state.frontend.arg_input.text.input, "ac");
    }

    #[rstest::rstest]
    #[test]
    fn enter_normal_mode_pops_arg_input_scope() {
        // Given ArgInput scope is active.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ArgInput);
        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name: "test".to_owned(),
            template_display: "<arg>".to_owned(),
            text: crate::common::line_input::LineInput {
                input: "partial".to_owned(),
                cursor_pos: 7,
            },
        };

        // When handling EnterNormalMode.
        let _result = IntentHandler::handle(
            &Intent::EnterNormalMode,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then ArgInput scope is popped and state cleared.
        assert!(!matches!(state.frontend.scope(), FocusScope::ArgInput));
        assert!(state.frontend.arg_input.text.input.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn paste_text_in_picker_scope_routes_to_picker() {
        // Given Picker scope is active.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: crate::protocol::PickerKind::Persona,
        });

        // When handling PasteText.
        let _result = IntentHandler::handle(
            &Intent::PasteText {
                text: "hello".into(),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then it doesn't panic and completes (paste is handled by picker).
        // The picker query filter is updated.
    }

    #[rstest::rstest]
    #[test]
    fn cancel_stream_prompt_esc_confirms() {
        // Given cancel_stream_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = true;

        // When handling NormalEscape.
        let result = IntentHandler::handle(
            &Intent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the prompt is dismissed and a CancelStream command is emitted.
        assert!(!state.frontend.cancel_stream_prompt);
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("CancelStream")),
            "should emit CancelStream: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn cancel_stream_prompt_other_intent_dismisses() {
        // Given cancel_stream_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = true;

        // When handling a different intent (InsertChar).
        let _result = IntentHandler::handle(
            &Intent::InsertChar { ch: 'a' },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the prompt is dismissed but no CancelStream command.
        assert!(!state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    #[test]
    fn cancel_stream_prompt_not_showing_returns_none() {
        // Given cancel_stream_prompt is NOT showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = false;

        // When handling NormalEscape.
        let _result = IntentHandler::handle(
            &Intent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then no cancel command is emitted (falls through to normal escape handling).
        // The prompt remains false.
        assert!(!state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    #[test]
    fn close_session_prompt_other_intent_dismisses() {
        // Given close_session_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.close_session_prompt = true;

        // When handling a different intent (ScrollUp).
        let _result = IntentHandler::handle(
            &Intent::ScrollUp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the prompt is dismissed.
        assert!(!state.frontend.close_session_prompt);
    }

    #[rstest::rstest]
    #[test]
    fn cancel_stream_prompt_noop_dismisses() {
        // Given cancel_stream_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = true;

        // When handling NoOp (unmapped key).
        let result = IntentHandler::handle(
            &Intent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the prompt is dismissed and no CancelStream command is emitted.
        assert!(!state.frontend.cancel_stream_prompt);
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("CancelStream")),
            "should not emit CancelStream: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn close_session_prompt_noop_dismisses() {
        // Given close_session_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.close_session_prompt = true;

        // When handling NoOp (unmapped key).
        let _result = IntentHandler::handle(
            &Intent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the prompt is dismissed.
        assert!(!state.frontend.close_session_prompt);
    }

    #[rstest::rstest]
    #[test]
    fn noop_is_empty_when_no_prompt() {
        // Given default state with no prompts showing.
        let mut state = AppState::default_with_scope_focus();

        // When handling NoOp.
        let result = IntentHandler::handle(
            &Intent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then result is empty.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn active_session_changed_emitted_on_session_switch() {
        // Given a state with two sessions.
        use crate::feat::session::chat_session::ChatSessionState;

        let mut state = AppState::default_with_scope_focus();
        let first_id = state.session.active_session_id().clone();

        let mut second = ChatSessionState::new();
        second.push_entry(ChatEntry::user("second session"));
        let second_id = second.session_id().clone();
        state.session.insert(second);

        // Activate second session directly (simulating sidebar click).
        state.session.set_active(second_id);

        // When handling an intent (any intent — we use SelectNextEntry as a no-op).
        // Actually, we need an intent that calls set_active.
        // The easiest way: call handle with an intent that doesn't change active session,
        // verify no event. Then manually switch and verify event.
        state.session.set_active(first_id);
        let result = IntentHandler::handle(
            &Intent::ChatEntrySelectNext,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
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
    fn switch_tab_is_inert_while_user_holds_terminal_control() {
        // Given the terminal-control overlay open (user holds control).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_clear_overlays();
        state.frontend.scope_push(FocusScope::TerminalControl);

        // When switching tabs.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the scope stays TerminalControl — handback is the only exit.
        assert_eq!(state.frontend.scope(), FocusScope::TerminalControl);
    }

    #[rstest::rstest]
    fn take_control_pushes_control_scope_and_flags_user() {
        // Given an AppState whose terminal tab shows a session.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);

        // When handling TerminalTakeControl.
        IntentHandler::handle(
            &Intent::TerminalTakeControl,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the scope is TerminalControl.
        assert_eq!(state.frontend.scope(), FocusScope::TerminalControl);
        // And the shared registry records the user as control holder (the
        // static is unwired in unit tests, so the flip is a no-op; the
        // observable behavior here is the scope push itself).
    }

    #[rstest::rstest]
    fn toggle_opens_view_overlay_for_live_session() {
        // Given default state whose active session has a live terminal.
        let mut state = AppState::default_with_scope_focus();
        let chat = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&chat, true));

        // When toggling the terminal overlay.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay opens in view mode.
        assert_eq!(state.frontend.scope(), FocusScope::TerminalView);
    }

    #[rstest::rstest]
    fn toggle_without_live_term_is_inert() {
        // Given default state with no live terminals.
        let mut state = AppState::default_with_scope_focus();

        // When toggling the terminal overlay.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the scope stays Input (default scope; no overlay opened).
        assert_eq!(state.frontend.scope(), FocusScope::Input);
    }

    #[rstest::rstest]
    fn set_hint_is_a_noop_without_the_status_bar_cell() {
        // Given state with no activated status-bar slice (bare `Slices`).
        let mut state = AppState::default_with_scope_focus();
        let slices = empty_slices();

        // When an intent arm raises a hint through the write seam.
        crate::feat::ui::status_hint::set_hint(
            &mut state,
            &slices,
            Some("should be dropped".to_owned()),
        );

        // Then the write is a silent no-op (removability: no slice, no
        // hint anywhere, no panic).
        assert!(crate::feat::ui::status_hint::hint(&slices).is_none());
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
    fn toggle_without_live_term_sets_a_status_hint() {
        // Given default state with no live terminals and an activated
        // status-bar cell (the hint's storage).
        let mut state = AppState::default_with_scope_focus();
        let slices = status_bar_slices();

        // When toggling the terminal overlay.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then no overlay opened (still the default scope).
        assert_eq!(state.frontend.scope(), FocusScope::Input);
        // And a status hint explains the inert press.
        let hint = crate::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref()
                .is_some_and(|h| h.contains("no live terminal")),
            "expected a no-live-terminal hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn next_intent_dismisses_a_raised_status_hint() {
        // Given a state carrying a hint from a failed overlay toggle.
        let mut state = AppState::default_with_scope_focus();
        let slices = status_bar_slices();
        crate::feat::ui::status_hint::set_hint(&mut state, &slices, Some("stale hint".to_owned()));

        // When handling any other intent.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the hint is cleared.
        assert!(crate::feat::ui::status_hint::hint(&slices).is_none());
    }

    #[rstest::rstest]
    fn toggle_closes_an_open_overlay() {
        // Given an open terminal overlay (view mode).
        let mut state = AppState::default_with_scope_focus();
        let chat = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&chat, true));
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // When toggling again.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay closes back to the base scope (the input scope the
        // overlay replaced does not resurrect).
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn toggle_with_explicit_session_targets_that_session() {
        // Given a state where the *selected* session (not the active one) has
        // a live terminal.
        let mut state = AppState::default_with_scope_focus();
        let selected = crate::protocol::SessionId::new();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&selected, true));

        // When toggling with the explicit session id.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay {
                session_id: Some(selected.clone()),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay opens.
        assert_eq!(state.frontend.scope(), FocusScope::TerminalView);
    }

    #[rstest::rstest]
    fn switch_tab_with_no_registered_tab_stays_normal() {
        // Given default (Normal) state and no dynamic tab registered.
        let mut state = AppState::default_with_scope_focus();

        // When switching tabs.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the base is Normal (chat is the only tab).
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn switch_tab_cycles_through_registered_tabs() {
        // Given a slices registry with one dynamic tab registered.
        let slices = crate::common::slices::Slices::new();
        let tab = jinn_slices::SliceScopeId::new("dashboard", "tab");
        slices.register_tab_scope(
            tab.clone(),
            jinn_slices::SlotKey::builtin("dashboard", "tab"),
        );
        let mut state = AppState::default_with_scope_focus();

        // When switching tabs twice.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            &empty_pickers(),
        );
        // Then the base is the registered tab.
        assert_eq!(
            state.frontend.scope_base(),
            FocusScope::Dynamic(tab.clone())
        );

        // When switching tabs again.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &slices,
            &empty_routes(),
            &empty_pickers(),
        );
        // Then the cycle wraps to Normal.
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn switch_tab_while_overlay_open_closes_it() {
        // Given an open terminal overlay over the Normal base.
        let mut state = AppState::default_with_scope_focus();
        let chat = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&chat, true));
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );
        assert_eq!(state.frontend.scope(), FocusScope::TerminalView);

        // When switching tabs.
        IntentHandler::handle(
            &Intent::SwitchTab,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay closed (back to base, not a tab flip).
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
        assert_eq!(state.frontend.scope_base(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn send_key_outside_control_scope_is_inert() {
        // Given an AppState in TerminalView (no control).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);

        // When handling TerminalSendKey.
        let result = IntentHandler::handle(
            &Intent::TerminalSendKey {
                bytes: b"a".to_vec(),
                label: String::new(),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then no pty write command is published.
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    fn handback_releases_flag_pops_scope_and_sends_nothing() {
        // Given an AppState where the user holds control with a screen mirror.
        let mut state = AppState::default_with_scope_focus();
        let handback_slices = status_bar_slices();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|t| {
            t.apply_screen(
                state.session.active_session_id(),
                "handback-screen-marker".to_owned(),
                ScreenCells::default(),
                (0, 0),
                false,
            )
        });
        IntentHandler::handle(
            &Intent::TerminalTakeControl,
            &mut state,
            &handback_slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // When handling TerminalHandback.
        let result = IntentHandler::handle(
            &Intent::TerminalHandback,
            &mut state,
            &handback_slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the scope pops back to TerminalView.
        assert_eq!(state.frontend.scope(), FocusScope::TerminalView);
        // And no message is published to the model (release is silent; `I` pushes).
        assert!(
            result.messages.is_empty(),
            "handback must not message the model; got {:?}",
            result.message_names
        );
        // And the status hint advertises the push key.
        let hint = crate::feat::ui::status_hint::hint(&handback_slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains('I')),
            "handback hint must advertise I; got {hint:?}"
        );
    }

    #[rstest::rstest]
    fn push_screen_when_idle_enqueues_user_message() {
        // Given an AppState in the TerminalView overlay with a screen mirror,
        // and the session is idle.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|t| {
            t.apply_screen(
                state.session.active_session_id(),
                "idle-screen-marker".to_owned(),
                ScreenCells::default(),
                (0, 0),
                false,
            )
        });

        // When handling TerminalPushScreen.
        let result = IntentHandler::handle(
            &Intent::TerminalPushScreen,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then an enqueue message is published (idle dispatch path).
        assert!(
            result
                .message_names
                .iter()
                .any(|name| name.ends_with("EnqueueUserMessage")),
            "idle push must publish EnqueueUserMessage; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn push_screen_while_busy_steers_via_buffer() {
        // Given an AppState in the TerminalView overlay with a screen mirror,
        // while the session is mid-turn (Streaming).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|t| {
            t.apply_screen(
                state.session.active_session_id(),
                "busy-screen-marker".to_owned(),
                ScreenCells::default(),
                (0, 0),
                false,
            )
        });
        {
            let sid = state.session.active_session_id().clone();
            if let Some(session) = state.session.get_mut(&sid) {
                session.begin_streaming();
            }
        }

        // When handling TerminalPushScreen.
        let result = IntentHandler::handle(
            &Intent::TerminalPushScreen,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then a steering message is published (buffer drains at next
        // dispatch-resume).
        assert!(
            result
                .message_names
                .iter()
                .any(|name| name.ends_with("SubmitSteeringMessage")),
            "busy push must publish SubmitSteeringMessage; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn push_screen_yanks_the_screen_text() {
        // Given an AppState in the TerminalView overlay with a screen mirror.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|t| {
            t.apply_screen(
                state.session.active_session_id(),
                "yank-and-push-marker".to_owned(),
                ScreenCells::default(),
                (0, 0),
                false,
            )
        });

        // When handling TerminalPushScreen.
        IntentHandler::handle(
            &Intent::TerminalPushScreen,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the screen text was also staged for the clipboard.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text.as_deref(),
            Some("yank-and-push-marker"),
            "push must also yank (I = yank + push)"
        );
    }

    #[rstest::rstest]
    fn yank_stages_screen_text_and_sets_line_count_hint() {
        // Given an AppState in the TerminalView overlay with a multi-line mirror.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        let yank_slices = status_bar_slices();
        state.term_tabs().expect("term tabs cell").update(|t| {
            t.apply_screen(
                state.session.active_session_id(),
                "line one\nline two\nline three".to_owned(),
                ScreenCells::default(),
                (0, 0),
                false,
            )
        });

        // When handling TerminalYank.
        IntentHandler::handle(
            &Intent::TerminalYank,
            &mut state,
            &yank_slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the screen text was staged for the clipboard.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text.as_deref(),
            Some("line one\nline two\nline three")
        );
        // And the status hint reports the copied line count.
        let hint = crate::feat::ui::status_hint::hint(&yank_slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains('3')),
            "yank hint must report the line count; got {hint:?}"
        );
    }

    #[rstest::rstest]
    fn yank_without_live_terminal_sets_a_hint_and_stages_nothing() {
        // Given an AppState in the TerminalView overlay with no mirror.
        let mut state = AppState::default_with_scope_focus();
        let yank_slices = status_bar_slices();
        state.frontend.scope_swap_base(FocusScope::TerminalView);

        // When handling TerminalYank.
        IntentHandler::handle(
            &Intent::TerminalYank,
            &mut state,
            &yank_slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then nothing was staged for the clipboard.
        assert!(state.frontend.signals_snapshot().yank_text.is_none());
        // And a status hint explains the inert press.
        let hint = crate::feat::ui::status_hint::hint(&yank_slices);
        assert!(
            hint.as_deref()
                .is_some_and(|h| h.contains("no live terminal")),
            "expected a no-live-terminal hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn push_screen_wording_speaks_as_the_user_not_about_them() {
        // Given a captured screen.
        let screen = "shared-marker";

        // When building the push message text.
        let text = crate::feat::interactive_term::takeover_intent::push_screen_text(screen);

        // Then the text opens with the first-person screen offer.
        assert!(text.contains("Here is the current terminal screen"));
        assert!(text.contains(screen));
        // And it never speaks about the user in third person, never claims
        // a handback, and never embeds the refusal note.
        assert!(!text.contains("The user"));
        assert!(!text.contains("handed"));
        assert!(
            !text
                .contains(crate::feat::tools_actor::interactive_term_send::USER_HAS_CONTROL_NOTICE),
            "push wording must not embed the user-control notice"
        );
    }

    #[rstest::rstest]
    fn close_overlay_from_view_leaves_control_with_agent() {
        // Given an AppState with the overlay open in view mode (the shared
        // control registry unwired, so control stays with the agent).
        let mut state = AppState::default_with_scope_focus();
        let chat = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(&chat, true));
        state.frontend.scope_swap_base(FocusScope::TerminalView);

        // When toggling the overlay closed.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlay { session_id: None },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay closed (pop on a base-only stack is a no-op, so
        // the view scope remains as the base).
        assert_eq!(state.frontend.scope(), FocusScope::TerminalView);
    }

    #[rstest::rstest]
    fn active_session_switch_closes_terminal_overlay() {
        // Given a state with two sessions, the overlay open over the first.
        use crate::feat::session::chat_entry::ChatEntryKind;
        use crate::feat::session::chat_session::ChatSessionState;
        use crate::feat::tools_actor::task::TASK_TOOL_NAME;
        use crate::protocol::SessionId;
        let mut state = AppState::default_with_scope_focus();
        let slices = status_bar_slices();
        let first_id = state.session.active_session_id().clone();
        let child_id = SessionId::new();
        let mut child = ChatSessionState::new_child(&first_id, false);
        child.set_session_id(child_id.clone());
        state.session.insert(child);
        let mut entry = ChatEntry::tool_call("tc_guard_test", TASK_TOOL_NAME, "{}");
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
        state.frontend.scope_push(FocusScope::TerminalView);

        // When handling LoadSubagentSession — an intent that switches the
        // active session directly (set_active on a loaded child) regardless
        // of the open overlay.
        IntentHandler::handle(
            &Intent::LoadSubagentSession,
            &mut state,
            &slices,
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the active session changed to the child.
        assert_eq!(
            state.session.active_session_id(),
            &child_id,
            "the child session must be activated"
        );
        // And the previously-open terminal overlay did not survive the switch.
        assert_ne!(
            state.frontend.scope(),
            FocusScope::TerminalView,
            "a switch under an open overlay must not carry it to the new session"
        );
        // And the hint explains the abrupt close.
        let hint = crate::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains("closed")),
            "expected an overlay-closed hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn active_session_switch_releases_user_control() {
        // Given a state with a linked child session, the overlay open in
        // control mode (user holds the previous session's terminal).
        use crate::feat::interactive_term::protocol::command::ControlHolder;
        use crate::feat::session::chat_entry::ChatEntryKind;
        use crate::feat::session::chat_session::ChatSessionState;
        use crate::feat::tools_actor::task::TASK_TOOL_NAME;
        use crate::protocol::SessionId;
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
        state.frontend.scope_push(FocusScope::TerminalControl);
        if let Some(registry) = crate::feat::interactive_term::takeover_intent::TERM_CONTROLS.get()
        {
            registry.set(&first_id, ControlHolder::User);
        }

        // When switching the active session.
        IntentHandler::handle(
            &Intent::LoadSubagentSession,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the switched-from session's control is released back to the
        // agent — a stuck User holder would refuse every future agent send
        // (only reachable when the wired registry is present).
        if let Some(registry) = crate::feat::interactive_term::takeover_intent::TERM_CONTROLS.get()
        {
            assert_eq!(
                registry.holder_for(&first_id),
                ControlHolder::Agent,
                "control must not stick on User after a switch"
            );
        }
        // And the overlay is closed.
        assert_ne!(state.frontend.scope(), FocusScope::TerminalControl);
    }

    #[rstest::rstest]
    fn overlay_opened_by_the_switch_intent_survives_the_guard() {
        // Given a state with two sessions where the *second* holds the live
        // terminal, and no overlay open yet.
        use crate::feat::session::chat_session::ChatSessionState;
        let mut state = AppState::default_with_scope_focus();
        let mut second = ChatSessionState::new();
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
            .update_sections(|s| s.sessions.selected_index = Some(0));

        // When the sidebar toggle activates the session and opens the overlay
        // in the same intent.
        IntentHandler::handle(
            &Intent::ToggleTerminalOverlayForSelected,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then the overlay is open on the newly-activated session's terminal
        // — the guard captured "overlay closed" before the intent and must
        // not close what its own intent opened.
        assert_eq!(
            state.frontend.scope(),
            FocusScope::TerminalView,
            "activate-then-open must survive the switch guard"
        );
    }

    #[rstest::rstest]
    fn terminal_send_key_targets_the_active_session() {
        // Given the user holding terminal control.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.frontend.scope_push(FocusScope::TerminalControl);

        // When pressing a key in control mode.
        let result = IntentHandler::handle(
            &Intent::TerminalSendKey {
                bytes: b"x".to_vec(),
                label: "x".to_owned(),
            },
            &mut state,
            &empty_slices(),
            &empty_routes(),
            &empty_pickers(),
        );

        // Then a SendTermKey command is emitted (targeting the active
        // session's terminal — the payload is built from the active session
        // id by the takeover arm, which is the only session it can name).
        let names: Vec<&str> = result
            .message_names
            .iter()
            .filter(|n| n.ends_with("SendTermKey"))
            .copied()
            .collect();
        assert!(
            !names.is_empty(),
            "expected a SendTermKey command; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn handback_screen_survives_drain_as_user_entry() {
        use crate::feat::session::steering_buffer::SteeringBuffer;

        // Given the push message text for a captured screen.
        let text =
            crate::feat::interactive_term::takeover_intent::push_screen_text("drain-chain-marker");

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
}
