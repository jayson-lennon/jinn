//! Chat input box intent handlers.
//!
//! Handles 16 chat-input intents:
//!
//! - **InsertChar** - inserts a character, manages autocomplete triggering/filtering/expansion.
//! - **PasteText** - bulk inserts pasted text, deactivates autocomplete.
//! - **DeleteGrapheme** - backspace with autocomplete awareness.
//! - **DeleteGraphemeForward** - forward delete with autocomplete awareness.
//! - **SubmitMessage** - validates, extracts text, resets buffer, returns `EnqueueUserMessage`.
//! - **AutocompleteConfirm** - confirms autocomplete selection or falls back to tab switch.
//! - **Cursor movement** (8 intents) - move cursor, optionally deactivating autocomplete.
//! - **EnterInsertMode** - switches to Input mode.
//! - **EnterNormalMode** - clears picker, switches to Normal mode.
//!
//! The cancel-stream confirmation is deliberately absent: escape in Normal
//! mode is a session concern, raised and confirmed by the kernel.

use crate::AutocompleteMatch;
use crate::AutocompleteTrigger;
use crate::ChatInputBoxState;
use crate::InputMode;
use jinn_chat_input_msg::{
    AutocompleteState, EnqueueUserMessage, ListDirectory, SlashCommand, SubmitSteeringMessage,
    home_dir, resolve_list_dir,
};
use jinn_context::PromptTemplateStore;
use jinn_core_types::PinPosition;
use jinn_core_types::SessionId;
use jinn_kernel::AppState;
use jinn_kernel::protocol::{ChatEntry, IntentResult};
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::PersistSession;
use jinn_slices::PhaseView;
use unicode_segmentation::UnicodeSegmentation as _;

use super::token::{
    compute_token_end, find_at_token_at_cursor, find_hash_token_at_cursor,
    find_slash_token_at_cursor, is_valid_at_trigger_position, is_valid_hash_trigger_position,
    is_valid_slash_trigger_position,
};
use super::validator;
use marked_path::{Absolute, MarkedPathBuf};

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
        // `@@` seam: typing `@` while an `At` popup is active forms `@@`.
        // No handler is wired for it, so deactivate the popup and let it
        // stay literal.
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
/// The mutation itself is the shared paste-append helper in the
/// crossing crate — the same implementation the dictation actor's
/// utterance commits run through — so the two paths cannot drift.
pub fn handle_paste_text(text: &str, state: &mut AppState) -> IntentResult {
    state.update_active_input(|i| jinn_chat_input_msg::append_paste_to_buffer(i, text));
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
pub fn handle_submit_message(
    state: &mut AppState,
    config: &jinn_config::ConfigLayer,
) -> IntentResult {
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        return handle_submit_message_with_autocomplete(state, config);
    }

    if validator::validate_submit_message(state).is_err() {
        return IntentResult::empty();
    }

    let session_id = state.session.active_session_id().clone();
    let input_text = state.with_active_input(|i| i.text().to_owned(), String::new);

    // Check for slash command execution.
    if let Some(command_name) = input_text.strip_prefix('/') {
        // The first word is the command; the rest, verbatim, is its argument.
        // Splitting on the first whitespace run only, so an argument with
        // spaces inside it survives.
        let mut parts = command_name.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let argument = parts.next().unwrap_or("").trim_start();
        if let Some(cmd) = SlashCommand::lookup(cmd) {
            state.update_active_input(ChatInputBoxState::reset);
            return with_mark_interacted(
                session_id,
                execute_slash_command(cmd, argument, state, config),
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
fn handle_submit_message_with_autocomplete(
    state: &mut AppState,
    config: &jinn_config::ConfigLayer,
) -> IntentResult {
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
                // Same split as the non-autocomplete path, so `/export` behaves
                // identically whether or not the popup happened to be open.
                let mut parts = command_name.splitn(2, char::is_whitespace);
                let cmd = parts.next().unwrap_or("");
                let argument = parts.next().unwrap_or("").trim_start();
                if let Some(cmd) = SlashCommand::lookup(cmd) {
                    state.update_active_input(ChatInputBoxState::reset);
                    return with_mark_interacted(
                        session_id,
                        execute_slash_command(cmd, argument, state, config),
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
/// [`jinn_session_state::ChatSessionState::push_entry`], so
/// both the enqueued message and the steering fragment flow through the single
/// expansion site. When steering, the buffer accumulates the raw display text.
fn route_to_enqueue_or_steer(
    state: &AppState,
    session_id: &SessionId,
    display: String,
) -> IntentResult {
    // Seed-mode bots are being composed, not run. A submission lands in
    // the bot's context as a *pinned* entry and dispatches nothing.
    //
    // Pinning is not cosmetic: a reset run force-excludes every non-pinned
    // entry, so a seed that is not pinned is deleted from context by the very
    // next fire. This branch is what makes seeded instructions survive.
    if let Some(result) = seed_mode_submission(state, session_id, display.clone()) {
        return result;
    }

    let mode = state.with_active_input(ChatInputBoxState::input_mode, Default::default);
    let phase = state.active_session().phase();
    match (mode, phase) {
        (InputMode::Steer, PhaseView::Idle) | (InputMode::Queue, _) => {
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

/// Pins a submission into a preparing bot's context without dispatching.
///
/// Returns `None` for any session that is not a bot in prep mode, so
/// the caller falls through to the normal routing.
fn seed_mode_submission(
    state: &AppState,
    session_id: &SessionId,
    display: String,
) -> Option<IntentResult> {
    let session = state.session.get(session_id)?;
    if !session.is_bot() || !session.bot_is_prepping() {
        return None;
    }
    let entry = ChatEntry::user(display);
    Some(
        IntentResult::empty()
            // The pin travels with the push rather than as a second
            // message: two messages race, and a pin that loses finds no
            // entry to attach to and is dropped without a word.
            .with_message(PushChatEntry {
                session_id: session_id.clone(),
                entry,
                pin: Some(PinPosition::Relative),
            })
            .with_message(PersistSession {
                session_id: session_id.clone(),
            }),
    )
}

/// Prepends a `MarkSessionInteracted` message to the result.
fn with_mark_interacted(session_id: SessionId, mut result: IntentResult) -> IntentResult {
    result.messages.insert(
        0,
        jinn_kernel::common::bridge::Bridge::publish_closure(MarkSessionInteracted { session_id }),
    );
    result
        .message_names
        .insert(0, std::any::type_name::<MarkSessionInteracted>());
    result
}

/// Executes a slash command.
///
/// The two `expect`s below are typed-path operations on model text the
/// branch itself already guarantees: a `/`-prefixed argument is absolute by
/// the `starts_with('/')` guard, and a non-`/` argument joins onto the
/// session cwd (already typed `Absolute`).
#[expect(
    clippy::expect_used,
    reason = "guarded typed-path re-typing and joins on a typed-absolute cwd"
)]
fn execute_slash_command(
    command: SlashCommand,
    argument: &str,
    state: &mut AppState,
    config: &jinn_config::ConfigLayer,
) -> IntentResult {
    match command {
        SlashCommand::Compact | SlashCommand::CompactAll => {
            let compact_all = matches!(command, SlashCommand::CompactAll);
            let session_id = state.session.active_session_id().clone();
            IntentResult::new_message(jinn_context_curation_msg::TriggerCompaction {
                session_id,
                compact_all,
            })
        }
        SlashCommand::New => {
            jinn_kernel::session_lifecycle::intent::handle_session_new(state, config)
        }
        SlashCommand::Export => {
            let session_id = state.session.active_session_id().clone();
            // The argument is model text; an empty argument means the
            // export slice picks a default. A relative argument resolves
            // against the session cwd so the destination is always absolute.
            let path = if argument.is_empty() {
                None
            } else if argument.starts_with('/') {
                Some(
                    MarkedPathBuf::<Absolute>::new(argument)
                        .expect("a `/`-prefixed export target is absolute"),
                )
            } else {
                Some(
                    state
                        .session
                        .active_session()
                        .cwd()
                        .join(argument)
                        .expect("joining a relative export target keeps the cwd absolute"),
                )
            };
            let path = match path {
                Some(p) => p,
                None => state.session.active_session().cwd().clone(),
            };
            IntentResult::new_message(jinn_export_msg::ExportSessionToFile { session_id, path })
        }
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
        let entry = state.frontend.with_file_picker(|picker| {
            picker
                .visible_entries(&filter)
                .get(selected_index)
                .map(|e| (e.name.clone(), e.is_dir))
        });
        let Some((name, is_dir)) = entry.flatten() else {
            return IntentResult::empty();
        };
        (name, is_dir)
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

/// Handles `EnterInsertMode` - pushes Input onto the scope stack.
pub fn handle_enter_insert_mode(state: &mut AppState) -> IntentResult {
    use jinn_slices::FocusScope;

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
///
/// Slice-owned pickers bind `<esc>` in their own scope, so an escape that
/// reaches here has already left any open picker and reverted its snapshot.
/// The read-only task-list browser is the one exception: it is sidebar-opened
/// and declares its own single-scope pop as a `ScopeSignal`.
pub fn handle_enter_normal_mode(
    state: &mut AppState,
    _config: &jinn_config::ConfigLayer,
) -> IntentResult {
    // If autocomplete is active, dismiss it and stay in the current scope.
    // Two-level ESC: first press closes popup, second press exits mode.
    if state.with_active_input(|i| i.autocomplete().is_some(), || false) {
        state.update_active_input(ChatInputBoxState::deactivate_autocomplete);
        return IntentResult::empty();
    }

    // A pending session creation stash only matters between opening a
    // project picker and confirming session creation. Returning to Normal
    // means that chain was abandoned, so clear any stale stash so it never
    // leaks into a future `n`.
    state.frontend.pending_creation = None;

    // Clear all overlay scopes - always returns to Normal.
    // Using clear_overlays() instead of pop() ensures that ESC from Input mode
    // always lands in Normal, even when a sidebar scope is stacked below Input
    // (e.g., [Normal, sidebar persona section, Input] -> [Normal]).
    state.frontend.scope_clear_overlays();
    IntentResult::empty()
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
/// the current filter (the popup reads the file-picker cell).
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
    let cwd = session.cwd().clone();
    let home = home_dir();
    let dir = resolve_list_dir(filter, &cwd, &home);
    // Bump the expected request id so stale replies are dropped, and show
    // the spinner until the reply the id now names lands. This is the
    // IntentHandler's half of the staleness rule; the actor's is the
    // matching check before it writes.
    let request_id = state
        .frontend
        .with_file_picker(|picker| picker.expected_request_id.wrapping_add(1))
        .unwrap_or_default();
    state.frontend.update_file_picker(|picker| {
        picker.expected_request_id = request_id;
        picker.loading = true;
    });
    let session_id = state.session.active_session_id().clone();
    ListDirectory {
        session_id,
        path: dir,
        request_id,
    }
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
    state
        .frontend
        .with_file_picker(|picker| picker.visible_entries(&filter).len())
        .unwrap_or_default()
}
