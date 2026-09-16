//! Chat input box intent handlers.
//!
//! Handles 17 chat-input intents:
//!
//! - **InsertChar** - inserts a character, manages autocomplete triggering/filtering/expansion.
//! - **PasteText** - bulk inserts pasted text, deactivates autocomplete.
//! - **DeleteGrapheme** - backspace with autocomplete awareness.
//! - **DeleteGraphemeForward** - forward delete with autocomplete awareness.
//! - **SubmitMessage** - validates, extracts text, resets buffer, returns `EnqueueUserMessage`.
//! - **AutocompleteConfirm** - confirms autocomplete selection or falls back to tab switch.
//! - **Cursor movement** (8 intents) - move cursor, optionally deactivating autocomplete.
//! - **EnterInsertMode** - switches to Input mode.
//! - **EnterNormalMode** - cancels streams, clears picker, switches to Normal mode.
//! - **NormalEscape** - clears chat entry selection.

use crate::common::app_state::AppState;
use crate::feat::chat_input::AutocompleteMatch;
use crate::feat::chat_input::AutocompleteTrigger;
use crate::feat::chat_input::ChatInputBoxState;
use crate::feat::chat_input::InputMode;
use crate::feat::chat_input::protocol::command::{EnqueueUserMessage, SubmitSteeringMessage};
use crate::feat::chat_input::slash_command::SlashCommand;
use crate::feat::chat_input::state::autocomplete::AutocompleteState;
use crate::feat::context::prompt_template::PromptTemplateStore;
use crate::feat::file_lister::ListDirectory;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;
use crate::protocol::{ChatEntry, IntentResult, SessionId};
use unicode_segmentation::UnicodeSegmentation as _;

use super::validator;

/// Handles `InsertChar` - inserts a character and manages autocomplete.
pub fn handle_insert_char(ch: char, state: &mut AppState) -> IntentResult {
    let is_autocomplete_active = state.with_active_input(|i| i.autocomplete().is_some(), || false);

    if is_autocomplete_active {
        return handle_insert_while_autocomplete_active(ch, state);
    }

    state.update_active_input(|i| i.insert_grapheme_at_cursor(ch));

    // `@` triggers the file popup (if at a valid boundary).
    if ch == '@' {
        let (valid, token_start) = state.with_active_input(
            |i| (is_valid_at_trigger_position(i), i.cursor_pos() - 1),
            || (false, 0),
        );
        if valid {
            state.update_active_input(|i| {
                i.activate_autocomplete(token_start, AutocompleteTrigger::At, Vec::new());
            });
            return IntentResult::new_message(emit_list_directory(state, ""));
        }
    }

    // `#` and `/` keep their existing static-list activation.
    match ch {
        '#' => {
            let (valid, token_start) = state.with_active_input(
                |i| (is_valid_hash_trigger_position(i), i.cursor_pos() - 1),
                || (false, 0),
            );
            if valid {
                let matches =
                    compute_matches(state.active_session().discovered_prompt_templates(), "");
                state.update_active_input(|i| {
                    i.activate_autocomplete(token_start, AutocompleteTrigger::Hash, matches);
                });
            }
        }
        '/' => {
            let (valid, token_start) = state.with_active_input(
                |i| (is_valid_slash_trigger_position(i), i.cursor_pos() - 1),
                || (false, 0),
            );
            if valid {
                let matches = compute_slash_matches("");
                state.update_active_input(|i| {
                    i.activate_autocomplete(token_start, AutocompleteTrigger::Slash, matches);
                });
            }
        }
        _ => {}
    }

    IntentResult::empty()
}

/// Handles a character typed while autocomplete is active.
///
/// For `#`/`/` triggers, this refines the static match list. For the `@` file
/// popup, a `/` descends into the deeper directory (emitting `ListDirectory`),
/// a space deactivates, and any other character just extends the path filter
/// (client-side entry filtering narrows the visible rows).
fn handle_insert_while_autocomplete_active(ch: char, state: &mut AppState) -> IntentResult {
    state.update_active_input(|i| i.insert_grapheme_at_cursor(ch));

    let trigger = state.with_active_input(
        |i| i.autocomplete().as_ref().map(AutocompleteState::trigger),
        || None,
    );

    // `@` file popup.
    if trigger == Some(AutocompleteTrigger::At) {
        // `@@` seam: typing `@` while an `At` popup is active forms `@@`,
        // which is the reserved trigger for the future multi-file picker.
        // Deactivate the `@` popup so `@@` stays literal with no handler.
        if ch == '@' {
            state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
            return IntentResult::empty();
        }
        if ch == ' ' {
            state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
            return IntentResult::empty();
        }
        if ch == '/' {
            let filter = state
                .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
                .unwrap_or_default();
            return IntentResult::new_message(emit_list_directory(state, &filter));
        }
        return IntentResult::empty();
    }

    // `#`/`/` static-list triggers.
    match (ch, trigger) {
        (' ', _) => {
            state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
        }
        ('#', Some(AutocompleteTrigger::Hash)) => {
            let Some(token_start) =
                state.with_active_input(ChatInputBoxState::autocomplete_token_start, || None)
            else {
                return IntentResult::empty();
            };
            let cursor_before_insert =
                state.with_active_input(ChatInputBoxState::cursor_pos, Default::default) - 1;
            let filter: String = state
                .with_active_input(|i| i.text().to_owned(), String::new)
                .graphemes(true)
                .enumerate()
                .skip_while(|(i, _)| *i < token_start + 1)
                .take_while(|(i, _)| *i < cursor_before_insert)
                .map(|(_, g)| g)
                .collect();
            if let Some(template) = state
                .active_session()
                .discovered_prompt_templates()
                .find_by_name(&filter)
            {
                let body = template.body.clone();
                state.update_active_input(|i| i.expand_autocomplete(&body));
            } else {
                state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
            }
        }
        _ => {
            let filter = state
                .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
                .unwrap_or_default();
            let matches = compute_updated_matches(
                state.active_session().discovered_prompt_templates(),
                trigger,
                &filter,
            );
            state.update_active_input(|i| i.update_autocomplete_matches(matches));
        }
    }
    IntentResult::empty()
}

/// Handles `PasteText` - bulk inserts pasted text and deactivates autocomplete.
///
/// Pastes bypass the per-character insertion pipeline entirely, inserting the
/// full string in one O(n) operation. Autocomplete is always deactivated on
/// paste since the pasted content may span multiple lines or tokens.
pub fn handle_paste_text(text: &str, state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    state.update_active_input(|i| i.insert_text(text));
    IntentResult::empty()
}

/// Handles `DeleteGrapheme` - backspace with autocomplete awareness.
pub fn handle_delete_grapheme(state: &mut AppState) -> IntentResult {
    let should_deactivate = if let Some(token_start) =
        state.with_active_input(ChatInputBoxState::autocomplete_token_start, || None)
    {
        state.with_active_input(ChatInputBoxState::cursor_pos, Default::default) <= token_start + 1
    } else {
        false
    };

    if should_deactivate {
        state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
        state.update_active_input(ChatInputBoxState::delete_grapheme_before_cursor);
    } else if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        state.update_active_input(ChatInputBoxState::delete_grapheme_before_cursor);

        let trigger = state.with_active_input(
            |i| i.autocomplete().as_ref().map(AutocompleteState::trigger),
            || None,
        );

        // `@` popup: deleting may shorten the path enough to change the listed
        // directory (e.g. deleting a `/`). Re-list.
        if trigger == Some(AutocompleteTrigger::At) {
            let filter = state
                .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
                .unwrap_or_default();
            return IntentResult::new_message(emit_list_directory(state, &filter));
        }

        let filter = state
            .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
            .unwrap_or_default();
        let matches = compute_updated_matches(
            state.active_session().discovered_prompt_templates(),
            trigger,
            &filter,
        );
        state.update_active_input(|i| i.update_autocomplete_matches(matches));
    } else {
        state.update_active_input(ChatInputBoxState::delete_grapheme_before_cursor);
    }

    try_reactivate_autocomplete(state);
    if let Some(cmd) = try_reactivate_at_autocomplete(state) {
        return IntentResult::new_message(cmd);
    }
    IntentResult::empty()
}

/// Handles `DeleteGraphemeForward` - forward delete with autocomplete awareness.
pub fn handle_delete_grapheme_forward(state: &mut AppState) -> IntentResult {
    let token_start = state.with_active_input(ChatInputBoxState::autocomplete_token_start, || None);
    let cursor = state.with_active_input(ChatInputBoxState::cursor_pos, Default::default);

    if let Some(token_start) = token_start {
        if cursor == token_start {
            state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
            state.update_active_input(ChatInputBoxState::delete_grapheme_after_cursor);
        } else {
            state.update_active_input(ChatInputBoxState::delete_grapheme_after_cursor);
            let should_deactivate = should_deactivate_on_cursor_move(state);
            if should_deactivate {
                state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
            } else {
                let filter = state
                    .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
                    .unwrap_or_default();
                let trigger = state.with_active_input(
                    |i| i.autocomplete().as_ref().map(AutocompleteState::trigger),
                    || None,
                );
                let matches = compute_updated_matches(
                    state.active_session().discovered_prompt_templates(),
                    trigger,
                    &filter,
                );
                state.update_active_input(|i| i.update_autocomplete_matches(matches));
            }
        }
    } else {
        state.update_active_input(ChatInputBoxState::delete_grapheme_after_cursor);
    }

    try_reactivate_autocomplete(state);
    IntentResult::empty()
}

/// Handles `ToggleInputMode` - flips Queue ↔ Steer.
pub fn handle_toggle_input_mode(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::toggle_input_mode);
    IntentResult::empty()
}

/// Handles `SubmitMessage` - confirms autocomplete if active, executes slash commands,
/// or submits the message as chat input.
pub fn handle_submit_message(state: &mut AppState) -> IntentResult {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        return handle_submit_message_with_autocomplete(state);
    }

    if validator::validate_submit_message(state).is_err() {
        return IntentResult::empty();
    }

    let session_id = state.session.active_session_id().clone();
    let input_text = state.with_active_input(|i| i.text().to_owned(), String::new);

    // Check for slash command execution.
    if let Some(command_name) = input_text.strip_prefix('/') {
        // Extract the first word after / (command name, ignoring arguments).
        let cmd = command_name.split_whitespace().next().unwrap_or("");
        if let Some(cmd) = SlashCommand::lookup(cmd) {
            state.update_active_input(ChatInputBoxState::reset);
            return with_mark_interacted(
                session_id,
                execute_slash_command(cmd, &input_text, state),
            );
        }
        // Unknown /command - fall through to normal message.
    }

    state.update_active_input(ChatInputBoxState::reset);

    let result = route_to_enqueue_or_steer(state, &session_id, input_text);
    with_mark_interacted(session_id, result)
}

/// Handles Enter when autocomplete is active - completes the selection and submits.
///
/// For `Hash` trigger: completes the name into the buffer, then submits as a
/// normal chat message (the completed name is just text in the message).
/// For `Slash` trigger: completes the command name, then re-checks for slash
/// command execution.
fn handle_submit_message_with_autocomplete(state: &mut AppState) -> IntentResult {
    let trigger = state.with_active_input(
        |i| i.autocomplete().as_ref().map(AutocompleteState::trigger),
        || None,
    );

    // Complete the selection.
    if let Some(name) = state.with_active_input(
        |i| i.autocomplete_selected().map(|m| m.name.clone()),
        || None,
    ) {
        state.update_active_input(|i| i.complete_autocomplete(&name));
    }
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);

    // Now submit based on what we completed.
    if validator::validate_submit_message(state).is_err() {
        return IntentResult::empty();
    }

    let session_id = state.session.active_session_id().clone();
    let display = state.with_active_input(|i| i.text().to_owned(), String::new);

    match trigger {
        Some(AutocompleteTrigger::Slash) => {
            // Check for slash command execution after completion.
            if let Some(command_name) = display.strip_prefix('/') {
                let cmd = command_name.split_whitespace().next().unwrap_or("");
                if let Some(cmd) = SlashCommand::lookup(cmd) {
                    state.update_active_input(ChatInputBoxState::reset);
                    return with_mark_interacted(
                        session_id,
                        execute_slash_command(cmd, &display, state),
                    );
                }
            }
            // Fall through to normal submit.
        }
        _ => {}
    }

    state.update_active_input(ChatInputBoxState::reset);

    let result = route_to_enqueue_or_steer(state, &session_id, display);
    with_mark_interacted(session_id, result)
}

/// Routes a submitted message based on input mode × session phase.
///
/// - Mode `Queue` (any phase) → `EnqueueUserMessage`
/// - Mode `Steer` + phase != `Idle` → `SubmitSteeringMessage`
/// - Mode `Steer` + phase == `Idle` → `EnqueueUserMessage` (fall-through)
///
/// Prompt-token (`#name`) expansion happens later, in
/// [`crate::feat::session::chat_session::ChatSessionState::push_entry`], so
/// both the enqueued message and the steering fragment flow through the single
/// expansion site. When steering, the buffer accumulates the raw display text.
fn route_to_enqueue_or_steer(
    state: &AppState,
    session_id: &SessionId,
    display: String,
) -> IntentResult {
    let mode = state.with_active_input(ChatInputBoxState::input_mode, Default::default);
    let phase = state.active_session().phase();
    match (mode, phase) {
        (InputMode::Steer, PhaseKind::Idle) | (InputMode::Queue, _) => {
            tracing::debug!(
                session_id = %session_id,
                mode = ?mode,
                phase = ?phase,
                "submit routed to enqueue"
            );
            IntentResult::empty().with_message(EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(display),
            })
        }
        (InputMode::Steer, _) => {
            tracing::debug!(
                session_id = %session_id,
                mode = ?mode,
                phase = ?phase,
                "submit routed to steering buffer"
            );
            IntentResult::empty().with_message(SubmitSteeringMessage {
                session_id: session_id.clone(),
                text: display,
            })
        }
    }
}

/// Prepends a `MarkSessionInteracted` message to the result.
fn with_mark_interacted(session_id: SessionId, mut result: IntentResult) -> IntentResult {
    result.messages.insert(
        0,
        crate::common::bridge::Bridge::publish_closure(MarkSessionInteracted { session_id }),
    );
    result
        .message_names
        .insert(0, std::any::type_name::<MarkSessionInteracted>());
    result
}

/// Executes a slash command.
fn execute_slash_command(
    command: SlashCommand,
    _display: &str,
    state: &mut AppState,
) -> IntentResult {
    match command {
        SlashCommand::Compact | SlashCommand::CompactAll => {
            let compact_all = matches!(command, SlashCommand::CompactAll);
            let session_id = state.session.active_session_id().clone();
            IntentResult::new_message(
                crate::feat::session::protocol::trigger_compaction::TriggerCompaction {
                    session_id,
                    compact_all,
                },
            )
        }
        SlashCommand::New => crate::feat::session::intent::handle_session_new(state),
    }
}

/// Handles `AutocompleteConfirm` - confirms selection or falls back to tab switch.
pub fn handle_autocomplete_confirm(state: &mut AppState) -> IntentResult {
    if validator::validate_autocomplete_confirm(state).is_err() {
        return IntentResult::empty();
    }

    // `@` popup confirm has dir-vs-file branching.
    let is_at = state.with_active_input(
        |i| {
            i.autocomplete()
                .as_ref()
                .is_some_and(|ac| matches!(ac.trigger(), AutocompleteTrigger::At))
        },
        || false,
    );
    if is_at {
        return confirm_at_popup(state);
    }

    // `#`/`/` confirm.
    if let Some(name) = state.with_active_input(
        |i| i.autocomplete_selected().map(|m| m.name.clone()),
        || None,
    ) {
        state.update_active_input(|i| i.complete_autocomplete(&name));
        let filter = state
            .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
            .unwrap_or_default();
        let trigger = state.with_active_input(
            |i| i.autocomplete().as_ref().map(AutocompleteState::trigger),
            || None,
        );
        let matches = compute_updated_matches(
            state.active_session().discovered_prompt_templates(),
            trigger,
            &filter,
        );
        state.update_active_input(|i| i.update_autocomplete_matches(matches));
    }
    IntentResult::empty()
}

/// Confirms the current `@` popup selection.
///
/// If the entry is a directory: inserts `name/`, keeps the popup active, and
/// emits `ListDirectory` to descend.
/// If the entry is a file: inserts `name` and deactivates the popup.
fn confirm_at_popup(state: &mut AppState) -> IntentResult {
    // Read everything we need from state up front, then drop the borrows before
    // the mutable `complete_at_entry` call.
    let (name, is_dir) = {
        let filter = state
            .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
            .unwrap_or_default();
        let selected_index = state.with_active_input(
            ChatInputBoxState::autocomplete_selected_index,
            Default::default,
        );
        // Select from the SAME filtered set the popup renders, so a stale or
        // out-of-range index never inserts an entry the user cannot see.
        let Some(entry) = state
            .frontend
            .file_picker
            .visible_entries(&filter)
            .get(selected_index)
            .copied()
        else {
            return IntentResult::empty();
        };
        (entry.name.clone(), entry.is_dir)
    };
    state.update_active_input(|i| {
        i.complete_at_entry(&name, is_dir);
    });
    if is_dir {
        // Popup stays active; the trailing `/` updates the filter, so emit
        // a fresh ListDirectory for the deeper path.
        let filter = state
            .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
            .unwrap_or_default();
        return IntentResult::new_message(emit_list_directory(state, &filter));
    }
    IntentResult::empty()
}

/// Handles `MoveCursorLeft` - moves cursor left, deactivates autocomplete if needed.
pub fn handle_move_cursor_left(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::move_cursor_left);
    let should_deactivate = should_deactivate_on_cursor_move(state);
    if should_deactivate {
        state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    }
    try_reactivate_autocomplete(state);
    if let Some(cmd) = try_reactivate_at_autocomplete(state) {
        return IntentResult::new_message(cmd);
    }
    IntentResult::empty()
}

/// Handles `MoveCursorRight` - moves cursor right, deactivates autocomplete if needed.
pub fn handle_move_cursor_right(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::move_cursor_right);
    let should_deactivate = should_deactivate_on_cursor_move(state);
    if should_deactivate {
        state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    }
    try_reactivate_autocomplete(state);
    if let Some(cmd) = try_reactivate_at_autocomplete(state) {
        return IntentResult::new_message(cmd);
    }
    IntentResult::empty()
}

/// Handles `MoveCursorToStart` - moves cursor to start, deactivates autocomplete.
pub fn handle_move_cursor_to_start(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    state.update_active_input(ChatInputBoxState::move_cursor_to_start);
    IntentResult::empty()
}

/// Handles `MoveCursorToEnd` - moves cursor to end, deactivates autocomplete.
pub fn handle_move_cursor_to_end(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    state.update_active_input(ChatInputBoxState::move_cursor_to_end);
    IntentResult::empty()
}

/// Handles `MoveCursorWordLeft` - moves cursor one word left, deactivates autocomplete.
pub fn handle_move_cursor_word_left(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    state.update_active_input(ChatInputBoxState::move_cursor_word_left);
    IntentResult::empty()
}

/// Handles `MoveCursorWordRight` - moves cursor one word right, deactivates autocomplete.
pub fn handle_move_cursor_word_right(state: &mut AppState) -> IntentResult {
    state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
    state.update_active_input(ChatInputBoxState::move_cursor_word_right);
    IntentResult::empty()
}

/// Handles `MoveCursorUp` - moves up in autocomplete or moves cursor up.
pub fn handle_move_cursor_up(state: &mut AppState) -> IntentResult {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        if is_at_popup(state) {
            let count = at_visible_count(state);
            state.update_active_input(|i| i.autocomplete_move_up_bounded(count));
        } else {
            state.update_active_input(ChatInputBoxState::autocomplete_move_up);
        }
    } else {
        state.update_active_input(ChatInputBoxState::move_cursor_up);
    }
    IntentResult::empty()
}

/// Handles `MoveCursorDown` - moves down in autocomplete or moves cursor down.
pub fn handle_move_cursor_down(state: &mut AppState) -> IntentResult {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        if is_at_popup(state) {
            let count = at_visible_count(state);
            state.update_active_input(|i| i.autocomplete_move_down_bounded(count));
        } else {
            state.update_active_input(ChatInputBoxState::autocomplete_move_down);
        }
    } else {
        state.update_active_input(ChatInputBoxState::move_cursor_down);
    }
    IntentResult::empty()
}

/// Handles `NormalEscape` - no-op for selection (always-selected invariant).
///
/// If the session is busy (streaming/sending), activates the cancel stream
/// confirmation prompt. Otherwise, does nothing.
pub fn handle_normal_escape(state: &mut AppState) -> IntentResult {
    super::validator::validate_normal_escape(state);

    if state.active_session().is_busy()
        || !matches!(state.active_session().phase(), PhaseKind::Idle)
    {
        // Session is busy - show cancel confirmation prompt.
        state.frontend.cancel_stream_prompt = true;
    }

    IntentResult::empty()
}

/// Handles `EnterInsertMode` - pushes Input onto the scope stack.
pub fn handle_enter_insert_mode(state: &mut AppState) -> IntentResult {
    use crate::common::app_state::FocusScope;

    // The pin cursor jump is only for pin → Normal, not pin → Insert.
    if state.active_session().has_saved_history_position() {
        state.active_session_mut().restore_history_position();
    }

    state.frontend.scope_push(FocusScope::Input);
    IntentResult::empty()
}

/// Handles `EnterNormalMode` - pops the scope stack (restores previous scope).
///
/// Simply switches out of the current mode. Does NOT cancel streams or drain
/// queues - the cancel confirmation prompt handles that via `NormalEscape`.
/// Registry-less variant for internal callers that can only reach
/// unmigrated pickers (the session-lifecycle chain); spec-driven
/// close hooks need the app's registry via
/// [`handle_enter_normal_mode_with_pickers`].
pub fn handle_enter_normal_mode(state: &mut AppState) -> IntentResult {
    handle_enter_normal_mode_with_pickers(state, &jinn_picker::PickerRegistry::new())
}

/// Handles `EnterNormalMode` with the picker registry: spec-driven
/// pickers run their `on_close` hook (snapshot revert) before the
/// legacy per-kind restores. The intent handler passes the app's
/// registry so migrated pickers revert correctly.
pub fn handle_enter_normal_mode_with_pickers(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    // If autocomplete is active, dismiss it and stay in the current scope.
    // Two-level ESC: first press closes popup, second press exits mode.
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
        return IntentResult::empty();
    }

    // Spec-driven pickers own their close behavior (snapshot revert).
    if let Some(result) = crate::feat::picker::action::try_close_active(state, pickers) {
        return result;
    }

    // TaskList picker is read-only and always opened from the sidebar task-list section.
    // Pop only the picker to preserve the sidebar scope (rather than clearing all
    // overlays, which would drop the task-list section and strand the user in Normal).
    if state.frontend.picker_kind() == Some(crate::protocol::PickerKind::TaskList)
        && state.frontend.is_picker()
    {
        state.frontend.scope_pop();
        return IntentResult::empty();
    }

    // A pending session creation stash only matters between opening the
    // project picker and confirming session creation. Returning to Normal
    // means that chain was abandoned, so clear any stale stash so it never
    // leaks into a future `n`/`N`.
    state.frontend.pending_creation = None;

    // Clear all overlay scopes - always returns to Normal.
    // Using clear_overlays() instead of pop() ensures that ESC from Input mode
    // always lands in Normal, even when a sidebar scope is stacked below Input
    // (e.g., [Normal, sidebar persona section, Input] → [Normal]).
    state.frontend.scope_clear_overlays();
    IntentResult::empty()
}

/// Checks whether the `#` at the cursor is in a valid position to trigger autocomplete.
fn is_valid_hash_trigger_position(input: &ChatInputBoxState) -> bool {
    let dollar_pos = input.cursor_pos() - 1;
    if dollar_pos == 0 {
        return true;
    }
    let prev = input.grapheme_at(dollar_pos - 1);
    prev == Some(" ") || prev == Some("\n")
}

/// Checks whether the `/` at the cursor is in a valid position to trigger slash autocomplete.
///
/// Valid only at position 0 (start of buffer).
fn is_valid_slash_trigger_position(input: &ChatInputBoxState) -> bool {
    input.cursor_pos() == 1 && input.text().starts_with('/')
}

/// Checks whether the `@` at the cursor is in a valid position to trigger
/// the `@path` file popup.
///
/// Mirrors the hash rule: start-of-buffer, or preceded by a space/newline.
/// Additionally reserves the `@@` seam: if the grapheme before this `@` is
/// another `@`, this returns false so `@@` stays literal (the future
/// `AtAt` picker is not yet wired).
fn is_valid_at_trigger_position(input: &ChatInputBoxState) -> bool {
    let at_pos = input.cursor_pos() - 1;
    if at_pos == 0 {
        return true;
    }
    let prev = input.grapheme_at(at_pos - 1);
    // `@@` seam: do not activate `At` on the second `@`.
    if prev == Some("@") {
        return false;
    }
    prev == Some(" ") || prev == Some("\n")
}

/// Returns true if the cursor has moved outside the autocomplete token region,
/// requiring deactivation.
///
/// Deactivates when the cursor is before `token_start` or past `token_end`.
fn should_deactivate_on_cursor_move(state: &AppState) -> bool {
    let Some((token_start, cursor)) = state.with_active_input(
        |i| Some((i.autocomplete_token_start()?, i.cursor_pos())),
        || None,
    ) else {
        return false;
    };
    if cursor <= token_start {
        return true;
    }
    let token_end = state.with_active_input(|i| compute_token_end(i, token_start), || 0);
    cursor > token_end
}

/// Computes the grapheme index one past the last character of the token
/// that starts at `token_start` (the `#` position).
///
/// Scans forward from `token_start + 1` until whitespace, `#`, or end of buffer.
fn compute_token_end(input: &ChatInputBoxState, token_start: usize) -> usize {
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();
    let mut end = token_start + 1;
    while end < len {
        let g = graphemes.get(end);
        if g.is_none_or(|c| c.trim().is_empty() || *c == "#") {
            break;
        }
        end += 1;
    }
    end
}

/// Performs a fuzzy search against the prompt template store and returns matching entries.
fn compute_matches(store: &PromptTemplateStore, filter: &str) -> Vec<AutocompleteMatch> {
    store
        .fuzzy_search(filter)
        .into_iter()
        .map(|t| AutocompleteMatch {
            name: t.name.clone(),
            description: t.description.clone(),
        })
        .collect()
}

/// Computes matches for the active autocomplete based on its trigger kind.
fn compute_updated_matches(
    store: &PromptTemplateStore,
    trigger: Option<AutocompleteTrigger>,
    filter: &str,
) -> Vec<AutocompleteMatch> {
    match trigger {
        Some(AutocompleteTrigger::Slash) => compute_slash_matches(filter),
        _ => compute_matches(store, filter),
    }
}

/// Performs a fuzzy search against the slash command registry.
fn compute_slash_matches(filter: &str) -> Vec<AutocompleteMatch> {
    let filter_lower = filter.to_lowercase();
    let entries = SlashCommand::all_entries();
    entries
        .into_iter()
        .filter(|e| {
            if filter_lower.is_empty() {
                return true;
            }
            let name_lower = e.name.to_lowercase();
            // Simple fuzzy: check if all filter chars appear in order in the name.
            let mut filter_chars = filter_lower.chars().peekable();
            for c in name_lower.chars() {
                if Some(c) == filter_chars.peek().copied() {
                    filter_chars.next();
                }
            }
            filter_chars.peek().is_none()
        })
        .map(|e| AutocompleteMatch {
            name: e.name,
            description: e.description,
        })
        .collect()
}

/// Scans the buffer to detect if the cursor sits inside a `#token` region.
///
/// Returns `Some((token_start, filter_text))` if the cursor is within a valid
/// token, where `token_start` is the grapheme index of the `#` and `filter_text`
/// is the text between `#+1` and the cursor position.
fn find_hash_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    use unicode_segmentation::UnicodeSegmentation as _;

    let cursor = input.cursor_pos();
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();

    // Scan leftward from the cursor to find a '#' at a valid trigger position.
    let mut i = cursor;
    loop {
        if graphemes.get(i) == Some(&"#") {
            // Check that the '#' is at a valid trigger position.
            let preceded_by_boundary = i == 0
                || graphemes.get(i.wrapping_sub(1)) == Some(&" ")
                || graphemes.get(i.wrapping_sub(1)) == Some(&"\n");
            if !preceded_by_boundary {
                return None;
            }
            // The token extends from i+1 to the next whitespace, '#', or end.
            let mut token_end = i + 1;
            while token_end < len {
                let g = graphemes.get(token_end);
                if g.is_none_or(|c| c.trim().is_empty() || *c == "#") {
                    break;
                }
                token_end += 1;
            }
            // The cursor must be >= i (on the '#' or within the token) and <= token_end.
            if cursor >= i && cursor <= token_end {
                let filter: String = graphemes
                    .get((i + 1)..cursor)
                    .map(|s| s.join(""))
                    .unwrap_or_default();
                return Some((i, filter));
            }
            return None;
        }
        // If we hit whitespace going left, stop - no valid token.
        let g = graphemes.get(i);
        if g.is_some_and(|c| c.trim().is_empty()) {
            return None;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Scans the buffer to detect if the cursor sits inside a `/command` region at position 0.
///
/// Returns `Some((token_start, filter_text))` if the buffer starts with `/` and the
/// cursor is within the token, where `token_start` is 0 and `filter_text` is the text
/// between position 1 and the cursor.
fn find_slash_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    use unicode_segmentation::UnicodeSegmentation as _;

    if !input.text().starts_with('/') {
        return None;
    }

    let cursor = input.cursor_pos();
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();

    // The token extends from 1 to the next whitespace or end.
    let mut token_end = 1;
    while token_end < len {
        let g = graphemes.get(token_end);
        if g.is_none_or(|c| c.trim().is_empty()) {
            break;
        }
        token_end += 1;
    }

    // The cursor must be >= 0 and <= token_end.
    if cursor <= token_end {
        let filter: String = graphemes
            .get(1..cursor)
            .map(|s| s.join(""))
            .unwrap_or_default();
        return Some((0, filter));
    }
    None
}

/// Scans the buffer to detect if the cursor sits inside an `@path` region.
///
/// Mirrors [`find_hash_token_at_cursor`] but for `@`. The token extends from
/// the `@` to the next whitespace. The `@@` seam is reserved: an `@` preceded
/// by another `@` is not a valid `At` trigger (so `@@` stays literal).
fn find_at_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    use unicode_segmentation::UnicodeSegmentation as _;

    let cursor = input.cursor_pos();
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();

    // Scan leftward from the cursor to find an `@` at a valid trigger position.
    let mut i = cursor;
    loop {
        if graphemes.get(i) == Some(&"@") {
            let preceded_by_boundary = i == 0
                || graphemes.get(i.wrapping_sub(1)) == Some(&" ")
                || graphemes.get(i.wrapping_sub(1)) == Some(&"\n");
            // `@@` seam: the second `@` is not an `At` trigger.
            let is_at_at = graphemes.get(i.wrapping_sub(1)) == Some(&"@");
            if !preceded_by_boundary || is_at_at {
                return None;
            }
            // The token extends from i+1 to the next whitespace, `@`, or end.
            let mut token_end = i + 1;
            while token_end < len {
                let g = graphemes.get(token_end);
                if g.is_none_or(|c| c.trim().is_empty() || *c == "@") {
                    break;
                }
                token_end += 1;
            }
            if cursor >= i && cursor <= token_end {
                let filter: String = graphemes
                    .get((i + 1)..cursor)
                    .map(|s| s.join(""))
                    .unwrap_or_default();
                return Some((i, filter));
            }
            return None;
        }
        let g = graphemes.get(i);
        if g.is_some_and(|c| c.trim().is_empty()) {
            return None;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Attempts to re-activate autocomplete if the cursor sits inside a token region.
///
/// Checks for both `#token` and `/command` regions.
fn try_reactivate_autocomplete(state: &mut AppState) {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        return;
    }

    // Try slash command token first (position 0).
    let slash_hit = state.with_active_input(find_slash_token_at_cursor, || None);
    if let Some((token_start, filter)) = slash_hit {
        let matches = compute_slash_matches(&filter);
        state.update_active_input(|i| {
            i.activate_autocomplete(token_start, AutocompleteTrigger::Slash, matches);
        });
        return;
    }

    // Try hash token.
    let Some((token_start, filter)) = state.with_active_input(find_hash_token_at_cursor, || None)
    else {
        return;
    };
    let matches = compute_matches(
        state.active_session().discovered_prompt_templates(),
        &filter,
    );
    state.update_active_input(|i| {
        i.activate_autocomplete(token_start, AutocompleteTrigger::Hash, matches);
    });
}

/// Attempts to re-activate the `@` file popup if the cursor sits in an `@path`
/// region.
///
/// On success, emits a [`ListDirectory`] command so the actor lists the dir for
/// the current filter (the popup reads `frontend.file_picker`).
fn try_reactivate_at_autocomplete(state: &mut AppState) -> Option<ListDirectory> {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        return None;
    }
    let (token_start, filter) = state.with_active_input(find_at_token_at_cursor, || None)?;
    state.update_active_input(|i| {
        i.activate_autocomplete(token_start, AutocompleteTrigger::At, Vec::new());
    });
    Some(emit_list_directory(state, &filter))
}

/// Builds and emits a [`ListDirectory`] command for the given `@` filter,
/// resolving it against the session cwd/home and bumping the staleness id.
///
/// Returns `None` if the active session is missing.
fn emit_list_directory(state: &mut AppState, filter: &str) -> ListDirectory {
    let session = state.active_session();
    let cwd = session.cwd().to_path_buf();
    let home = home_dir();
    let dir = crate::feat::file_lister::resolve_list_dir(filter, &cwd, &home);
    // Bump the expected request id so stale replies are dropped.
    let request_id = state
        .frontend
        .file_picker
        .expected_request_id
        .wrapping_add(1);
    state.frontend.file_picker.expected_request_id = request_id;
    state.frontend.file_picker.loading = true;
    let session_id = state.session.active_session_id().clone();
    ListDirectory {
        session_id,
        path: dir,
        request_id,
    }
}

/// Returns the user's home directory. Falls back to cwd if $HOME is unset.
fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::PathBuf::from,
    )
}

/// Returns true if the active autocomplete is the `@path` file popup.
fn is_at_popup(state: &AppState) -> bool {
    state.with_active_input(
        |i| {
            i.autocomplete()
                .as_ref()
                .is_some_and(|ac| matches!(ac.trigger(), AutocompleteTrigger::At))
        },
        || false,
    )
}

/// Returns the number of entries the `@` popup currently shows (after the
/// prefix filter), so arrow-key navigation is bounded by the visible set.
fn at_visible_count(state: &AppState) -> usize {
    let filter = state
        .with_active_input(ChatInputBoxState::autocomplete_filter, || None)
        .unwrap_or_default();
    state.frontend.file_picker.visible_entries(&filter).len()
}
