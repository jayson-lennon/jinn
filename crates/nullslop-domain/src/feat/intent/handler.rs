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

//! The [`IntentHandler`] — a single decision point for all user input.
//!
//! Processes every [`Intent`] variant: call the validator, then act.
//! On validation failure, the handler does nothing (no-op). On success,
//! it mutates [`AppState`] directly, optionally sets TUI signals, and
//! returns [`IntentResult`] carrying commands for the actor system.

#![allow(
    clippy::missing_docs_in_private_items,
    reason = "Phase 2 transitional — Phase 4 refactors handler into per-intent modules"
)]
#![allow(
    clippy::doc_markdown,
    reason = "auto-idents like IntentHandler, AppState, PickerKind are meaningful names"
)]

use crate::AppState;
use crate::feat::session::chat_session::SessionPhase;
use crate::protocol::{Command, PinPosition};

use crate::Intent;
use crate::feat;

use crate::IntentResult;

/// Processes user intents — the single decision point for all user input.
///
/// For each [`Intent`] variant: call the validator, then act.
/// On validation failure, the handler does nothing (no-op).
///
/// Some intents set "TUI signals" on `state.frontend.tui_signals` — flags that the
/// outer platform layer reads after `handle()` returns and acts upon
/// (e.g., opening an external editor, toggling a popup).
pub struct IntentHandler;

impl IntentHandler {
    /// Process an intent against the current application state.
    ///
    /// Clears TUI signals from the previous call, then processes the intent.
    /// Mutates `state` directly for UI operations.
    /// Returns commands and events for the actor system.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive match on all Intent variants"
    )]
    pub fn handle(intent: &Intent, state: &mut AppState) -> IntentResult {
        state.frontend.tui_signals.clear();

        // Cancel stream prompt intercept: if the prompt is showing,
        // ESC (NormalEscape) confirms the cancel;
        // any other intent dismisses the prompt and continues processing.
        if state.frontend.cancel_stream_prompt {
            state.frontend.cancel_stream_prompt = false;
            if matches!(intent, Intent::NormalEscape) {
                let session_id = state.session.active_session_id().clone();

                if state.active_session().phase() == SessionPhase::Compacting {
                    // Cancel compaction.
                    let drained = state.active_session_mut().cancel_compacting();
                    state
                        .active_session_mut()
                        .push_entry(crate::ChatEntry::system("Context compaction cancelled."));
                    let mut commands = vec![Command::CancelCompaction(
                        crate::feat::compaction_actor::protocol::command::CancelCompaction {
                            session_id: session_id.clone(),
                        },
                    )];
                    // If messages were queued, start a new turn.
                    if !drained.is_empty() {
                        let entries: Vec<crate::ChatEntry> = drained.into_iter().collect();
                        for entry in &entries {
                            state.active_session_mut().push_entry(entry.clone());
                        }
                        state.active_session_mut().begin_sending();
                        let history = state.active_session().history().to_vec();
                        let model_name = state.active_session().profile().model.clone();
                        commands.push(Command::AssemblePrompt(
                            crate::feat::context::protocol::command::AssemblePrompt {
                                session_id,
                                history,
                                tools: vec![],
                                model_name,
                            },
                        ));
                    }
                    return IntentResult::with_commands(commands);
                }
                // Existing stream cancel behavior (Streaming, Sending, Assembling, Idle).
                state.active_session_mut().cancel_stream_and_drain();
                return IntentResult::with_commands(vec![Command::CancelStream(
                    crate::feat::provider::protocol::command::CancelStream { session_id },
                )]);
            }
            // Any other key — dismiss prompt, fall through to normal processing.
        }

        match intent {
            // --- Token Budget Input (takes priority when TokenBudgetInput scope is active) ---
            Intent::InsertChar { ch }
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_insert_char(state, *ch)
            }
            // --- Sliding Window Input (takes priority when SlidingWindowInput scope is active) ---
            Intent::InsertChar { ch }
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_insert_char(state, *ch)
            }
            Intent::DeleteGrapheme
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_delete(state)
            }
            Intent::MoveCursorLeft
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_cursor_left(state)
            }
            Intent::MoveCursorRight
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_cursor_right(state)
            }
            Intent::DeleteGraphemeForward
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_delete_forward(state)
            }
            Intent::EnterNormalMode
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::TokenBudgetInput
                ) =>
            {
                feat::token_budget_input::intent::handle_token_budget_leave(state)
            }

            // --- Sliding Window Input (takes priority when SlidingWindowInput scope is active) ---
            Intent::DeleteGrapheme
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_delete(state)
            }
            Intent::MoveCursorLeft
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_cursor_left(state)
            }
            Intent::MoveCursorRight
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_cursor_right(state)
            }
            Intent::DeleteGraphemeForward
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_delete_forward(state)
            }
            Intent::EnterNormalMode
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::SlidingWindowInput
                ) =>
            {
                feat::sliding_window_input::intent::handle_sliding_window_leave(state)
            }

            // --- Arg Input (takes priority when ArgInput scope is active) ---
            Intent::InsertChar { ch }
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_insert_char(state, *ch)
            }
            Intent::DeleteGrapheme
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_delete(state)
            }
            Intent::MoveCursorLeft
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_cursor_left(state)
            }
            Intent::MoveCursorRight
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_cursor_right(state)
            }
            Intent::DeleteGraphemeForward
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                feat::session_lifecycle::intent::handle_arg_input_delete_forward(state)
            }
            Intent::EnterNormalMode
                if matches!(
                    state.frontend.scope_stack.current(),
                    crate::common::app_state::FocusScope::ArgInput
                ) =>
            {
                // ESC cancels arg input — pop scope, clear state.
                state.frontend.scope_stack.pop();
                state.frontend.arg_input = crate::common::app_state::ArgInputState::default();
                crate::protocol::IntentResult::empty()
            }

            // --- Chat Input ---
            Intent::InsertChar { ch } => feat::chat_input::intent::handle_insert_char(*ch, state),
            Intent::DeleteGrapheme => feat::chat_input::intent::handle_delete_grapheme(state),
            Intent::DeleteGraphemeForward => {
                feat::chat_input::intent::handle_delete_grapheme_forward(state)
            }
            Intent::SubmitMessage => feat::chat_input::intent::handle_submit_message(state),
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

            // --- Paste ---
            Intent::PasteText { text } => match state.frontend.scope_stack.current() {
                crate::common::app_state::FocusScope::Input => {
                    feat::chat_input::intent::handle_paste_text(text, state)
                }
                crate::common::app_state::FocusScope::Picker { .. } => {
                    feat::picker::intent::handle_picker_paste(state, text)
                }
                crate::common::app_state::FocusScope::ArgInput => {
                    feat::session_lifecycle::intent::handle_arg_input_paste(state, text)
                }
                crate::common::app_state::FocusScope::TokenBudgetInput => {
                    feat::token_budget_input::intent::handle_paste(state, text)
                }
                crate::common::app_state::FocusScope::SlidingWindowInput => {
                    feat::sliding_window_input::intent::handle_paste(state, text)
                }
                crate::common::app_state::FocusScope::RenameSessionInput => {
                    feat::rename_session_input::intent::handle_paste(state, text)
                }
                _ => IntentResult::empty(),
            },

            // --- Navigation ---
            Intent::ScrollUp => feat::navigation::intent::handle_scroll_up(state),
            Intent::ScrollDown => feat::navigation::intent::handle_scroll_down(state),
            Intent::MouseScrollUp => feat::navigation::intent::handle_mouse_scroll_up(state),
            Intent::MouseScrollDown => feat::navigation::intent::handle_mouse_scroll_down(state),
            Intent::ScrollToTop => feat::navigation::intent::handle_scroll_to_top(state),
            Intent::ScrollToBottom => feat::navigation::intent::handle_scroll_to_bottom(state),

            Intent::EditInput => feat::navigation::intent::handle_edit_input(state),

            // --- Mode & App ---
            Intent::Quit => feat::global::intent::handle_quit(state),
            Intent::Interrupt { session_id } => {
                feat::global::intent::handle_interrupt(state, session_id.as_ref())
            }
            Intent::EnterInsertMode => feat::chat_input::intent::handle_enter_insert_mode(state),
            Intent::EnterNormalMode => feat::chat_input::intent::handle_enter_normal_mode(state),
            Intent::ToggleWhichkey => feat::global::intent::handle_toggle_whichkey(state),
            Intent::NormalEscape => feat::chat_input::intent::handle_normal_escape(state),

            // --- Picker ---
            Intent::OpenPicker { kind } => feat::picker::intent::handle_open_picker(state, *kind),
            Intent::PickerInsertChar { ch } => feat::picker::intent::handle_insert_char(state, *ch),
            Intent::PickerBackspace => feat::picker::intent::handle_backspace(state),
            Intent::PickerConfirm => {
                let (result, maybe_intent) = feat::picker::intent::handle_picker_confirm(state);
                if let Some(intent) = maybe_intent {
                    let redispatch = IntentHandler::handle(&intent, state);
                    IntentResult::with_commands([result.commands, redispatch.commands].concat())
                } else {
                    result
                }
            }
            Intent::PickerMoveUp => feat::picker::intent::handle_move_up(state),
            Intent::PickerMoveDown => feat::picker::intent::handle_move_down(state),
            Intent::PickerMoveCursorLeft => feat::picker::intent::handle_move_cursor_left(state),
            Intent::PickerMoveCursorRight => feat::picker::intent::handle_move_cursor_right(state),
            Intent::ToggleKeymapScopeFilter => {
                feat::picker::intent::handle_toggle_keymap_scope_filter(state)
            }
            Intent::SessionNew => feat::session::intent::handle_session_new(state),
            Intent::RefreshModels => feat::session::intent::handle_refresh_models(state),
            Intent::RescanPromptTemplates => {
                feat::session::intent::handle_rescan_prompt_templates(state)
            }

            // --- Sidebar ---
            Intent::SidebarFocus => feat::ui::sidebar::intent::handle_sidebar_focus(state),
            Intent::SidebarLeave => feat::ui::sidebar::intent::handle_sidebar_leave(state),
            Intent::SidebarMoveDown => {
                feat::ui::sidebar::navigate_sidebar(
                    &feat::ui::sidebar::SidebarIntent::MoveDown,
                    state,
                );
                IntentResult::empty()
            }
            Intent::SidebarMoveUp => {
                feat::ui::sidebar::navigate_sidebar(
                    &feat::ui::sidebar::SidebarIntent::MoveUp,
                    state,
                );
                IntentResult::empty()
            }
            Intent::SidebarSectionNext => {
                feat::ui::sidebar::jump_to_section(
                    &feat::ui::sidebar::SidebarIntent::MoveDown,
                    state,
                );
                IntentResult::empty()
            }
            Intent::SidebarSectionPrev => {
                feat::ui::sidebar::jump_to_section(
                    &feat::ui::sidebar::SidebarIntent::MoveUp,
                    state,
                );
                IntentResult::empty()
            }
            Intent::PinsUnpin => feat::ui::sidebar::pins::pins_section::handle_pins_unpin(state),
            Intent::PinsPinTop => {
                feat::ui::sidebar::pins::pins_section::handle_pins_pin(state, PinPosition::Top)
            }
            Intent::PinsPinBottom => {
                feat::ui::sidebar::pins::pins_section::handle_pins_pin(state, PinPosition::Bottom)
            }
            Intent::PinsPinRelative => {
                feat::ui::sidebar::pins::pins_section::handle_pins_pin(state, PinPosition::Relative)
            }
            Intent::PinsPinCycle => {
                feat::ui::sidebar::pins::pins_section::handle_pins_pin_cycle(state)
            }
            Intent::SidebarPersonaEdit => {
                feat::ui::sidebar::pins::pins_section::handle_sidebar_persona_edit(state)
            }
            Intent::SidebarSessionNewWithLifecycle => {
                feat::ui::sidebar::sessions::handle_sidebar_session_new_with_lifecycle(state)
            }
            Intent::SidebarSessionClose => {
                feat::ui::sidebar::sessions::handle_session_close_with_lifecycle(state)
            }
            Intent::SidebarSessionTeardown => {
                feat::ui::sidebar::sessions::handle_session_teardown(state)
            }
            Intent::SidebarConfirm => {
                feat::ui::sidebar::sessions::handle_session_activate(state);
                IntentResult::empty()
            }

            // --- Chat Entry Selection ---
            Intent::ChatEntrySelectNext => {
                feat::chat_entry_selection::intent::handle_select_next(state)
            }
            Intent::ChatEntrySelectPrev => {
                feat::chat_entry_selection::intent::handle_select_prev(state)
            }
            Intent::ChatEntryPinSelected => {
                feat::chat_entry_selection::intent::handle_pin_selected(state)
            }
            Intent::ExpandToolEntry => {
                feat::chat_entry_selection::intent::handle_expand_tool_entry(state)
            }
            Intent::ToggleForkUserFilter => {
                feat::picker::intent::handle_toggle_fork_user_filter(state)
            }
            Intent::ToggleForkAssistantFilter => {
                feat::picker::intent::handle_toggle_fork_assistant_filter(state)
            }

            // --- Session Lifecycle ---
            Intent::SessionLifecycleSetup {
                lifecycle_name,
                args,
            } => feat::session_lifecycle::intent::handle_session_lifecycle_setup(
                state,
                lifecycle_name,
                args,
            ),
            Intent::SessionClose => feat::session_lifecycle::intent::handle_session_close(state),
            Intent::ArgInputConfirm => {
                feat::session_lifecycle::intent::handle_arg_input_confirm(state)
            }

            // --- Sidebar Resize ---
            Intent::SidebarResizeEnter => feat::sidebar_resize::intent::handle_resize_enter(state),
            Intent::SidebarResizeExpand => {
                feat::sidebar_resize::intent::handle_resize_expand(state)
            }
            Intent::SidebarResizeContract => {
                feat::sidebar_resize::intent::handle_resize_contract(state)
            }
            Intent::SidebarResizeLeave => feat::sidebar_resize::intent::handle_resize_leave(state),

            // --- Token Budget Input ---
            Intent::TokenBudgetInputEnter => {
                feat::token_budget_input::intent::handle_token_budget_enter(state)
            }
            Intent::TokenBudgetInputConfirm => {
                feat::token_budget_input::intent::handle_token_budget_confirm(state)
            }
            Intent::TokenBudgetInputLeave => {
                feat::token_budget_input::intent::handle_token_budget_leave(state)
            }

            // --- Sliding Window Input ---
            Intent::SlidingWindowInputEnter => {
                feat::sliding_window_input::intent::handle_sliding_window_enter(state)
            }
            Intent::SlidingWindowInputConfirm => {
                feat::sliding_window_input::intent::handle_sliding_window_confirm(state)
            }
            Intent::SlidingWindowInputLeave => {
                feat::sliding_window_input::intent::handle_sliding_window_leave(state)
            }

            // --- Rename Session Input ---
            Intent::SidebarRenameSession => {
                feat::rename_session_input::intent::handle_rename_session_enter(state)
            }
            Intent::RenameSessionConfirm => {
                feat::rename_session_input::intent::handle_rename_session_confirm(state)
            }
            Intent::RenameSessionLeave => {
                feat::rename_session_input::intent::handle_rename_session_leave(state)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use crate::common::app_state::AppState;
    use crate::feat::intent::IntentHandler;
    use crate::protocol::Intent;

    #[rstest::rstest]
    fn paste_text_ignored_in_normal_scope() {
        // Given an AppState in Normal scope (default).
        let mut state = AppState::default();

        // When handling PasteText.
        let result = IntentHandler::handle(
            &Intent::PasteText {
                text: "hello".into(),
            },
            &mut state,
        );

        // Then the buffer is empty and no commands are emitted.
        assert!(state.active_chat_input().is_empty());
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn paste_text_inserts_in_input_scope() {
        // Given an AppState in Input scope.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Input);

        // When handling PasteText.
        let result = IntentHandler::handle(
            &Intent::PasteText {
                text: "hello\nworld".into(),
            },
            &mut state,
        );

        // Then the buffer has the pasted text.
        assert_eq!(state.active_chat_input().text(), "hello\nworld");
        assert!(result.commands.is_empty());
    }
}
