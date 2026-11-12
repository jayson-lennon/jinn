//! Handoff compaction — the gate, handoff dispatch, and fold of
//! end-of-conversation compaction.
//!
//! Reference: "You Don't Need Long Context" (NVIDIA),
//! <https://arxiv.org/abs/2611.05306>. This module implements the
//! paper's End-of-Conversation Compaction (EoCC) regime per Section 3
//! (the compaction cycle) and Appendix A (the recap brief structure):
//!
//! - **Gate** (`evaluate_handoff_gate`): at a model-request boundary
//!   whose estimated working context — the assembled estimate minus the
//!   constant floor (system prompt, skills, shipped tool schemas), i.e.
//!   the conversation content alone — reaches the configured absolute
//!   token trigger (Appendix E's explicit total-limit/threshold pairs,
//!   e.g. 16K/12K), the pending request is replaced by a handoff turn.
//! - **Handoff turn**: the session model authors the recap
//!   in-conversation with the full context in-frame, forbidden tool
//!   calls at the API layer.
//! - **Fold** (`build_fold_mutations`): after the recap completes, the
//!   context folds down to the pins and the new recap (Appendix A:
//!   pointers + plan) — the paper's fresh conversation, resumed from
//!   the remaining plan. The recap is the sole task carrier: user turns
//!   and Intersession dispatches fold away with the rest of the
//!   conversation, so nothing accumulates verbatim across cycles.
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
    /// Below the trigger, disabled, or no valid firing regime — dispatch
    /// normally.
    Pass,
}

/// Evaluates the handoff gate for one model-request boundary.
///
/// Fires when the estimated accumulated **work** — `work_size` is the
/// assembled estimate minus the constant floor, i.e. the conversation
/// content alone — meets the configured work-token trigger. The floor
/// (system prompt, skills, shipped tool schemas) is constant across
/// requests, so it is subtracted out by construction and cannot sit at
/// the fire line.
///
/// `work_size` of `None` (no honest size pair cached — e.g. a
/// compaction just invalidated the cache and the recalc has not landed)
/// is a **pass**, not a guess: in blocking mode jinn never fires a
/// handoff it cannot measure.
#[must_use]
pub fn evaluate_handoff_gate(
    config: &HandoffCompactionConfig,
    work_size: Option<u32>,
) -> GateDecision {
    let Some(trigger) = config.resolve_trigger() else {
        return GateDecision::Pass;
    };
    let Some(work) = work_size else {
        return GateDecision::Pass;
    };
    if work >= trigger {
        GateDecision::Fire
    } else {
        GateDecision::Pass
    }
}

/// Builds the handoff nudge content from the recap prompt.
///
/// `recap_prompt` is the point-of-use load of `_handoff.md` (the recap
/// brief structure from the paper's Appendix G). The nudge preamble is
/// the paper's own: stop work, write only the recap, the conversation
/// resets afterward.
#[must_use]
pub fn build_handoff_nudge(recap_prompt: &str) -> String {
    recap_prompt.to_owned()
}

/// The worker name the fold stamps on its mutations.
const HANDOFF_WORKER: &str = "handoff-compaction";

/// Builds the fold's mutation batch from the post-handoff history.
///
/// Protected (never excluded): pinned entries (the user's explicit
/// keep-signal) and entries already excluded (idempotence). Excluded:
/// everything else — user turns and Intersession dispatches (the recap
/// carries their intent forward), assistant prose, tool calls/results
/// (the just-completed pair included), prior compaction summaries
/// (superseded by this cycle's), actor/thinking/system/error/transient
/// entries, the nudge, and the raw handoff response.
///
/// `recap_entry` is the fold's own insertion: a context-riding
/// [`ChatEntryKind::Compaction`] entry placed after `last_excluded_id`,
/// so the fresh context is system prompt + pins + recap.
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
/// Pinned entries are the user's explicit keep-signal and outrank
/// everything; already-excluded entries stay excluded (idempotence — a
/// re-fold must not restamp them). Everything else — user turns,
/// Intersession dispatches, assistant prose, tool traffic, prior
/// summaries — joins the swept set: the recap is the sole task carrier
/// across compaction cycles.
#[must_use]
pub fn is_fold_protected(entry: &ChatEntry) -> bool {
    if entry.pin_position.is_some() {
        return true;
    }
    entry.context_override == ContextOverride::ForcedExclude
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
