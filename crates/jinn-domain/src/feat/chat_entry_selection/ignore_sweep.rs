//! Sweep continuation for the ignore-selected intent.
//!
//! Holding `x` applies the _captured_ context-override (not a toggle) to
//! exactly one eligible entry per keypress, advancing the cursor one step
//! for the next press. The captured direction is carried across presses by a
//! short (~100ms) memory window: as long as keypresses keep arriving inside
//! the window, each one continues in the same direction (into or out of
//! context) rather than re-toggling.
//!
//! Pre-existing obstacles — pinned entries and collapsed ignored blocks —
//! are skipped transparently: the sweep advances past them within a single
//! press without adjusting them, so the user never sees a "skip" as a
//! separate step.
//!
//! If applying an override forms a NEW collapsed block that swallows the
//! cursor (because the just-excluded entry now joins a run of neighbors),
//! the sweep advances past that block once and stops — exactly one entry was
//! adjusted this press. The memory stays armed so the next press picks up
//! beyond the block.
//!
//! Under the pure-toggle model, the captured target is always `ForcedInclude`
//! or `ForcedExclude` — never `Default`. Resetting to `Default` is `r`'s job,
//! which does not use this sweep (it advances the cursor directly).
use crate::common::app_state::AppState;
use crate::feat::context::protocol::event::ContextOverrideChanged;
use crate::feat::session_lifecycle::protocol::command::PersistSession;
use crate::protocol::ChatEntry;
use crate::protocol::{ChatEntryId, ContextOverride, IntentResult, SessionId};

use super::intent::advance_selection_one;

/// Execute a full sweep invocation: apply the captured `target` override,
/// skipping pinned entries and collapsed blocks, until an eligible entry is
/// found or the bottom is reached.
///
/// This replaces the old inline loop in `handle_ignore_selected`.
pub(crate) fn run_sweep(state: &mut AppState, target: ContextOverride) -> IntentResult {
    let session_id = state.active_session().session_id().clone();
    let mut changed_ids: Vec<ChatEntryId> = Vec::new();

    loop {
        let session = state.active_session_mut();

        if session.selected_entry_index().is_none() {
            return IntentResult::empty();
        }

        // Skip pinned entries and collapsed blocks.
        let is_pinned = session.selected_entry().is_some_and(ChatEntry::is_pinned);
        if is_pinned || session.is_selected_collapsed_block() {
            if !advance_selection_one(session) {
                if changed_ids.is_empty() {
                    return IntentResult::empty();
                }
                return finalize_sweep(state, &session_id, changed_ids);
            }
            continue;
        }

        // Apply the captured state directly (not a toggle).
        if let Some(id) = session.set_entry_context_override(target) {
            changed_ids.push(id);
        }
        session.rebuild_visual_items();

        // Applying the override may have collapsed the entry we just changed
        // (together with neighbors) into a CollapsedIgnoredBlock, leaving the
        // cursor on that block (`selected_entry()` -> None). This is a NEW
        // collapse created by this press — NOT a pre-existing obstacle. We must
        // advance past it exactly once and STOP, otherwise the loop would
        // re-apply to the next entry in the same keypress and chain all the way
        // to the bottom.
        if session.selected_entry().is_none() {
            advance_selection_one(session); // best-effort; at-bottom is also terminal
            session.set_ignore_sweep(target);
            return finalize_sweep(state, &session_id, changed_ids);
        }

        let Some(selected) = session.selected_entry() else {
            session.set_ignore_sweep(target);
            return finalize_sweep(state, &session_id, changed_ids);
        };
        let entry_id = selected.id.clone();

        // Propagate shown state to new sub-blocks when un-ignoring.
        // Under the pure-toggle model, `x` only ever captures ForcedInclude
        // (into context) or ForcedExclude (out of context). ForcedExclude takes
        // the entry out of context, so only ForcedInclude can split a shown
        // excluded block and needs propagation.
        if matches!(target, ContextOverride::ForcedInclude) {
            session.propagate_shown_on_unignore(&entry_id);
        }

        // Advance cursor for next keypress.
        let at_bottom = !advance_selection_one(session);
        session.set_ignore_sweep(target);

        if at_bottom {
            return finalize_sweep(state, &session_id, changed_ids);
        }
        return finalize_sweep(state, &session_id, changed_ids);
    }
}

/// Common tail for the x-sweep: persist session and emit one
/// `ContextOverrideChanged` event per entry whose override actually changed.
fn finalize_sweep(
    _state: &mut AppState,
    session_id: &SessionId,
    changed_ids: Vec<ChatEntryId>,
) -> IntentResult {
    let events = changed_ids.into_iter().map(|id| ContextOverrideChanged {
        session_id: session_id.clone(),
        entry_id: id,
    });
    IntentResult::new_message(PersistSession {
        session_id: session_id.clone(),
    })
    .with_messages(events)
}
