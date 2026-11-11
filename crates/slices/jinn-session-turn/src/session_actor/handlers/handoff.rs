//! Handoff compaction — the EoCC gate, handoff dispatch, and fold.
//!
//! Implements the End-of-Conversation Compaction regime from
//! arXiv 2611.05306: at a model-request boundary whose estimated context
//! crosses the configured threshold, the pending request is replaced by a
//! handoff turn — the session model authors a recap in-conversation with
//! the full context in-frame — after which the fold excludes everything
//! except user turns, Intersession entries, pins, and the new recap
//! (inserted as a context-riding [`ChatEntryKind::Compaction`] entry).
//!
//! The three pieces here are pure: the gate math, the handoff content
//! builder, and the fold's mutation builder. The session actor wires
//! them at the request boundary (`enqueue.rs`) and completion
//! (`streaming.rs`).

use jinn_core_types::ChatEntryId;
use jinn_core_types::HistoryMutation;
use jinn_core_types::entry_timing::EntryTiming;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};
use jinn_preferences_config::schemas::HandoffCompactionConfig;

#[cfg(test)]
mod gate_tests;

/// The gate's decision for one model-request boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Fire: replace the pending request with the handoff turn.
    Fire,
    /// Below threshold, disabled, or no effective limit — dispatch
    /// normally.
    Pass,
}

/// Evaluates the handoff gate for one model-request boundary.
///
/// Fires when the estimated context (`assembled_size + excluded_cost`)
/// meets `threshold × effective_limit`. `assembled_size` is the session's
/// cached assembled estimate (the status-bar value); `excluded_cost` is
/// the token cost of entries excluded since that assembly.
///
/// A missing effective limit is a **pass**, not a guess: in blocking mode
/// jinn never fires a handoff it cannot bound.
#[must_use]
pub fn evaluate_handoff_gate(
    config: &HandoffCompactionConfig,
    effective_limit: Option<u32>,
    assembled_size: Option<u32>,
    excluded_cost: u32,
) -> GateDecision {
    if !config.enabled {
        return GateDecision::Pass;
    }
    let Some(limit) = effective_limit else {
        return GateDecision::Pass;
    };
    let Some(assembled) = assembled_size else {
        return GateDecision::Pass;
    };
    let threshold = config.effective_threshold();
    let projected = u64::from(assembled) + u64::from(excluded_cost);
    let line = f64::from(limit) * threshold;
    if f64::from(u32::try_from(projected).unwrap_or(u32::MAX)) >= line {
        GateDecision::Fire
    } else {
        GateDecision::Pass
    }
}

/// Builds the handoff nudge content from the recap prompt.
///
/// `recap_prompt` is the point-of-use load of `_handoff.md` (the
/// Appendix G structure). The nudge preamble is the paper's own: stop
/// work, write only the recap, the conversation resets afterward.
#[must_use]
pub fn build_handoff_nudge(recap_prompt: &str) -> String {
    format!("{recap_prompt}")
}

/// The worker name the fold stamps on its mutations.
const HANDOFF_WORKER: &str = "handoff-compaction";

/// Builds the fold's mutation batch from the post-handoff history.
///
/// Protected (never excluded): user turns, Intersession entries, pinned
/// entries, entries already excluded (idempotent). Excluded: everything
/// else — assistant prose, tool calls/results (the just-completed pair
/// included), prior compaction summaries (superseded by this cycle's),
/// actor/thinking/system/error/transient entries, the nudge, and the raw
/// handoff response.
///
/// `recap_entry` is the fold's own insertion: a context-riding
/// [`ChatEntryKind::Compaction`] entry placed after `last_excluded_id`,
/// so the fresh context is system prompt + task + recap — the paper's
/// fresh conversation.
///
/// Pins are never mutated by the fold.
#[must_use]
pub fn build_fold_mutations(
    history: &[ChatEntry],
    recap_entry: ChatEntry,
    last_excluded_id: Option<ChatEntryId>,
) -> Vec<HistoryMutation> {
    let source = ChangeSource::Worker {
        name: HANDOFF_WORKER.to_owned(),
    };
    let mut mutations = Vec::new();
    for entry in history {
        if is_fold_protected(entry) {
            continue;
        }
        mutations.push(HistoryMutation::SetContextOverride {
            entry_id: entry.id.clone(),
            value: ContextOverride::ForcedExclude,
            source: source.clone(),
        });
    }
    mutations.push(HistoryMutation::InsertEntry {
        after_entry_id: last_excluded_id,
        entry: recap_entry,
    });
    mutations
}

/// Whether the fold must leave `entry` in the fresh context.
///
/// User turns and Intersession dispatches are the task record (the
/// paper's verbatim task); pinned entries are the user's explicit
/// keep-signal; already-excluded entries stay excluded (idempotence).
#[must_use]
pub fn is_fold_protected(entry: &ChatEntry) -> bool {
    if entry.pin_position.is_some() {
        return true;
    }
    match &entry.kind {
        ChatEntryKind::User { .. } | ChatEntryKind::Intersession { .. } => true,
        _ => entry.context_override == ContextOverride::ForcedExclude,
    }
}

/// Constructs the recap entry the fold inserts.
///
/// Carries the same shape the background compaction writer produces, so
/// search, rendering, and context inclusion treat the two identically.
#[must_use]
pub fn build_recap_entry(
    id: ChatEntryId,
    recap_text: String,
    tokens_before: usize,
    tokens_after: usize,
    entries_compacted: usize,
    model_used: String,
) -> ChatEntry {
    ChatEntry::new_with_kind(
        id,
        EntryTiming::instant_now(),
        ChatEntryKind::Compaction {
            summary: recap_text,
            tokens_before,
            tokens_after,
            entries_compacted,
            model_used,
        },
        None,
    )
}
