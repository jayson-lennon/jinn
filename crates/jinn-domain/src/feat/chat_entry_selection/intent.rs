//! Chat entry selection intent handlers - navigate and pin entries.

use crate::ChatEntry;
use crate::ChatEntryKind;
use crate::common::app_state::AppState;
use crate::feat::chat_input::protocol::command::PushChatEntry;
use crate::feat::context::protocol::command::{PinChatEntry, UnpinChatEntry};
use crate::feat::session::ChatSessionState;
use crate::feat::session::protocol::session_fork_requested::SessionForkRequested;
use crate::feat::ui::chat_log::visual_item::VisualItem;
use crate::protocol::{IntentResult, PinPosition};

use super::validator;

/// Advances the selection cursor by one entry, paging the viewport if needed.
///
/// Returns `false` if already at the last entry (no-op).
pub(crate) fn advance_selection_one(session: &mut ChatSessionState) -> bool {
    let visible = session.visible_entry_range();
    let current = session.selected_entry_index();
    let items = session.visual_items_snapshot();
    let max = if items.is_empty() {
        session.history().len().saturating_sub(1)
    } else {
        items.len().saturating_sub(1)
    };

    let Some(cur) = current else {
        // No selection - select first entry if history exists.
        if !session.history().is_empty() {
            session.select_next_entry();
            return true;
        }
        return false;
    };

    if cur >= max {
        return false; // At bottom - no-op.
    }

    let last_visible = if visible.is_empty() {
        None
    } else {
        Some(visible.end.saturating_sub(1))
    };

    if last_visible == Some(cur) {
        let viewport_height = session.viewport_height_value().max(1);
        session.scroll_down(viewport_height);
    }
    session.select_next_entry();
    true
}
/// Selects the next chat entry in the active session.
///
/// If the cursor is on the last visible entry, pages the viewport down first,
/// then advances the cursor by exactly 1 (not jump to first visible in new viewport).
/// Clamps at the last entry in history - no wrapping.
pub fn handle_select_next(state: &mut AppState) -> IntentResult {
    validator::validate_chat_entry_select_next(state);
    advance_selection_one(state.active_session_mut());
    IntentResult::empty()
}

/// Selects the previous chat entry in the active session.
///
/// If the cursor is on the first visible entry, pages the viewport up first,
/// then moves the cursor back by exactly 1. Clamps at entry 0 - no wrapping.
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
pub fn handle_select_prev(state: &mut AppState) -> IntentResult {
    validator::validate_chat_entry_select_prev(state);
    let session = state.active_session_mut();
    let visible = session.visible_entry_range();
    let current = session.selected_entry_index();

    if let Some(cur) = current {
        if cur == 0 {
            // Already at first entry - no-op.
            return IntentResult::empty();
        }
        // Check if cursor is at first visible entry.
        let first_visible = if visible.is_empty() {
            None
        } else {
            Some(visible.start)
        };
        if first_visible == Some(cur) {
            // At first visible - page up, then move back by exactly 1.
            let viewport_height = session.viewport_height_value().max(1);
            session.scroll_up(viewport_height);
            session.select_prev_entry();
        } else {
            session.select_prev_entry();
        }
    } else if !session.history().is_empty() {
        session.select_prev_entry();
    }
    IntentResult::empty()
}

/// Resolve the anchor history index for a compaction jump.
///
/// Uses the current selection's history index. When the selection sits on a
/// [`CollapsedIgnoredBlock`], `selected_history_index` returns `None`, so the
/// block's concrete `start` index is used instead — the jump originates from
/// the block's first entry rather than the end of history.
///
/// When nothing is selected at all (no cursor id), falls back to
/// `history.len()` — a sentinel one past the newest entry. This keeps the
/// forward/backward scans exclusive of the anchor while still letting a first
/// press land on the newest (for `[c`) compaction.
///
/// Returns `None` when the history is empty (no-op for the caller).
fn jump_anchor(session: &ChatSessionState) -> Option<usize> {
    let history = session.history();
    if history.is_empty() {
        return None;
    }
    if let Some(idx) = session.selected_history_index() {
        return Some(idx);
    }
    // selected_history_index is None: either nothing is selected, or the
    // selection is on a collapsed block. For a collapsed block, anchor on its
    // first entry so the jump is relative to the block's position, not the end.
    if let Some(VisualItem::CollapsedIgnoredBlock { start, .. }) = session.selected_visual_item() {
        return Some(start);
    }
    Some(history.len())
}

/// Jump the cursor to the next (newer) compaction summary entry.
///
/// Anchors on the current selection; when nothing is selected, the anchor
/// is the sentinel past the newest entry (see `jump_anchor`).
/// Scans forward — exclusive of the anchor — for the first compaction entry.
/// Clamps (no wrap): a silent no-op if no compaction exists beyond the
/// anchor. The viewport auto-follows the new cursor.
pub fn handle_jump_next_entry<F>(state: &mut AppState, cb: F) -> IntentResult
where
    F: Fn(&ChatEntry) -> bool,
{
    let target_id = {
        let session = state.active_session();
        let Some(anchor) = jump_anchor(session) else {
            return IntentResult::empty();
        };
        session
            .history()
            .get(anchor + 1..)
            .and_then(|tail| tail.iter().find(|&e| cb(e)))
            .map(|e| e.id.clone())
    };

    if let Some(id) = target_id {
        state.active_session_mut().set_selected_cursor_id(id);
    }
    IntentResult::empty()
}

/// Jump the cursor to the previous (older) compaction summary entry.
///
/// Anchors on the current selection; when nothing is selected, the anchor
/// is the sentinel past the newest entry (see `jump_anchor`).
/// Scans backward — exclusive of the anchor — for the first compaction entry.
/// Clamps (no wrap): a silent no-op if no compaction exists beyond the
/// anchor. The viewport auto-follows the new cursor.
pub fn handle_jump_prev_entry<F>(state: &mut AppState, cb: F) -> IntentResult
where
    F: Fn(&ChatEntry) -> bool,
{
    let target_id = {
        let session = state.active_session();
        let Some(anchor) = jump_anchor(session) else {
            return IntentResult::empty();
        };
        session
            .history()
            .get(..anchor)
            .and_then(|head| head.iter().rfind(|&e| cb(e)))
            .map(|e| e.id.clone())
    };

    if let Some(id) = target_id {
        state.active_session_mut().set_selected_cursor_id(id);
    }
    IntentResult::empty()
}

/// Toggles the pin state of the currently selected chat entry.
///
/// If the entry is pinned, sends an `UnpinChatEntry` command.
/// If the entry is not pinned, sends a `PinChatEntry` command with `Relative` position.
pub fn handle_pin_selected(state: &mut AppState) -> IntentResult {
    tracing::debug!("handle_pin_selected called");
    if validator::validate_chat_entry_pin_selected(state).is_err() {
        tracing::debug!("validation failed");
        return IntentResult::empty();
    }

    let session_id = state.session.active_session_id().clone();
    let Some(selected) = state.active_session().selected_entry() else {
        tracing::debug!("active session doesnt match session id");
        return IntentResult::empty();
    };
    let entry_id = selected.id.clone();

    if selected.is_pinned() {
        IntentResult::new_message(UnpinChatEntry {
            session_id,
            entry_id,
        })
    } else {
        IntentResult::new_message(PinChatEntry {
            session_id,
            entry_id,
            position: PinPosition::Relative,
        })
    }
}

/// Toggles expand/collapse of the selected tool entry (tool call, tool result, or annotation).
pub fn handle_expand_tool_entry(state: &mut AppState) -> IntentResult {
    if validator::validate_expand_tool_entry(state).is_err() {
        return IntentResult::empty();
    }

    let Some(entry_id) = state.active_session().selected_entry_id().cloned() else {
        return IntentResult::empty();
    };

    state.active_session_mut().toggle_expand_entry(entry_id);
    IntentResult::empty()
}

/// Toggles visibility of ignored entries in the selected visual item's block.
///
/// - If the selected item is a `CollapsedIgnoredBlock` → expand it.
/// - If the selected item is an ignored entry in an expanded block → collapse the block.
/// - If the selected item is a non-ignored entry → no-op.
/// - If nothing is selected → no-op.
pub fn handle_toggle_ignored_block(state: &mut AppState) -> IntentResult {
    let session = state.active_session();
    let Some(vi_idx) = session.selected_entry_index() else {
        return IntentResult::empty();
    };

    let history = session.history();
    let items = session.visual_items_snapshot();

    let entry_id = match items.get(vi_idx) {
        Some(VisualItem::CollapsedIgnoredBlock { start, .. }) => {
            // Block is collapsed → expand it.
            let Some(entry) = history.get(*start) else {
                return IntentResult::empty();
            };
            entry.id.clone()
        }
        Some(VisualItem::Entry(hist_idx)) => {
            // Entry might be in an expanded ignored block → collapse it.
            let Some(entry) = history.get(*hist_idx) else {
                return IntentResult::empty();
            };
            if entry.is_in_context() {
                return IntentResult::empty();
            }
            entry.id.clone()
        }
        None => return IntentResult::empty(),
    };

    drop(items);
    state
        .active_session_mut()
        .toggle_ignored_block_visibility(&entry_id);
    IntentResult::empty()
}

/// Forks the session at the currently selected chat entry.
///
/// Emits a `SessionForkRequested` command with `at_ordinal` set to the
/// selected entry's index in the history. The session actor handles the
/// actual fork in SQLite and loads the new session.
///
/// No longer panics — returns `IntentResult::empty()` if the selected
/// entry cannot be resolved after validation.
pub fn handle_fork_from_entry(state: &mut AppState) -> IntentResult {
    if super::validator::validate_fork_from_entry(state).is_err() {
        return IntentResult::empty();
    }

    let source_session_id = state.session.active_session_id().clone();
    let Some(at_ordinal) = state.active_session().selected_history_index() else {
        return IntentResult::empty();
    };

    state.session.begin_load(source_session_id.clone());

    IntentResult::new_message(SessionForkRequested {
        source_session_id,
        at_ordinal,
    })
}

/// Creates a new empty session seeded with the selected entry's text.
///
/// Unlike [`handle_fork_from_entry`], the new session carries no inherited
/// history. Only the selected entry is copied in (kind preserved) as the sole
/// history entry. Restricted to User and Assistant entries.
///
/// Creates a fresh session via the no-setup lifecycle path (inheriting model,
/// persona, reasoning effort, and CWD), then chains a [`PushChatEntry`] command
/// so the session actor pushes and persists the seed. The handler does NOT push
/// the seed in-memory — the actor owns that, and a double push would result.
///
/// No auto-dispatch: the new session stays idle, ready for the next user message.
///
/// [`PushChatEntry`]: crate::feat::chat_input::protocol::command::PushChatEntry
pub fn handle_new_session_from_entry(state: &mut AppState) -> IntentResult {
    if super::validator::validate_new_session_from_entry(state).is_err() {
        return IntentResult::empty();
    }

    // Capture the source entry's text and kind BEFORE the active session
    // changes — the lifecycle setup switches the active session to the new one.
    let (text, is_assistant) = {
        let Some(entry) = state.active_session().selected_entry() else {
            return IntentResult::empty();
        };
        (
            entry.text(),
            matches!(entry.kind, ChatEntryKind::Assistant(_)),
        )
    };

    // Create a fresh empty session (inherits model/persona/CWD).
    let result = crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(
        state,
        "",
        &[],
        None,
    );

    // After setup, the active session is the new one.
    let new_session_id = state.session.active_session_id().clone();
    // Build a fresh seed entry preserving kind (new id/timing).
    let seed = build_seed_entry(text, is_assistant);

    // Chain PushChatEntry; the session actor pushes and persists. Do NOT push
    // in-memory — that would double-push against the actor's push_entry.
    result.with_message(PushChatEntry {
        session_id: new_session_id,
        entry: seed,
    })
}

/// Build a fresh seed entry from captured text, preserving the source kind.
///
/// Returns a new [`ChatEntry`] with a fresh id and timing. `is_assistant`
/// selects the kind: an Assistant seed preserves natural priming (the LLM
/// sees its own prior framing); a User seed seeds as a user turn.
fn build_seed_entry(text: String, is_assistant: bool) -> ChatEntry {
    if is_assistant {
        ChatEntry::assistant(text)
    } else {
        ChatEntry::user(text)
    }
}
/// Yanks (copies) the selected chat entry's raw content to the clipboard.
///
/// Extracts the entry's clipboard text via [`ChatEntry::yank_text`] — tool
/// results yield untruncated output without the tool-name prefix, tool calls
/// yield the raw JSON arguments — and stashes it in [`TuiSignals::yank_text`]
/// for the TUI layer to write to the system clipboard.
pub fn handle_yank_selected(state: &mut AppState) -> IntentResult {
    if validator::validate_yank_selected(state).is_err() {
        return IntentResult::empty();
    }
    let Some(entry) = state.active_session().selected_entry() else {
        return IntentResult::empty();
    };
    let text = entry.yank_text();
    state
        .frontend
        .update_scope(|s| s.signals.yank_text = Some(text));
    IntentResult::empty()
}

/// Toggle the `ignored` flag on the currently selected chat entry, with
/// sweep support for holding `x`.
///
/// **First press:** validates, toggles the entry, captures the resulting
/// `ContextOverride`, advances the cursor, and stores the sweep state.
///
/// **Subsequent presses within 100ms:** applies the captured state (not a
/// toggle) to the now-selected entry, advances the cursor, and refreshes
/// the sweep timestamp. Pinned entries are skipped during the sweep.
///
/// The sweep state is cleared by either a >100ms gap or any non-
/// `ChatEntryIgnoreSelected` intent.
///
/// Returns gracefully if the selected entry cannot be resolved after
/// validation (e.g. collapsed ignored block).
pub fn handle_ignore_selected(state: &mut AppState) -> IntentResult {
    if let Some(target) = state.active_session_mut().take_ignore_sweep() {
        return super::ignore_sweep::run_sweep(state, target);
    }
    handle_fresh_toggle(state)
}

/// Fresh press of `x`: validate, toggle the entry, capture sweep state,
/// propagate shown blocks, advance cursor.
fn handle_fresh_toggle(state: &mut AppState) -> IntentResult {
    use crate::feat::context::protocol::event::ContextOverrideChanged;
    use crate::feat::session_lifecycle::protocol::command::PersistSession;

    // If cursor is on a collapsed block, skip past it before validation.
    // Validation calls selected_entry() which returns None for collapsed blocks.
    if state.active_session().is_selected_collapsed_block() {
        advance_selection_one(state.active_session_mut());
        return IntentResult::empty();
    }

    if validator::validate_chat_entry_ignore_selected(state).is_err() {
        return IntentResult::empty();
    }

    // Toggle the entry's ignore state.
    let maybe_entry_id = state.active_session_mut().toggle_entry_ignored();

    // Capture the resulting state on the toggled entry.
    let Some(selected) = state.active_session().selected_entry() else {
        return IntentResult::empty();
    };
    let captured = selected.context_override();
    let entry_id = selected.id.clone();

    // Propagate shown blocks if this toggle brought an entry into context
    // and split a previously shown excluded block.
    state
        .active_session_mut()
        .propagate_shown_on_unignore(&entry_id);

    // Advance cursor.
    advance_selection_one(state.active_session_mut());

    // Store sweep state for potential continuation.
    state.active_session_mut().set_ignore_sweep(captured);

    let session_id = state.active_session().session_id().clone();

    let mut result = IntentResult::empty().with_message(PersistSession {
        session_id: session_id.clone(),
    });

    if let Some(id) = maybe_entry_id {
        result = result.with_message(ContextOverrideChanged {
            session_id,
            entry_id: id,
        });
    }

    result
}
/// Press of `r`: validate, reset the selected entry's override to
/// `ContextOverride::Default`, advance the cursor. Each press resets one entry,
/// so holding `r` sweeps resets via cursor advance with no dedicated state.
///
/// Pinned entries and collapsed blocks are skipped (cursor advances past
/// them), mirroring the `x`-sweep so `r` sweeps transparently across obstacles.
/// Already-`Default` entries are silent no-ops.
///
/// Returns gracefully if the selected entry cannot be resolved after
/// validation (e.g. collapsed ignored block).
pub fn handle_reset_selected(state: &mut AppState) -> IntentResult {
    use crate::feat::context::protocol::event::ContextOverrideChanged;
    use crate::feat::session::chat_entry::ChatEntry;
    use crate::feat::session_lifecycle::protocol::command::PersistSession;
    use crate::protocol::ContextOverride;

    // Skip past obstacles before validation, mirroring the x-sweep
    // (ignore_sweep.rs): pinned entries and collapsed blocks are passed
    // over transparently so a held `r` continues sweeping entries beyond them.
    let session = state.active_session();
    let on_obstacle = session.is_selected_collapsed_block()
        || session.selected_entry().is_some_and(ChatEntry::is_pinned);
    if on_obstacle {
        advance_selection_one(state.active_session_mut());
        return IntentResult::empty();
    }

    if validator::validate_chat_entry_reset_selected(state).is_err() {
        return IntentResult::empty();
    }

    // Reset the entry's override to Default.
    let maybe_entry_id = state
        .active_session_mut()
        .set_entry_context_override(ContextOverride::Default);

    // Propagate shown blocks if this reset brought an entry into context
    // (ForcedExclude -> Default) and split a previously shown excluded block.
    if let Some(id) = &maybe_entry_id {
        state.active_session_mut().propagate_shown_on_unignore(id);
    }

    // Advance cursor (enables free hold-to-sweep via cursor advance).
    advance_selection_one(state.active_session_mut());

    // No change means no persistence or override events.
    let Some(id) = maybe_entry_id else {
        return IntentResult::empty();
    };

    let session_id = state.active_session().session_id().clone();
    IntentResult::empty()
        .with_message(PersistSession {
            session_id: session_id.clone(),
        })
        .with_message(ContextOverrideChanged {
            session_id,
            entry_id: id,
        })
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
    use crate::common::app_state::AppState;
    use crate::feat::session::chat_entry::ChangeSource;
    use crate::feat::session::tool_result_status::ToolResultStatus;
    use crate::protocol::{ChatEntry, ContextOverride, PinPosition};

    use super::*;

    #[rstest::rstest]
    fn chat_entry_select_next_increments_index() {
        // Given a state with entries and selection at first.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        // After push, selection is at index 1 (last pushed). Move to 0.
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling select next.
        let _result = handle_select_next(&mut state);

        // Then the second entry is selected.
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
    }

    #[rstest::rstest]
    fn chat_entry_select_next_returns_no_commands() {
        // Given a state with entries and selection at first.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        // After push, selection is at index 1 (last pushed). Move to 0.
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling select next.
        let result = handle_select_next(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn chat_entry_select_prev_decrements_index() {
        // Given a state with entries and selection at last.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().select_prev_entry();

        // When handling select prev.
        let _result = handle_select_prev(&mut state);

        // Then selection moved.
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
    }

    #[rstest::rstest]
    fn chat_entry_select_prev_returns_no_commands() {
        // Given a state with entries and selection at last.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().select_prev_entry();

        // When handling select prev.
        let result = handle_select_prev(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn chat_entry_pin_selected_returns_pin_command() {
        // Given a state with a selected entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling pin selected.
        let result = handle_pin_selected(&mut state);

        // Then a PinChatEntry command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PinChatEntry"))
        );
    }

    #[rstest::rstest]
    fn chat_entry_pin_selected_noop_with_empty_history() {
        // Given a state with no history.
        let mut state = AppState::default_with_scope_focus();

        // When handling pin selected.
        let result = handle_pin_selected(&mut state);

        // Then no commands.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn toggle_pin_selected_returns_unpin_command_when_pinned() {
        // Given a state with a selected pinned entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();
        state
            .active_session_mut()
            .pin_entry(&entry_id, PinPosition::Top);

        // When handling pin selected (toggle).
        let result = handle_pin_selected(&mut state);

        // Then an UnpinChatEntry command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("UnpinChatEntry"))
        );
    }

    #[rstest::rstest]
    fn expand_tool_entry_toggles_expanded_state() {
        // Given a state with a selected tool result.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                "output",
                ToolResultStatus::Success,
            ));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();

        // When handling expand tool entry.
        let _result = handle_expand_tool_entry(&mut state);

        // Then the entry is expanded.
        assert!(state.active_session().is_entry_expanded(&entry_id));
    }

    #[rstest::rstest]
    fn expand_tool_entry_returns_no_commands() {
        // Given a state with a selected tool result.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                "output",
                ToolResultStatus::Success,
            ));
        state.active_session_mut().select_next_entry();
        let _entry_id = state.active_session().selected_entry_id().unwrap().clone();

        // When handling expand tool entry.
        let result = handle_expand_tool_entry(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn expand_tool_entry_toggles_back_to_collapsed() {
        // Given a state with an expanded tool result.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                "output",
                ToolResultStatus::Success,
            ));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();
        state
            .active_session_mut()
            .toggle_expand_entry(entry_id.clone());

        // When handling expand tool entry again.
        handle_expand_tool_entry(&mut state);

        // Then the entry is collapsed.
        assert!(!state.active_session().is_entry_expanded(&entry_id));
    }

    #[rstest::rstest]
    fn expand_annotation_entry_toggles_expanded_state() {
        // Given a state with a selected annotation entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::annotation(vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }]));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();

        // When handling expand tool entry.
        let _result = handle_expand_tool_entry(&mut state);

        // Then the entry is expanded.
        assert!(state.active_session().is_entry_expanded(&entry_id));
    }

    #[rstest::rstest]
    fn expand_annotation_entry_toggles_back_to_collapsed() {
        // Given a state with an expanded annotation entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::annotation(vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }]));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();
        state
            .active_session_mut()
            .toggle_expand_entry(entry_id.clone());

        // When handling expand tool entry again.
        handle_expand_tool_entry(&mut state);

        // Then the entry is collapsed.
        assert!(!state.active_session().is_entry_expanded(&entry_id));
    }

    #[rstest::rstest]
    fn expand_tool_entry_noop_with_no_selection() {
        // Given a state with entries but no selection.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                "output",
                ToolResultStatus::Success,
            ));

        // When handling expand tool entry.
        let result = handle_expand_tool_entry(&mut state);

        // Then no change.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn expand_tool_entry_noop_with_non_tool_entry() {
        // Given a state with a selected user entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling expand tool entry.
        let result = handle_expand_tool_entry(&mut state);

        // Then no change.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn expand_tool_entry_toggles_tool_call_expanded_state() {
        // Given a state with a selected tool call.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            "id",
            "bash",
            "{\"cmd\": true}",
        ));
        state.active_session_mut().select_next_entry();
        let entry_id = state.active_session().selected_entry_id().unwrap().clone();

        // When handling expand tool entry.
        let _result = handle_expand_tool_entry(&mut state);

        // Then the entry is expanded.
        assert!(state.active_session().is_entry_expanded(&entry_id));
    }

    #[rstest::rstest]
    fn expand_tool_entry_tool_call_returns_no_commands() {
        // Given a state with a selected tool call.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            "id",
            "bash",
            "{\"cmd\": true}",
        ));
        state.active_session_mut().select_next_entry();
        let _entry_id = state.active_session().selected_entry_id().unwrap().clone();

        // When handling expand tool entry.
        let result = handle_expand_tool_entry(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn fork_from_entry_returns_fork_command() {
        // Given a state with 3 entries, middle entry selected (index 1).
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("first"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("second"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("third"));
        // Select second entry (index 1).
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(1));

        // When handling fork from entry.
        let result = handle_fork_from_entry(&mut state);

        // Then a SessionForkRequested command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("SessionForkRequested"))
        );
    }

    #[rstest::rstest]
    fn fork_from_entry_noop_with_no_selection() {
        // Given a state with entries but no selection.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().clear_selection();

        // When handling fork from entry.
        let result = handle_fork_from_entry(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn fork_from_entry_noop_with_empty_history() {
        // Given a state with no history.
        let mut state = AppState::default_with_scope_focus();

        // When handling fork from entry.
        let result = handle_fork_from_entry(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn fork_from_entry_uses_selected_index_as_ordinal() {
        // Given a state with 5 entries, entry at index 3 selected.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("d"));
        state.active_session_mut().push_entry(ChatEntry::user("e"));
        // Select entry at index 3 ("d").
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(3));

        // When handling fork from entry.
        let result = handle_fork_from_entry(&mut state);

        // Then a SessionForkRequested command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("SessionForkRequested"))
        );
    }

    #[rstest::rstest]
    fn yank_selected_sets_yank_text_when_entry_selected() {
        // Given a state with a selected user entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling yank selected.
        let _result = handle_yank_selected(&mut state);

        // Then yank_text is set to the entry text.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text,
            Some("hello".to_owned())
        );
    }

    #[rstest::rstest]
    fn yank_selected_returns_no_commands() {
        // Given a state with a selected entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling yank selected.
        let result = handle_yank_selected(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn yank_selected_noop_with_no_selection() {
        // Given a state with no entries.
        let mut state = AppState::default_with_scope_focus();

        // When handling yank selected.
        let result = handle_yank_selected(&mut state);

        // Then yank_text is not set and no commands are emitted.
        assert!(state.frontend.signals_snapshot().yank_text.is_none());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn yank_selected_preserves_selection_index() {
        // Given a state with 2 entries, first selected.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling yank selected.
        let _result = handle_yank_selected(&mut state);

        // Then the selection index is unchanged.
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
    }

    #[rstest::rstest]
    fn yank_selected_extracts_assistant_text() {
        // Given a state with a selected assistant entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("response text"));
        state.active_session_mut().select_next_entry();

        // When handling yank selected.
        let _result = handle_yank_selected(&mut state);

        // Then yank_text contains the assistant text.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text,
            Some("response text".to_owned())
        );
    }

    #[rstest::rstest]
    fn yank_selected_extracts_tool_result_text() {
        // Given a state with a selected tool result.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                "output text",
                ToolResultStatus::Success,
            ));
        state.active_session_mut().select_next_entry();

        // When handling yank selected.
        let _result = handle_yank_selected(&mut state);

        // Then yank_text contains the raw content, without the name prefix.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text,
            Some("output text".to_owned())
        );
    }

    #[rstest::rstest]
    fn yank_selected_extracts_tool_call_arguments() {
        // Given a state with a selected tool call.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            "id",
            "bash",
            "{\"command\":\"ls\"}",
        ));
        state.active_session_mut().select_next_entry();

        // When handling yank selected.
        let _result = handle_yank_selected(&mut state);

        // Then yank_text contains the raw JSON arguments, without the name prefix.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text,
            Some("{\"command\":\"ls\"}".to_owned())
        );
    }

    #[rstest::rstest]
    fn toggle_ignored_block_noop_with_no_selection() {
        // Given a state with no selection.
        let mut state = AppState::default_with_scope_focus();

        // When handling toggle ignored block.
        let result = handle_toggle_ignored_block(&mut state);

        // Then no commands are emitted and no state change.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn toggle_ignored_block_noop_with_non_ignored_entry() {
        // Given a state with a selected non-ignored entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling toggle ignored block.
        let result = handle_toggle_ignored_block(&mut state);

        // Then no commands emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn toggle_ignored_block_expands_collapsed_block() {
        // Given a session with a collapsed ignored block selected.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        for _ in 0..15 {
            let mut entry = ChatEntry::user("ignored");
            entry.apply_context_override(
                crate::protocol::ContextOverride::ForcedExclude,
                ChangeSource::Internal {
                    label: "test".into(),
                },
            );
            state.active_session_mut().push_entry(entry);
        }
        state.active_session_mut().push_entry(ChatEntry::user("b"));

        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Find the collapsed block's visual-item index.
        let collapsed_vi_idx = items
            .iter()
            .position(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. }))
            .expect("should have a collapsed block");
        state
            .active_session_mut()
            .set_selected_entry_index(collapsed_vi_idx);

        let block_start_id = state.active_session().history()[1].id.clone();
        assert!(
            !state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block_start_id),
            "block should start collapsed"
        );

        // When handling toggle ignored block.
        let _result = handle_toggle_ignored_block(&mut state);

        // Then the block is expanded.
        assert!(
            state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block_start_id),
            "block should be shown after toggle on collapsed block"
        );
    }

    #[rstest::rstest]
    fn toggle_ignored_block_collapses_expanded_block() {
        // Given a session with an expanded ignored block, an ignored entry selected.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        for _ in 0..15 {
            let mut entry = ChatEntry::user("ignored");
            entry.apply_context_override(
                crate::protocol::ContextOverride::ForcedExclude,
                ChangeSource::Internal {
                    label: "test".into(),
                },
            );
            state.active_session_mut().push_entry(entry);
        }
        state.active_session_mut().push_entry(ChatEntry::user("b"));

        let block_start_id = state.active_session().history()[1].id.clone();

        // Expand the block first.
        let entry_5_id = state.active_session().history()[5].id.clone();
        state
            .active_session_mut()
            .toggle_ignored_block_visibility(&entry_5_id);
        assert!(
            state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block_start_id),
            "block should be expanded"
        );

        // Rebuild visual items (now expanded - individual Entry items).
        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Select an ignored entry within the expanded block (history index 5).
        let target_vi_idx = items
            .iter()
            .position(|i| {
                matches!(i, crate::feat::ui::chat_log::visual_item::VisualItem::Entry(hist_idx) if *hist_idx == 5)
            })
            .expect("should find ignored entry at history index 5");
        state
            .active_session_mut()
            .set_selected_entry_index(target_vi_idx);

        // When handling toggle ignored block.
        let _result = handle_toggle_ignored_block(&mut state);

        // Then the block is collapsed.
        assert!(
            !state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block_start_id),
            "block should be collapsed after toggle on ignored entry"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_false_to_true() {
        // Given a state with a selected user entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is now ignored.
        let selected = state.active_session().selected_entry().expect("entry");
        assert!(selected.ignored(), "entry should be ignored after toggle");
        // And a PersistSession command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PersistSession")),
            "should contain PersistSession command"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_emits_context_override_changed() {
        // Given a state with a selected user entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();
        let _session_id = state.active_session().session_id().clone();
        let _entry_id = state
            .active_session()
            .selected_entry()
            .expect("entry")
            .id
            .clone();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then ContextOverrideChanged event is emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("ContextOverrideChanged")),
            "should emit ContextOverrideChanged event"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_true_to_false() {
        // Given a state with a selected ignored user entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello").with_ignored(true));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let _result = handle_ignore_selected(&mut state);

        // Then the entry is now un-ignored.
        let selected = state.active_session().selected_entry().expect("entry");
        assert!(
            !selected.ignored(),
            "entry should be un-ignored after toggle"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_noop_empty_history() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then no commands or events are emitted.
        assert!(
            result.message_names.is_empty(),
            "empty history should produce no commands"
        );
        assert!(
            result.message_names.is_empty(),
            "empty history should produce no events"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_noop_no_selection() {
        // Given a session with entries but no selection.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().clear_selection();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then no commands are emitted.
        assert!(
            result.message_names.is_empty(),
            "no selection should produce no commands"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_noop_pinned_entry() {
        // Given a state with a selected pinned entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello").with_pin(PinPosition::Top));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then no commands are emitted and ignored is unchanged.
        assert!(
            result.message_names.is_empty(),
            "pinned entry should produce no commands"
        );
        let selected = state.active_session().selected_entry().expect("entry");
        assert!(
            !selected.ignored(),
            "pinned entry ignored should stay false"
        );
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_system_entry() {
        // Given a state with a selected system entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::system("system prompt"));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is toggled (System is excluded by default, so toggle → ForcedInclude).
        assert!(
            !result.message_names.is_empty(),
            "system entry toggle should produce commands"
        );
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::ForcedInclude);
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_thinking_entry() {
        // Given a state with a selected thinking entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::thinking("thinking..."));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is toggled (Thinking is excluded by default, so toggle → ForcedInclude).
        assert!(
            !result.message_names.is_empty(),
            "thinking entry toggle should produce commands"
        );
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::ForcedInclude);
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_transient_entry() {
        // Given a state with a selected transient entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::transient("ephemeral"));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is toggled (Transient is excluded by default, so toggle → ForcedInclude).
        assert!(
            !result.message_names.is_empty(),
            "transient entry toggle should produce commands"
        );
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::ForcedInclude);
    }

    #[rstest::rstest]
    fn handle_ignore_selected_toggles_compaction_entry() {
        // Given a state with a selected compaction entry.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry {
            id: crate::protocol::ChatEntryId::new(),
            timing: crate::protocol::EntryTiming::instant_now(),
            kind: crate::protocol::ChatEntryKind::Compaction {
                summary: "summary".to_owned(),
                tokens_before: 100,
                tokens_after: 50,
                entries_compacted: 5,
                model_used: "test/model".to_owned(),
            },
            pin_position: None,
            context_override: crate::protocol::ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        });
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is toggled (Compaction is included by default, so toggle → ForcedExclude).
        assert!(
            !result.message_names.is_empty(),
            "compaction entry toggle should produce commands"
        );
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::ForcedExclude);
    }

    #[rstest::rstest]
    fn handle_reset_sets_forced_exclude_back_to_default() {
        // Given a selected entry that is ForcedExclude.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(
            ChatEntry::user("hello").with_context_override(ContextOverride::ForcedExclude),
        );
        state.active_session_mut().select_next_entry();

        // When handling reset selected.
        let _result = handle_reset_selected(&mut state);

        // Then the entry's override is Default.
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::Default);
    }

    #[rstest::rstest]
    fn handle_reset_sets_forced_include_back_to_default() {
        // Given a selected entry that is ForcedInclude.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(
            ChatEntry::system("sys").with_context_override(ContextOverride::ForcedInclude),
        );
        state.active_session_mut().select_next_entry();

        // When handling reset selected.
        let _result = handle_reset_selected(&mut state);

        // Then the entry's override is Default.
        let selected = state.active_session().selected_entry().expect("entry");
        assert_eq!(selected.context_override(), ContextOverride::Default);
    }

    #[rstest::rstest]
    fn handle_reset_emits_events_when_override_changes() {
        // Given a selected entry that is ForcedExclude.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(
            ChatEntry::user("hello").with_context_override(ContextOverride::ForcedExclude),
        );
        state.active_session_mut().select_next_entry();

        // When handling reset selected.
        let result = handle_reset_selected(&mut state);

        // Then persist and override events are emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PersistSession"))
        );
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("ContextOverrideChanged"))
        );
    }

    #[rstest::rstest]
    fn handle_reset_is_noop_on_already_default_entry() {
        // Given a selected entry that is already Default.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().select_next_entry();

        // When handling reset selected.
        let result = handle_reset_selected(&mut state);

        // Then no events are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn handle_reset_advances_cursor() {
        // Given two entries with the first selected.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a").with_context_override(ContextOverride::ForcedExclude));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling reset selected.
        let _result = handle_reset_selected(&mut state);

        // Then the cursor has advanced to entry 1.
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
    }

    #[rstest::rstest]
    fn handle_reset_is_noop_on_pinned_entry() {
        // Given a selected pinned entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello").with_pin(PinPosition::Top));
        state.active_session_mut().select_next_entry();

        // When handling reset selected.
        let result = handle_reset_selected(&mut state);

        // Then no events are emitted.
        assert!(result.message_names.is_empty());
    }
    #[rstest::rstest]
    fn handle_reset_sweep_skips_pinned_entry() {
        // Given [user(ForcedExclude), user_pinned, user(ForcedExclude)],
        // entry 0 selected.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a").with_context_override(ContextOverride::ForcedExclude));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("pinned").with_pin(PinPosition::Top));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("b").with_context_override(ContextOverride::ForcedExclude));
        state.active_session_mut().select_prev_entry();
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When pressing `r` three times (reset 0, skip pinned, reset 2).
        handle_reset_selected(&mut state); // entry 0 -> Default, cursor -> 1
        let result = handle_reset_selected(&mut state); // pinned: skip, cursor -> 2
        handle_reset_selected(&mut state); // entry 2 -> Default, cursor -> bottom

        // Then entries 0 and 2 are reset to Default.
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::Default
        );
        assert_eq!(
            state.active_session().history()[2].context_override(),
            ContextOverride::Default
        );
        // And the middle press (the skip) emits no events.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn handle_reset_sweep_resets_each_entry_via_cursor_advance() {
        // Given two ForcedExclude entries with the first selected.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a").with_context_override(ContextOverride::ForcedExclude));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("b").with_context_override(ContextOverride::ForcedExclude));
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling reset twice (simulating hold-to-sweep).
        handle_reset_selected(&mut state);
        handle_reset_selected(&mut state);

        // Then both entries are now Default.
        let history = state.active_session().history();
        assert_eq!(history[0].context_override(), ContextOverride::Default);
        assert_eq!(history[1].context_override(), ContextOverride::Default);
    }

    #[rstest::rstest]
    fn sweep_first_press_toggles_and_captures_state() {
        // Given a session with 3 user entries, first selected.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        // Select first entry.
        state.active_session_mut().select_prev_entry();
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling ignore selected (first press).
        let result = handle_ignore_selected(&mut state);

        // Then entry 0 is toggled to ForcedExclude.
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );
        // And the cursor has advanced to entry 1.
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
        // And sweep state is stored with ForcedExclude target.
        let sweep = state.active_session_mut().take_ignore_sweep();
        assert_eq!(sweep, Some(ContextOverride::ForcedExclude));
        // And a PersistSession command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PersistSession")),
        );
    }

    #[rstest::rstest]
    fn sweep_second_press_applies_captured_state() {
        // Given a session with 3 user entries, entry 0 already toggled to
        // ForcedExclude, cursor at entry 1, sweep active with ForcedExclude.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        state.active_session_mut().select_prev_entry();
        state.active_session_mut().select_prev_entry();

        // First press - toggles entry 0, advances to 1.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(state.active_session().selected_entry_index(), Some(1));

        // When handling ignore selected again (second press, within 100ms).
        let _result = handle_ignore_selected(&mut state);

        // Then entry 1 is now ForcedExclude (applied, not toggled).
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::ForcedExclude
        );
        // And cursor has advanced to entry 2.
        assert_eq!(state.active_session().selected_entry_index(), Some(2));
        // And sweep state is still active.
        assert!(state.active_session_mut().take_ignore_sweep().is_some());
    }

    #[rstest::rstest]
    fn sweep_third_press_continues_to_bottom() {
        // Given a session with 3 user entries, sweep already applied to 0 and 1,
        // cursor at entry 2.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        state.active_session_mut().select_prev_entry();
        state.active_session_mut().select_prev_entry();

        // First press.
        let _result = handle_ignore_selected(&mut state);
        // Second press.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(state.active_session().selected_entry_index(), Some(2));

        // When handling ignore selected (third press).
        let result = handle_ignore_selected(&mut state);

        // Then entry 2 is ForcedExclude.
        assert_eq!(
            state.active_session().history()[2].context_override(),
            ContextOverride::ForcedExclude
        );
        // And cursor stays at entry 2 (at bottom, can't advance further).
        assert_eq!(state.active_session().selected_entry_index(), Some(2));
        // And commands are still returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PersistSession")),
        );
    }

    #[rstest::rstest]
    fn sweep_stops_at_bottom() {
        // Given a single entry session.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("only"));
        state.active_session_mut().select_next_entry();

        // When handling ignore selected.
        let result = handle_ignore_selected(&mut state);

        // Then the entry is toggled.
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );
        // And cursor stays at 0 (at bottom).
        assert_eq!(state.active_session().selected_entry_index(), Some(0));
        // And sweep state is stored (ready if more entries appear).
        assert!(state.active_session_mut().take_ignore_sweep().is_some());
        // And commands are returned.
        assert!(!result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sweep_expired_resets_to_toggle() {
        // Given a session with 2 user entries, sweep was started but expired.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().select_prev_entry();

        // First press - toggles entry 0, advances to 1.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(state.active_session().selected_entry_index(), Some(1));

        // Expire the sweep by setting a stale timestamp.
        state.active_session_mut().set_ignore_sweep_at(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(200))
                .unwrap(),
            ContextOverride::ForcedExclude,
        );

        // When handling ignore selected again (after timeout).
        let _result = handle_ignore_selected(&mut state);

        // Then entry 1 is toggled (fresh toggle: Default → ForcedExclude).
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::ForcedExclude
        );
        // And sweep state is refreshed with a new timestamp.
        let sweep = state.active_session_mut().take_ignore_sweep_raw();
        assert!(sweep.is_some(), "sweep state should be re-stored");
        let (instant, target) = sweep.unwrap();
        assert!(instant.elapsed() < std::time::Duration::from_millis(10));
        assert_eq!(target, ContextOverride::ForcedExclude);
    }

    #[rstest::rstest]
    fn sweep_skips_pinned_entries() {
        // Given entries [user, user_pinned, user], entry 0 selected.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        let pinned_entry = ChatEntry::user("pinned").with_pin(PinPosition::Top);
        state.active_session_mut().push_entry(pinned_entry);
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        state.active_session_mut().select_prev_entry();
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // First press - toggles entry 0, advances to entry 1 (pinned).
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );

        // When handling second press (cursor on pinned entry 1).
        let _result = handle_ignore_selected(&mut state);

        // Then pinned entry 1 is unchanged (still Default).
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::Default
        );
        // And entry 2 (after pinned) gets ForcedExclude.
        assert_eq!(
            state.active_session().history()[2].context_override(),
            ContextOverride::ForcedExclude
        );
        // And cursor has advanced past the pinned entry to entry 2 (or beyond).
        assert_eq!(state.active_session().selected_entry_index(), Some(2));
    }

    #[rstest::rstest]
    fn sweep_skips_pinned_at_bottom_stops() {
        // Given entries [user, pinned], entry 0 selected.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("pinned").with_pin(PinPosition::Top));
        state.active_session_mut().select_prev_entry();

        // First press - toggles entry 0, advances to entry 1 (pinned).
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(state.active_session().selected_entry_index(), Some(1));

        // When handling second press (cursor on pinned entry 1, at bottom).
        let result = handle_ignore_selected(&mut state);

        // Then pinned entry is unchanged.
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::Default
        );
        // And no commands returned (no-op: pinned at bottom).
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn sweep_continues_with_forced_include_target() {
        // Given a session with 2 user entries, entry 0 already ForcedExclude.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a").with_ignored(true));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        state.active_session_mut().select_prev_entry();
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling ignore selected (toggle from ForcedExclude → ForcedInclude).
        let _result = handle_ignore_selected(&mut state);

        // Then entry 0 is now ForcedInclude (brought into context).
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedInclude
        );
        // And cursor has advanced.
        assert_eq!(state.active_session().selected_entry_index(), Some(1));
        // And sweep state IS stored with ForcedInclude target (the toggle never yields Default).
        let sweep = state.active_session_mut().take_ignore_sweep();
        assert_eq!(sweep, Some(ContextOverride::ForcedInclude));
    }

    #[rstest::rstest]
    fn sweep_skips_collapsed_block_without_mutating() {
        // Given: 1 user, 10 ignored (will collapse), 5 user.
        // The 10 ignored entries form a collapsed block.
        // The sweep should skip the collapsed block entirely — no expansion,
        // no mutation of entries inside.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("before"));
        for _ in 0..10 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("ignored").with_ignored(true));
        }
        for _ in 0..5 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("after"));
        }

        // Build visual items so the collapsed block exists.
        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Verify we have a collapsed block.
        let collapsed = items
            .iter()
            .find(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. }));
        assert!(collapsed.is_some(), "should have a collapsed block");

        // Select first user entry.
        let first_vi = items
            .iter()
            .position(|i| matches!(i, VisualItem::Entry(0)))
            .expect("first entry");
        state
            .active_session_mut()
            .set_selected_entry_index(first_vi);

        // First press - toggles entry 0 to ForcedExclude, advances.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );

        // Second press - should skip the collapsed block and land on
        // an entry after it.
        let _result = handle_ignore_selected(&mut state);

        // Entries inside the collapsed block should NOT be mutated
        // (they were already ForcedExclude, no change applied).
        // Just verify the block was NOT expanded.
        let block_start_id = state.active_session().history()[1].id.clone();
        assert!(
            !state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block_start_id),
            "collapsed block should NOT be expanded during sweep"
        );

        // The cursor should have jumped past the collapsed block
        // to one of the 'after' entries.
        let selected = state.active_session().selected_entry();
        assert!(
            selected.is_some(),
            "cursor should be on an entry after the collapsed block"
        );
        let selected_text = selected.expect("entry").text();
        assert!(
            selected_text.starts_with("after"),
            "cursor should be on an 'after' entry, got: {selected_text:?}"
        );
    }

    #[rstest::rstest]
    fn sweep_unignore_propagates_shown_to_new_block() {
        // Given: 1 user, 15 ignored in a shown (expanded) block, 5 user.
        // Sweep un-ignore will bring entries into context, splitting the block.
        // The new forward sub-block should auto-expand.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("before"));
        for _ in 0..15 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("ignored").with_ignored(true));
        }
        for _ in 0..5 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("after"));
        }

        // Show (expand) the block first.
        let block_start_id = state.active_session().history()[1].id.clone();
        state
            .active_session_mut()
            .show_ignored_block(block_start_id);

        // Build visual items (now expanded - individual entries).
        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Select entry at history index 1 (first ignored entry).
        let vi_idx = items
            .iter()
            .position(|i| {
                matches!(
                    i,
                    crate::feat::ui::chat_log::visual_item::VisualItem::Entry(1)
                )
            })
            .expect("entry at history index 1");
        state.active_session_mut().set_selected_entry_index(vi_idx);

        // First press - toggles entry 1 from ForcedExclude → ForcedInclude.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::ForcedInclude
        );
        // Sweep state is ForcedInclude (un-ignore sweep target).
        assert_eq!(
            state.active_session_mut().take_ignore_sweep(),
            Some(ContextOverride::ForcedInclude)
        );
        // Restore sweep since we consumed it.
        state
            .active_session_mut()
            .set_ignore_sweep(ContextOverride::ForcedInclude);
        // Propagation should have shown the forward sub-block.
        // Entry 2 is the start of the forward excluded sub-block.
        let forward_block_id = state.active_session().history()[2].id.clone();
        assert!(
            state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&forward_block_id),
            "forward sub-block should be auto-shown after un-ignore split"
        );
    }

    #[rstest::rstest]
    fn sweep_continues_past_collapsed_block_to_entries_beyond() {
        // Given: 1 user (in-context), 10 ignored (collapsed block), 3 user (in-context).
        // Sweep starts on the first user entry, should continue through the
        // collapsed block and reach the user entries after it.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("before"));
        for _ in 0..10 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("ignored").with_ignored(true));
        }
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("after1"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("after2"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("after3"));

        // Build visual items.
        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Verify layout: Entry(0), CollapsedIgnoredBlock(1, 10), Entry(11), Entry(12), Entry(13).
        let collapsed = items
            .iter()
            .find(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. }));
        assert!(collapsed.is_some(), "should have a collapsed block");

        // Select first user entry ("before").
        let first_vi = items
            .iter()
            .position(|i| matches!(i, VisualItem::Entry(0)))
            .expect("first entry");
        state
            .active_session_mut()
            .set_selected_entry_index(first_vi);

        // First press - toggles "before" to ForcedExclude, advances to collapsed block.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );

        // Second press - should expand collapsed block, apply override, advance past it.
        let _result = handle_ignore_selected(&mut state);

        // The sweep should have continued — cursor should now be on an entry
        // after the collapsed block (not stuck on it).
        let selected_idx = state.active_session().selected_entry_index();
        assert!(
            selected_idx.is_some(),
            "cursor should have a selection after sweep through block"
        );

        // The selected entry should be one of the "after" entries (history index 11+).
        let selected_entry = state.active_session().selected_entry().expect("entry");
        assert!(
            selected_entry.text().starts_with("after"),
            "cursor should be on an 'after' entry, got: {:?}",
            selected_entry.text()
        );

        // Third press - should continue sweeping the "after" entries.
        let _result = handle_ignore_selected(&mut state);
        // Verify one of the after entries got ForcedExclude.
        let any_after_excluded = state
            .active_session()
            .history()
            .iter()
            .skip(11)
            .any(|e| e.context_override() == ContextOverride::ForcedExclude);
        assert!(
            any_after_excluded,
            "at least one 'after' entry should be ForcedExclude"
        );
    }

    #[rstest::rstest]
    fn sweep_skips_multiple_collapsed_blocks() {
        // Given: 2 user, 10 ignored (block 1), 2 user, 10 ignored (block 2), 5 user.
        // Sweep starts on first user, skips collapsed blocks, processes in-between entries.
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
        };

        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state.active_session_mut().push_entry(ChatEntry::user("b"));
        for _ in 0..10 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("ignored1").with_ignored(true));
        }
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        state.active_session_mut().push_entry(ChatEntry::user("d"));
        for _ in 0..10 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("ignored2").with_ignored(true));
        }
        for _ in 0..5 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("after"));
        }

        // Build visual items.
        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session_mut().set_visual_items(items.clone());

        // Verify two collapsed blocks exist.
        let collapsed_count = items
            .iter()
            .filter(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. }))
            .count();
        assert_eq!(collapsed_count, 2, "should have two collapsed blocks");

        // The ignored entries inside the blocks should NOT be mutated.
        let block1_start_id = state.active_session().history()[2].id.clone();

        // Select first entry.
        let first_vi = items
            .iter()
            .position(|i| matches!(i, VisualItem::Entry(0)))
            .expect("first entry");
        state
            .active_session_mut()
            .set_selected_entry_index(first_vi);

        // First press - toggles entry "a" to ForcedExclude.
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(
            state.active_session().history()[0].context_override(),
            ContextOverride::ForcedExclude
        );

        // Second press - cursor is on "b" (the entry after "a").
        // Applying ForcedExclude to "b" merges entries 0-1 with the pre-existing
        // ignored block (2-11) into a single collapsed block, so the cursor
        // lands on the merged block and is advanced past it to "c".
        // Only "b" is adjusted this press (1 keypress = 1 entry).
        let _result = handle_ignore_selected(&mut state);
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::ForcedExclude,
            "entry 'b' should be ForcedExclude after second press"
        );
        // Entry "c" must NOT be excluded yet (it is only reached on press 3).
        assert_eq!(
            state.active_session().history()[12].context_override(),
            ContextOverride::Default,
            "entry 'c' should still be Default after second press (no chain)"
        );

        // Third press - cursor on "c", applies ForcedExclude.
        let _result = handle_ignore_selected(&mut state);

        // Entry "b" (index 1) should be ForcedExclude.
        assert_eq!(
            state.active_session().history()[1].context_override(),
            ContextOverride::ForcedExclude,
            "entry 'b' should be ForcedExclude"
        );

        // Entry "c" (index 12) should be processed on press 3.
        assert_eq!(
            state.active_session().history()[12].context_override(),
            ContextOverride::ForcedExclude,
            "entry 'c' should be ForcedExclude after third press"
        );

        // Guard against over-chaining: "d" (index 13) must still be Default.
        // Before the fix, press 2 chained through "c", "d", and beyond.
        assert_eq!(
            state.active_session().history()[13].context_override(),
            ContextOverride::Default,
            "entry 'd' should still be Default (no over-chain)"
        );

        // The block entries (2..=11) should NOT be mutated (skipped).
        for i in 2..=11 {
            assert_eq!(
                state.active_session().history()[i].context_override(),
                ContextOverride::ForcedExclude,
                "ignored entry {i} should still be ForcedExclude (untouched by sweep)"
            );
        }

        // The block should NOT be expanded.
        assert!(
            !state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&block1_start_id),
            "first collapsed block should not be expanded"
        );
    }

    /// Helper: push `count` in-context user entries and select the first.
    fn build_in_context_history(state: &mut AppState, count: usize) {
        for n in 0..count {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("entry-{n}")));
        }
        // Default selection is the last entry; walk back to index 0.
        for _ in 0..count.saturating_sub(1) {
            state.active_session_mut().select_prev_entry();
        }
    }

    /// Helper: count entries currently set to `ForcedExclude`.
    fn count_excluded(state: &AppState) -> usize {
        state
            .active_session()
            .history()
            .iter()
            .filter(|e| e.context_override() == ContextOverride::ForcedExclude)
            .count()
    }

    #[rstest::rstest]
    fn sweep_stops_one_entry_per_press_even_when_collapse_forms() {
        // Given 10 in-context user entries with the cursor on the first.
        // 10 entries is enough that a mid-press collapse forms (entries 0..2
        // excluded -> collapse, well before the proximity-protected tail of
        // the last 3).
        let mut state = AppState::default_with_scope_focus();
        build_in_context_history(&mut state, 10);
        assert_eq!(state.active_session().selected_entry_index(), Some(0));

        // When handling ignore selected three times (three presses within
        // the 100ms memory window).
        let _r1 = handle_ignore_selected(&mut state); // entry 0
        let _r2 = handle_ignore_selected(&mut state); // entry 1 (may collapse 0-1)
        let _r3 = handle_ignore_selected(&mut state); // entry 2 (collapse now forms)

        // Then exactly 3 entries are ForcedExclude. Before the fix, press 3
        // chained to the proximity tail, excluding ~7 entries (indices 0..6).
        assert_eq!(
            count_excluded(&state),
            3,
            "three presses must exclude exactly three entries (no chaining)"
        );
        // And entries 0..3 are the excluded ones.
        for i in 0..3 {
            assert_eq!(
                state.active_session().history()[i].context_override(),
                ContextOverride::ForcedExclude,
                "entry {i} should be ForcedExclude"
            );
        }
        // And entry 3 is untouched (guard: the chain did not overshoot).
        assert_eq!(
            state.active_session().history()[3].context_override(),
            ContextOverride::Default,
            "entry 3 must remain Default (no over-chain)"
        );
        // And the sweep memory is still armed for the next press.
        assert_eq!(
            state.active_session_mut().take_ignore_sweep(),
            Some(ContextOverride::ForcedExclude),
            "memory must carry the ForcedExclude direction past a collapse"
        );
    }

    #[rstest::rstest]
    fn sweep_advances_exactly_one_entry_across_multiple_presses() {
        // Given 10 in-context user entries with the cursor on the first.
        let mut state = AppState::default_with_scope_focus();
        build_in_context_history(&mut state, 10);

        // When handling ignore selected five times (five presses within the
        // 100ms memory window). Each press must adjust exactly one entry.
        let mut total_override_events = 0;
        for _ in 0..5 {
            let result = handle_ignore_selected(&mut state);
            total_override_events += result
                .message_names
                .iter()
                .filter(|n| n.contains("ContextOverrideChanged"))
                .count();
        }

        // Then exactly 5 entries are ForcedExclude. Before the fix, the 3rd
        // press chained to the proximity tail, excluding ~7 in one go.
        assert_eq!(
            count_excluded(&state),
            5,
            "five presses must exclude exactly five entries"
        );
        // And exactly one ContextOverrideChanged event was emitted per press.
        assert_eq!(
            total_override_events, 5,
            "each press must emit exactly one ContextOverrideChanged event"
        );
        // And entry 5 is untouched (guard against over-chaining).
        assert_eq!(
            state.active_session().history()[5].context_override(),
            ContextOverride::Default,
            "entry 5 must remain Default (no over-chain)"
        );
    }

    #[rstest::rstest]
    fn sweep_does_not_chain_to_bottom_in_large_history() {
        // Given 50 in-context user entries with the cursor on the first.
        // A large history is where the bug was most visible: press 3 used to
        // chain ~47 entries to the proximity tail in a single keypress.
        let mut state = AppState::default_with_scope_focus();
        build_in_context_history(&mut state, 50);

        // When handling ignore selected twice.
        handle_ignore_selected(&mut state); // entry 0
        handle_ignore_selected(&mut state); // entry 1
        // Then exactly 2 entries are excluded.
        assert_eq!(
            count_excluded(&state),
            2,
            "two presses must exclude exactly two entries"
        );

        // When handling ignore selected a third time (the press that forms
        // a real collapse and would previously chain to the bottom).
        handle_ignore_selected(&mut state); // entry 2

        // Then exactly 3 entries are excluded — NOT ~47. This is the core
        // regression assertion: the sweep must not reach the proximity tail.
        assert_eq!(
            count_excluded(&state),
            3,
            "third press must exclude exactly one more entry, not chain to bottom"
        );
        // And the cursor sits on a real entry near the top, not at the end.
        let selected = state.active_session().selected_entry().expect("entry");
        assert!(
            selected.text().starts_with("entry-3"),
            "cursor should be on entry-3, got: {:?}",
            selected.text()
        );
    }

    #[rstest::rstest]
    fn sweep_memory_persists_after_collapse_forms() {
        // Given 10 in-context user entries with the cursor on the first, with
        // two entries already excluded so the next press forms a collapse.
        let mut state = AppState::default_with_scope_focus();
        build_in_context_history(&mut state, 10);
        handle_ignore_selected(&mut state); // entry 0 -> ForcedExclude
        handle_ignore_selected(&mut state); // entry 1 -> ForcedExclude

        // When handling ignore selected a third time (forms a collapse).
        handle_ignore_selected(&mut state); // entry 2 -> ForcedExclude + collapse

        // Then the sweep memory is still armed with the captured direction,
        // so the next press continues in ForcedExclude rather than re-toggling.
        assert_eq!(
            state.active_session_mut().take_ignore_sweep(),
            Some(ContextOverride::ForcedExclude),
            "memory must persist after a press that forms a collapse"
        );
    }
}

/// Tests for `]c` / `[c` compaction-jump handlers.
#[cfg(test)]
mod jump_compaction_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use crate::common::app_state::AppState;
    use crate::feat::session::chat_entry::{ChatEntry, ChatEntryId, ChatEntryKind};
    use crate::protocol::ContextOverride;
    use crate::protocol::EntryTiming;

    use super::*;

    /// Build a compaction summary entry with a distinctive summary.
    fn compaction_entry(summary: &str) -> ChatEntry {
        ChatEntry {
            id: ChatEntryId::new(),
            timing: EntryTiming::instant_now(),
            kind: ChatEntryKind::Compaction {
                summary: summary.to_owned(),
                tokens_before: 100,
                tokens_after: 50,
                entries_compacted: 5,
                model_used: "test/model".to_owned(),
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Canonical history: user(0), compaction-A(1), user(2), compaction-B(3).
    /// Returns the IDs of compaction A (older) and B (newer).
    fn build_two_compaction_history(state: &mut AppState) -> (ChatEntryId, ChatEntryId) {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("first"));
        state.active_session_mut().push_entry(compaction_entry("A"));
        let a_id = state.active_session().history()[1].id.clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("middle"));
        state.active_session_mut().push_entry(compaction_entry("B"));
        let b_id = state.active_session().history()[3].id.clone();
        (a_id, b_id)
    }

    /// Select the entry at a history index by cloning its ID into the cursor.
    fn select_at(state: &mut AppState, hist_idx: usize) {
        let id = state.active_session().history()[hist_idx].id.clone();
        state.active_session_mut().set_selected_cursor_id(id);
    }

    #[rstest::rstest]
    fn jump_next_moves_to_next_compaction() {
        // Given history user,A,user,B with the cursor on compaction A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_compaction_history(&mut state);
        select_at(&mut state, 1);
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));

        // When handling jump to next compaction.
        let _result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor moves to compaction B.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
    }

    #[rstest::rstest]
    fn jump_prev_moves_to_prev_compaction() {
        // Given history user,A,user,B with the cursor on compaction B.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_compaction_history(&mut state);
        select_at(&mut state, 3);
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));

        // When handling jump to previous compaction.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor moves to compaction A.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
    }

    #[rstest::rstest]
    fn jump_next_noop_at_last_compaction() {
        // Given the cursor on the last compaction B.
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id) = build_two_compaction_history(&mut state);
        select_at(&mut state, 3);

        // When handling jump to next compaction.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_at_first_compaction() {
        // Given the cursor on the first compaction A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, _b_id) = build_two_compaction_history(&mut state);
        select_at(&mut state, 1);

        // When handling jump to previous compaction.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_next_noop_when_no_selection() {
        // Given the canonical history with no active selection.
        let mut state = AppState::default_with_scope_focus();
        build_two_compaction_history(&mut state);
        state.active_session_mut().clear_selection();
        assert!(state.active_session().selected_cursor_id().is_none());

        // When handling jump to next compaction (anchor = last entry).
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then it is a no-op: nothing newer than the last entry exists.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_anchors_on_last_entry_when_no_selection() {
        // Given the canonical history with no active selection.
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id) = build_two_compaction_history(&mut state);
        state.active_session_mut().clear_selection();
        assert!(state.active_session().selected_cursor_id().is_none());

        // When handling jump to previous compaction.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the anchor is the last entry, so [c lands on compaction B.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
    }

    #[rstest::rstest]
    fn jump_next_noop_when_no_compactions() {
        // Given a history with no compactions.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        select_at(&mut state, 0);
        let before = state.active_session().selected_cursor_id();

        // When handling jump to next compaction.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then it is a no-op.
        assert_eq!(state.active_session().selected_cursor_id(), before);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_when_no_compactions() {
        // Given a history with no compactions.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        select_at(&mut state, 1);
        let before = state.active_session().selected_cursor_id();

        // When handling jump to previous compaction.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then it is a no-op.
        assert_eq!(state.active_session().selected_cursor_id(), before);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_next_noop_when_history_empty() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling jump to next compaction.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then it is a no-op without panic.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_when_history_empty() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling jump to previous compaction.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then it is a no-op without panic.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    /// Build history user,A,user,B where A and B are pinned entries.
    fn build_two_pinned_history(state: &mut AppState) -> (ChatEntryId, ChatEntryId) {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("first"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("A").with_pin(PinPosition::Top));
        let a_id = state.active_session().history()[1].id.clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("middle"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("B").with_pin(PinPosition::Top));
        let b_id = state.active_session().history()[3].id.clone();
        (a_id, b_id)
    }

    #[rstest::rstest]
    fn jump_next_moves_to_next_pinned() {
        // Given history user,A,user,B with the cursor on pinned entry A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_pinned_history(&mut state);
        select_at(&mut state, 1);
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));

        // When handling jump to next pinned entry.
        let _result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then the cursor moves to pinned entry B.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
    }

    #[rstest::rstest]
    fn jump_prev_moves_to_prev_pinned() {
        // Given history user,A,user,B with the cursor on pinned entry B.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_pinned_history(&mut state);
        select_at(&mut state, 3);
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));

        // When handling jump to previous pinned entry.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then the cursor moves to pinned entry A.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
    }

    #[rstest::rstest]
    fn jump_next_noop_at_last_pinned() {
        // Given the cursor on the last pinned entry B.
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id) = build_two_pinned_history(&mut state);
        select_at(&mut state, 3);

        // When handling jump to next pinned entry.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_at_first_pinned() {
        // Given the cursor on the first pinned entry A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, _b_id) = build_two_pinned_history(&mut state);
        select_at(&mut state, 1);

        // When handling jump to previous pinned entry.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_next_noop_when_no_pinned() {
        // Given a history with no pinned entries.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        select_at(&mut state, 0);
        let before = state.active_session().selected_cursor_id();

        // When handling jump to next pinned entry.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then it is a no-op.
        assert_eq!(state.active_session().selected_cursor_id(), before);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_when_no_pinned() {
        // Given a history with no pinned entries.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        select_at(&mut state, 1);
        let before = state.active_session().selected_cursor_id();

        // When handling jump to previous pinned entry.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then it is a no-op.
        assert_eq!(state.active_session().selected_cursor_id(), before);
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_anchors_on_last_when_no_selection() {
        // Given the canonical history with no active selection.
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id) = build_two_pinned_history(&mut state);
        state.active_session_mut().clear_selection();
        assert!(state.active_session().selected_cursor_id().is_none());

        // When handling jump to previous pinned entry.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then the anchor is the last entry, so [p lands on pinned entry B.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
    }

    #[rstest::rstest]
    fn jump_next_noop_when_history_empty_pinned() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling jump to next pinned entry.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_pinned,
        );

        // Then it is a no-op without panic.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    /// Build history user,A,user,B where A and B are Sources (annotation) entries.
    fn build_two_annotation_history(state: &mut AppState) -> (ChatEntryId, ChatEntryId) {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("first"));
        let a_id = {
            let citation = jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            };
            state
                .active_session_mut()
                .push_entry(ChatEntry::annotation(vec![citation]));
            state.active_session().history()[1].id.clone()
        };
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("middle"));
        let b_id = {
            let citation = jinn_provider::UrlCitation {
                url: "https://example.com/b".to_owned(),
                title: "Source B".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            };
            state
                .active_session_mut()
                .push_entry(ChatEntry::annotation(vec![citation]));
            state.active_session().history()[3].id.clone()
        };
        (a_id, b_id)
    }

    #[rstest::rstest]
    fn jump_next_moves_to_next_annotation() {
        // Given history user,A,user,B with the cursor on annotation entry A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_annotation_history(&mut state);
        select_at(&mut state, 1);
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));

        // When handling jump to next annotation entry.
        let _result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then the cursor moves to annotation entry B.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
    }

    #[rstest::rstest]
    fn jump_prev_moves_to_prev_annotation() {
        // Given history user,A,user,B with the cursor on annotation entry B.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id) = build_two_annotation_history(&mut state);
        select_at(&mut state, 3);
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));

        // When handling jump to previous annotation entry.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then the cursor moves to annotation entry A.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
    }

    #[rstest::rstest]
    fn jump_next_noop_at_last_annotation() {
        // Given the cursor on the last annotation entry B.
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id) = build_two_annotation_history(&mut state);
        select_at(&mut state, 3);

        // When handling jump to next annotation entry.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(b_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_at_first_annotation() {
        // Given the cursor on the first annotation entry A.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, _b_id) = build_two_annotation_history(&mut state);
        select_at(&mut state, 1);

        // When handling jump to previous annotation entry.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then the cursor is unchanged (no wrap) and no commands emitted.
        assert_eq!(state.active_session().selected_cursor_id(), Some(a_id));
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_next_noop_when_history_empty_annotation() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling jump to next annotation entry.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then it is a no-op without panic.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_noop_when_history_empty_annotation() {
        // Given an empty session.
        let mut state = AppState::default_with_scope_focus();

        // When handling jump to previous annotation entry.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_annotation,
        );

        // Then it is a no-op without panic.
        assert!(state.active_session().selected_cursor_id().is_none());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_next_returns_no_commands() {
        // Given history with a compaction, cursor on compaction A.
        let mut state = AppState::default_with_scope_focus();
        build_two_compaction_history(&mut state);
        select_at(&mut state, 1);

        // When handling jump to next compaction.
        let result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then no commands or events are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn jump_prev_returns_no_commands() {
        // Given history with a compaction, cursor on compaction B.
        let mut state = AppState::default_with_scope_focus();
        build_two_compaction_history(&mut state);
        select_at(&mut state, 3);

        // When handling jump to previous compaction.
        let result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then no commands or events are emitted.
        assert!(result.message_names.is_empty());
    }

    /// Build a history where a collapsed ignored block sits BETWEEN two
    /// compactions, then select that collapsed block.
    ///
    /// Layout (history indices, 11 entries total):
    ///   0      user "first"
    ///   1      compaction A            <- older compaction
    ///   2..=6  5 ignored entries        <- collapse into one CollapsedIgnoredBlock
    ///   7      compaction B            <- newer compaction
    ///   8..=10 3 trailing user entries (keep the ignored block out of the
    ///                                     proximity-protected tail so it collapses)
    ///
    /// Returns (a_id, b_id, collapsed_vi_idx).
    fn build_collapsed_block_between_compactions(
        state: &mut AppState,
    ) -> (ChatEntryId, ChatEntryId, usize) {
        use crate::feat::session::chat_entry::ChangeSource;
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
        };

        state
            .active_session_mut()
            .push_entry(ChatEntry::user("first"));
        state.active_session_mut().push_entry(compaction_entry("A"));
        let a_id = state.active_session().history()[1].id.clone();
        // Five ignored entries — must exceed DEFAULT_MIN_COLLAPSE_COUNT to collapse,
        // and must sit entirely before the proximity-protected tail.
        for _ in 0..DEFAULT_MIN_COLLAPSE_COUNT + 2 {
            let mut entry = ChatEntry::user("ignored");
            entry.apply_context_override(
                crate::protocol::ContextOverride::ForcedExclude,
                ChangeSource::Internal {
                    label: "test".into(),
                },
            );
            state.active_session_mut().push_entry(entry);
        }
        state.active_session_mut().push_entry(compaction_entry("B"));
        let b_id = state
            .active_session()
            .history()
            .last()
            .expect("at least compaction B")
            .id
            .clone();
        // Trailing entries push the proximity-protected tail past the ignored block
        // so it collapses (PROXIMITY_COUNT entries are always shown).
        for n in 0..PROXIMITY_COUNT {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("trailing-{n}")));
        }

        let items = build_visual_items(
            state.active_session().history(),
            &state.active_session().shown_ignored_blocks_snapshot(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        let collapsed_vi_idx = items
            .iter()
            .position(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. }))
            .expect("should have a collapsed block");
        state.active_session_mut().set_visual_items(items);
        state
            .active_session_mut()
            .set_selected_entry_index(collapsed_vi_idx);

        (a_id, b_id, collapsed_vi_idx)
    }

    #[rstest::rstest]
    fn jump_next_from_collapsed_block_lands_on_newer_compaction() {
        // Given the cursor on a collapsed ignored block sitting between compaction A
        // (older) and compaction B (newer).
        // Regression: `]c` previously no-op'd because selected_history_index() is
        // None on a collapsed block, anchoring at history.len() (past the end).
        let mut state = AppState::default_with_scope_focus();
        let (_a_id, b_id, _vi_idx) = build_collapsed_block_between_compactions(&mut state);
        assert!(
            state.active_session().is_selected_collapsed_block(),
            "cursor must be on a collapsed block for this test"
        );

        // When handling jump to next compaction.
        let _result = handle_jump_next_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor lands on compaction B (the newer one), not a no-op.
        assert_eq!(
            state.active_session().selected_cursor_id(),
            Some(b_id),
            "]c from a collapsed block must land on the next newer compaction"
        );
    }

    #[rstest::rstest]
    fn jump_prev_from_collapsed_block_lands_on_older_compaction() {
        // Given the cursor on a collapsed ignored block sitting between compaction A
        // (older) and compaction B (newer).
        // Regression: `[c` previously jumped to the NEWEST compaction (B) because
        // the None anchor fell back to history.len(), scanning the whole history.
        let mut state = AppState::default_with_scope_focus();
        let (a_id, b_id, _vi_idx) = build_collapsed_block_between_compactions(&mut state);
        assert!(
            state.active_session().is_selected_collapsed_block(),
            "cursor must be on a collapsed block for this test"
        );

        // When handling jump to previous compaction.
        let _result = handle_jump_prev_entry(
            &mut state,
            crate::feat::session::chat_entry::ChatEntry::is_compaction,
        );

        // Then the cursor lands on compaction A (the older one), NOT compaction B.
        assert_eq!(
            state.active_session().selected_cursor_id(),
            Some(a_id),
            "[c from a collapsed block must land on the previous older compaction, not the newest"
        );
        assert_ne!(
            state.active_session().selected_cursor_id(),
            Some(b_id),
            "[c from a collapsed block must not jump forward to compaction B"
        );
    }
}

#[cfg(test)]
mod new_session_from_entry_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use crate::common::app_state::AppState;
    use crate::protocol::ChatEntry;

    use super::*;

    #[rstest::rstest]
    fn new_session_from_entry_switches_active_session_on_user_entry() {
        // Given a state with a selected User entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();
        let old_id = state.session.active_session_id().clone();

        // When handling new session from entry.
        let _result = handle_new_session_from_entry(&mut state);

        // Then the active session switched to a new one.
        assert_ne!(state.session.active_session_id(), &old_id);
    }

    #[rstest::rstest]
    fn new_session_from_entry_returns_push_chat_entry() {
        // Given a state with a selected User entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();

        // When handling new session from entry.
        let result = handle_new_session_from_entry(&mut state);

        // Then a PushChatEntry command is returned.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("PushChatEntry")),
            "expected PushChatEntry in message_names: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn new_session_from_entry_emits_session_created() {
        // Given a state with a selected User entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();

        // When handling new session from entry.
        let result = handle_new_session_from_entry(&mut state);

        // Then a SessionCreated event is returned (from the lifecycle setup).
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("SessionCreated")),
            "expected SessionCreated in message_names: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn new_session_from_entry_no_dispatch_command() {
        // Given a state with a selected User entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();

        // When handling new session from entry.
        let result = handle_new_session_from_entry(&mut state);

        // Then no dispatch command is emitted.
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("EnqueueUserMessage") || n.contains("SendToLlmProvider")),
            "no dispatch expected, got: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn new_session_from_entry_preserves_old_session_in_map() {
        // Given a state with a selected User entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();
        let old_id = state.session.active_session_id().clone();

        // When handling new session from entry.
        let _result = handle_new_session_from_entry(&mut state);

        // Then the old session is still present in the sessions map.
        assert!(state.session.contains(&old_id));
    }

    #[rstest::rstest]
    fn new_session_from_entry_inherits_active_session_cwd() {
        // Given a state whose active session has a distinct CWD.
        let mut state = AppState::default_with_scope_focus();
        let inherited_cwd = std::path::PathBuf::from("/tmp/inherited-project");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("a metaprompt"));
        state.active_session_mut().select_next_entry();

        // When handling new session from entry.
        let _result = handle_new_session_from_entry(&mut state);

        // Then the new session inherited the old active session's CWD.
        assert_eq!(state.active_session().cwd(), inherited_cwd);
    }

    #[rstest::rstest]
    fn new_session_from_entry_noop_on_system_kind() {
        // Given a state with a selected System entry.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::system("status update"));
        state.active_session_mut().select_next_entry();
        let old_id = state.session.active_session_id().clone();

        // When handling new session from entry.
        let result = handle_new_session_from_entry(&mut state);

        // Then the result is empty and the active session is unchanged.
        assert!(result.message_names.is_empty());
        assert_eq!(state.session.active_session_id(), &old_id);
    }

    #[rstest::rstest]
    fn new_session_from_entry_noop_on_no_selection() {
        // Given a state with entries but no selection.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state.active_session_mut().clear_selection();
        let old_id = state.session.active_session_id().clone();

        // When handling new session from entry.
        let result = handle_new_session_from_entry(&mut state);

        // Then the result is empty and the active session is unchanged.
        assert!(result.message_names.is_empty());
        assert_eq!(state.session.active_session_id(), &old_id);
    }

    #[rstest::rstest]
    fn build_seed_entry_user_preserves_text_and_kind() {
        // Given captured user text.
        let text = "a metaprompt".to_owned();

        // When building the seed entry for a non-assistant source.
        let seed = build_seed_entry(text.clone(), false);

        // Then the seed is a User entry with the copied text.
        assert_eq!(seed.text(), text);
        assert!(matches!(seed.kind, ChatEntryKind::User { .. }));
    }

    #[rstest::rstest]
    fn build_seed_entry_assistant_preserves_text_and_kind() {
        // Given captured assistant text.
        let text = "a generated metaprompt".to_owned();

        // When building the seed entry for an assistant source.
        let seed = build_seed_entry(text.clone(), true);

        // Then the seed is an Assistant entry with the copied text.
        assert_eq!(seed.text(), text);
        assert!(matches!(seed.kind, ChatEntryKind::Assistant(_)));
    }

    #[rstest::rstest]
    fn build_seed_entry_mints_fresh_id_distinct_from_source() {
        // Given a source entry.
        let source = ChatEntry::user("original");
        let text = source.text();

        // When building the seed entry.
        let seed = build_seed_entry(text, false);

        // Then the seed has a different id from the source.
        assert_ne!(seed.id, source.id);
    }
}
