//! Isolate intent - force-include the highlighted chunk, user-force-exclude the rest.
//!
//! `gci` keeps only the highlighted entry's tool loop in LLM context: the
//! highlighted chunk gets `ForcedInclude` and every other non-pinned chunk gets
//! a user `ForcedExclude`. Writes go through the same per-chunk
//! [`HistoryEditor::set_context`] path the ignore sweep uses, so pin guards,
//! chunk atomicity, no-op detection, and pruner precedence behave identically.
//!
//! The cursor remains on the highlighted entry. Already-correct chunks no-op
//! inside the editor; a fully idempotent re-press returns an empty result.
//! No-op cases: empty history, cursor resting on a collapsed block (no
//! selected entry), or a pinned highlighted entry (validator rejection).

use super::validator;
use crate::common::app_state::AppState;
use crate::feat::context::protocol::event::ContextOverrideChanged;
use crate::feat::session_lifecycle::protocol::command::PersistSession;
use crate::protocol::{ChatEntryId, ContextOverride, IntentResult};
use jinn_session_history::history_editor::tool_group_end;

/// Walk the history chunk by chunk and set each chunk's context override:
/// [`ContextOverride::ForcedInclude`] for the chunk containing
/// `highlight_idx`, [`ContextOverride::ForcedExclude`] for all others.
///
/// Returns the ids the editor actually changed (one per mutated chunk,
/// anchored on the chunk's first member). Pinned chunks refuse exclusion and
/// contribute nothing; already-correct chunks are editor no-ops.
fn isolate_all_but_highlighted_chunk(
    state: &mut AppState,
    highlight_idx: usize,
    history_len: usize,
) -> Vec<ChatEntryId> {
    let mut changed_ids = Vec::new();
    let mut idx = 0;
    while idx < history_len {
        let (chunk_end, is_highlight_chunk) = {
            let history = state.active_session().history();
            let end = tool_group_end(history, idx).unwrap_or(idx + 1);
            (end, (idx..end).contains(&highlight_idx))
        };
        let target = if is_highlight_chunk {
            ContextOverride::ForcedInclude
        } else {
            ContextOverride::ForcedExclude
        };
        // Chunk representative: the first member id. `set_context` expands
        // the write to the whole tool loop, so one call per chunk suffices.
        let Some(anchor_id) = state
            .active_session()
            .history()
            .get(idx)
            .map(|e| e.id.clone())
        else {
            break;
        };
        if let Some(id) = state
            .active_session_mut()
            .set_entry_context_override_by_id(&anchor_id, target)
        {
            changed_ids.push(id);
        }
        idx = chunk_end;
    }
    changed_ids
}

/// Force-include the highlighted chunk and user-force-exclude every other
/// non-pinned chunk, then persist and emit one event per changed entry.
pub fn handle_isolate_selected(state: &mut AppState) -> IntentResult {
    // Validate: empty history, cursor on a collapsed block, or a pinned
    // highlight all no-op.
    if validator::validate_chat_entry_isolate_selected(state).is_err() {
        return IntentResult::empty();
    }

    let session_id = state.active_session().session_id().clone();

    // Capture the highlight before mutating. Ids and indices are stable
    // (set_context mutates entries in place, never removes), so the captured
    // index stays valid for the whole walk. The validator guarantees a
    // selection; the graceful returns only guard a cursor resting on a
    // collapsed block, which resolves to no entry/index.
    let Some(selected) = state.active_session().selected_entry() else {
        return IntentResult::empty();
    };
    let highlight_id = selected.id.clone();
    let Some(highlight_idx) = state.active_session().selected_history_index() else {
        return IntentResult::empty();
    };
    let history_len = state.active_session().history().len();

    let changed_ids = isolate_all_but_highlighted_chunk(state, highlight_idx, history_len);

    // Visual bookkeeping: a shown excluded block that now contains an
    // in-context entry must not visually split; cursor stays anchored on the
    // highlighted entry.
    {
        let session = state.active_session_mut();
        session.propagate_shown_on_unignore(&highlight_id);
        session.rebuild_visual_items();
        session.set_selected_cursor_id(highlight_id);
    }

    // Nothing changed (e.g. second press): silent no-op, like reset.
    if changed_ids.is_empty() {
        return IntentResult::empty();
    }

    // Persist once and emit one event per changed entry (context-size and
    // token-count actors re-render from these).
    let events = changed_ids.into_iter().map(|id| ContextOverrideChanged {
        session_id: session_id.clone(),
        entry_id: id,
    });
    IntentResult::new_message(PersistSession {
        session_id: session_id.clone(),
    })
    .with_messages(events)
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
    use crate::protocol::PinPosition;
    use crate::protocol::ToolResultStatus;
    use crate::protocol::{ChatEntry, ContextOverride};

    use super::*;

    /// user, empty assistant, one call, one result (a complete tool loop).
    fn loop_entries() -> Vec<ChatEntry> {
        vec![
            ChatEntry::user("run it"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "bash", "{}"),
            ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ]
    }

    fn state_with(entries: Vec<ChatEntry>) -> AppState {
        let mut state = AppState::default();
        for entry in entries {
            state.active_session_mut().push_entry(entry);
        }
        state
    }

    fn overrides(state: &AppState) -> Vec<ContextOverride> {
        state
            .active_session()
            .history()
            .iter()
            .map(crate::protocol::ChatEntry::context_override)
            .collect()
    }

    #[rstest::rstest]
    fn isolate_keeps_whole_tool_loop_of_highlighted_result() {
        // Given one loop with the cursor on its tool result.
        let mut state = state_with(loop_entries());
        let history_len = state.active_session().history().len();
        let result_id = state.active_session().history()[3].id.clone();
        state.active_session_mut().set_selected_cursor_id(result_id);

        // When handling isolate selected.
        let _result = handle_isolate_selected(&mut state);

        // Then the whole loop (assistant, call, result) plus the user opener
        // chunk (forced-excluded) is consistent: every loop member is in
        // context.
        let history = state.active_session().history();
        assert_eq!(history.len(), history_len, "entries are never removed");
        assert!(
            history[1..4].iter().all(ChatEntry::is_in_context),
            "assistant+call+result all stay in context"
        );
        // And the user opener was forced-excluded.
        assert_eq!(overrides(&state)[0], ContextOverride::ForcedExclude);
    }

    #[rstest::rstest]
    fn isolate_skips_pinned_chunks_without_events() {
        // Given two loops with the first loop's result pinned, highlight on
        // the second loop's user opener.
        let mut state = state_with([loop_entries(), loop_entries()].concat());
        let first_result_id = state.active_session().history()[3].id.clone();
        state
            .active_session_mut()
            .pin_entry(&first_result_id, PinPosition::Relative);
        let opener_id = state.active_session().history()[4].id.clone();
        state.active_session_mut().set_selected_cursor_id(opener_id);

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then the pinned loop's members are untouched (pin wins, they stay
        // in context).
        let history = state.active_session().history();
        assert!(
            history[1..4].iter().all(ChatEntry::is_in_context),
            "pinned chunk remains in context"
        );
        // And the first loop's non-pinned user opener is still excluded, but
        // the pinned member itself emitted no override change (editor
        // refuses; its override is untouched).
        assert_eq!(overrides(&state)[3], ContextOverride::Default);
        // And no event names beyond persist and the changed entries appear.
        assert!(
            result
                .message_names
                .iter()
                .all(|n| n.contains("PersistSession") || n.contains("ContextOverrideChanged")),
            "only persist and override events are emitted"
        );
    }

    #[rstest::rstest]
    fn isolate_emits_persist_and_one_event_per_changed_chunk() {
        // Given two loops, highlight on the second loop's user opener.
        let mut state = state_with([loop_entries(), loop_entries()].concat());
        let opener_id = state.active_session().history()[4].id.clone();
        state.active_session_mut().set_selected_cursor_id(opener_id);

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then persist happens once.
        assert_eq!(
            result
                .message_names
                .iter()
                .filter(|n| n.contains("PersistSession"))
                .count(),
            1,
            "exactly one PersistSession"
        );
        // And four chunks changed: the opener (Default→include), its loop,
        // the first loop, and the first opener (Default→exclude) — one event
        // per chunk, mirroring the x path's per-chunk event granularity.
        let override_events = result
            .message_names
            .iter()
            .filter(|n| n.contains("ContextOverrideChanged"))
            .count();
        assert_eq!(override_events, 4, "one event per changed chunk");
    }

    #[rstest::rstest]
    fn isolate_already_excluded_entries_emit_nothing() {
        // Given two loops with the first opener already user-force-excluded,
        // highlight on the second loop's opener (it and its loop must still
        // change; the first opener must not).
        let mut state = state_with([loop_entries(), loop_entries()].concat());
        let first_opener_id = state.active_session().history()[0].id.clone();
        state
            .active_session_mut()
            .set_entry_context_override_by_id(&first_opener_id, ContextOverride::ForcedExclude);
        let second_opener_id = state.active_session().history()[4].id.clone();
        state
            .active_session_mut()
            .set_selected_cursor_id(second_opener_id);

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then only the remaining not-yet-excluded chunks changed: the first
        // loop and the second loop (the second opener changes from Default to
        // include, its loop and the first loop exclude, the first opener
        // no-ops) — three chunk events, and no re-emission for the first
        // opener.
        let override_events = result
            .message_names
            .iter()
            .filter(|n| n.contains("ContextOverrideChanged"))
            .count();
        assert_eq!(override_events, 3, "already-excluded opener no-ops");
        // And the state ends fully isolated on the second loop's opener.
        let history = state.active_session().history();
        assert_eq!(overrides(&state)[0], ContextOverride::ForcedExclude);
        assert!(history[4].is_in_context());
        assert!(
            history[5..8]
                .iter()
                .all(|e| e.context_override() == ContextOverride::ForcedExclude)
        );
    }

    #[rstest::rstest]
    fn isolate_is_idempotent_on_second_press() {
        // Given a state that was already isolated on the highlighted opener.
        let mut state = state_with(loop_entries());
        let opener_id = state.active_session().history()[0].id.clone();
        state.active_session_mut().set_selected_cursor_id(opener_id);
        let _first = handle_isolate_selected(&mut state);

        // When handling isolate selected again.
        let result = handle_isolate_selected(&mut state);

        // Then nothing is emitted (every chunk is already at target).
        assert!(
            result.message_names.is_empty(),
            "second press is a silent no-op"
        );
    }

    #[rstest::rstest]
    fn isolate_preserves_cursor_on_highlighted_entry() {
        // Given two loops with the highlight on the second loop's opener.
        let mut state = state_with([loop_entries(), loop_entries()].concat());
        let opener_id = state.active_session().history()[4].id.clone();
        state
            .active_session_mut()
            .set_selected_cursor_id(opener_id.clone());

        // When handling isolate selected.
        let _result = handle_isolate_selected(&mut state);

        // Then the cursor still rests on the highlighted entry.
        assert_eq!(
            state.active_session().selected_cursor_id(),
            Some(opener_id),
            "cursor stays on the highlighted entry"
        );
    }

    #[rstest::rstest]
    fn isolate_noops_on_pinned_highlight() {
        // Given a pinned highlighted opener.
        let mut state = state_with(loop_entries());
        let opener_id = state.active_session().history()[0].id.clone();
        state
            .active_session_mut()
            .set_selected_cursor_id(opener_id.clone());
        state
            .active_session_mut()
            .pin_entry(&opener_id, PinPosition::Top);

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then nothing is emitted and all overrides stay Default.
        assert!(result.message_names.is_empty());
        assert!(
            overrides(&state)
                .iter()
                .all(|o| *o == ContextOverride::Default),
            "no override was touched"
        );
    }

    #[rstest::rstest]
    fn isolate_noops_on_empty_history() {
        // Given an empty session.
        let mut state = AppState::default();

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then nothing is emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn isolate_noops_without_selection() {
        // Given a session with entries but no selection.
        let mut state = state_with(loop_entries());
        state.active_session_mut().clear_selection();

        // When handling isolate selected.
        let result = handle_isolate_selected(&mut state);

        // Then nothing is emitted and no override changed.
        assert!(result.message_names.is_empty());
        assert!(
            overrides(&state)
                .iter()
                .all(|o| *o == ContextOverride::Default)
        );
    }

    #[rstest::rstest]
    fn isolate_propagates_shown_block_on_highlighted_include() {
        // Given an excluded block (user opener + loop) marked shown, with the
        // highlight on the loop's tool result inside it.
        let mut state = state_with(loop_entries());
        let opener_id = state.active_session().history()[0].id.clone();
        let result_id = state.active_session().history()[3].id.clone();
        state
            .active_session_mut()
            .set_selected_cursor_id(result_id.clone());
        state
            .active_session_mut()
            .set_entry_context_override_by_id(&opener_id, ContextOverride::ForcedExclude);
        state.active_session_mut().rebuild_visual_items();
        state
            .active_session_mut()
            .toggle_ignored_block_visibility(&opener_id);
        assert!(
            state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&opener_id),
            "block is shown before isolate"
        );

        // When handling isolate selected (the tool result's chunk comes into
        // context, splitting the shown block).
        let _result = handle_isolate_selected(&mut state);

        // Then the highlighted loop is back in context.
        let history = state.active_session().history();
        assert!(history[1..4].iter().all(ChatEntry::is_in_context));
        // And the shown-block tracking survived the split: the forward
        // opener sub-block is marked shown.
        assert!(
            state
                .active_session()
                .shown_ignored_blocks_snapshot()
                .contains(&opener_id),
            "forward sub-block is tracked as shown"
        );
    }
}
