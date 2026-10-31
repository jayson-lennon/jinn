//! Runtime-only state for one live session.

use std::collections::HashMap;

use jiff::Timestamp;
use jinn_context::{ContextFile, PromptTemplateStore};
use jinn_core_types::HistoryMutation;
use jinn_core_types::ToolCallId;
use jinn_skills_msg::Skill;
use jinn_slices::PhaseView;
use jinn_turn_dispatch_msg::TurnQueue;
use jinn_workflows_msg::Workflow;
use serde::{Deserialize, Serialize};

use crate::mutation_accumulator::MutationAccumulator;
use crate::steering_buffer::SteeringBuffer;

/// Session state that exists only for the current process and is never persisted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCoreEphemeral {
    /// The session's phase, as everyone reads it.
    ///
    /// A **display projection**: written by the turn ledger's own admission
    /// and settlement paths (`mint_dispatch`, the session actor's settle),
    /// never by anything deciding admission. The ledger is the authority —
    /// this field exists so a renderer reads one value instead of asking a
    /// service across the workspace.
    pub phase: PhaseView,
    /// Index into history for the entry currently receiving stream tokens.
    pub streaming_entry_index: Option<usize>,
    /// Index into history for the entry currently receiving thinking tokens.
    pub streaming_thinking_entry_index: Option<usize>,
    /// Maps stream tool-call index to history index for in-progress tool
    /// calls whose arguments are still arriving. Turn-scoped: survives the
    /// `Streaming`/`Working` swaps within one turn, cleared at settle.
    pub streaming_tool_call_indices: HashMap<usize, usize>,
    /// Maps `tool_call_id` to history index for the in-flight tool batch's
    /// `Pending` result entries. Cleared at settle.
    pub streaming_tool_result_indices: HashMap<ToolCallId, usize>,
    /// When `true`, the turn settles to `Idle` instead of continuing the
    /// tool loop. Set by judge verdict tools; self-clearing on read.
    #[serde(default)]
    pub tool_loop_disabled: bool,
    /// When `true`, the settle's `Cancel` arm drains the steering buffer and
    /// the turn queue into the input box before reporting `Idle`, instead of
    /// leaving them for the queue actor to re-dispatch. Set when a
    /// drain-flagged `CancelTurn` is handled; self-clearing on read.
    /// Process-local: a cancel rides one settle, so the request never needs
    /// to outlive the message that carried it.
    #[serde(skip, default)]
    pub pending_cancel_drain: bool,
    /// Buffered work waiting for the next safe dispatch boundary.
    pub message_queue: TurnQueue,
    /// Most recently assembled prompt size, in tokens.
    pub cached_context_size: Option<u32>,
    /// Generation marker for the currently active inference stream.
    #[serde(skip)]
    pub stream_dispatched_at: Option<Timestamp>,
    /// The model requested at dispatch for the currently active inference
    /// stream. Entries created while a generation is live are stamped from
    /// this; the completion-time response override never touches it.
    #[serde(skip)]
    pub stream_request_model: Option<String>,
    /// Deduplicated context overrides waiting for an accumulation threshold.
    #[serde(skip)]
    pub accumulated_overrides: MutationAccumulator,
    /// History mutation batches waiting for a safe turn boundary.
    #[serde(skip)]
    pub pending_mutations: Vec<Vec<HistoryMutation>>,
    /// Skills discovered for this session's project context.
    #[serde(skip)]
    pub discovered_skills: Vec<Skill>,
    /// Prompt templates discovered for this session's project context.
    #[serde(skip)]
    pub discovered_prompt_templates: PromptTemplateStore,
    /// Project context files discovered for this session's working directory.
    #[serde(skip)]
    pub discovered_context_files: Vec<ContextFile>,
    /// Workflows discovered for this session (launch-global, config dir).
    #[serde(skip)]
    pub discovered_workflows: Vec<Workflow>,
}

/// Presentation-owned state that is discarded when a session closes.
#[derive(Debug, Default)]
pub struct SessionUi {
    /// Fragments submitted while a turn is in progress.
    pub steering_buffer: SteeringBuffer,
}

impl Clone for SessionUi {
    fn clone(&self) -> Self {
        Self {
            steering_buffer: self.steering_buffer.clone(),
        }
    }
}

impl SessionCoreEphemeral {
    /// Disable the tool loop for this session's current turn.
    ///
    /// The turn settles to `Idle` instead of continuing the tool loop.
    /// Survives every phase swap; the flag is consumed (cleared) by
    /// [`Self::take_tool_loop_disabled`].
    pub fn set_tool_loop_disabled(&mut self) {
        self.tool_loop_disabled = true;
    }

    /// Take the tool-loop-disabled flag, clearing it.
    pub fn take_tool_loop_disabled(&mut self) -> bool {
        std::mem::take(&mut self.tool_loop_disabled)
    }

    /// Returns `true` if the tool loop is disabled, without clearing.
    pub fn is_tool_loop_disabled(&self) -> bool {
        self.tool_loop_disabled
    }

    /// Request a buffer drain at this turn's cancel settle.
    ///
    /// Set by the session actor when the handled `CancelTurn` carries the
    /// drain request; consumed by the settle's `Cancel` arm. Survives every
    /// phase swap between the message and the settle, because the drain
    /// points (`StartedAndSettled` inline and the completion ack) can be a
    /// message or two apart.
    pub fn request_cancel_drain(&mut self) {
        self.pending_cancel_drain = true;
    }

    /// Take the pending-cancel-drain request, clearing it.
    pub fn take_pending_cancel_drain(&mut self) -> bool {
        std::mem::take(&mut self.pending_cancel_drain)
    }

    /// Shift all streaming indices >= `inserted_at` by +1.
    ///
    /// Called after an entry is inserted to keep indices valid.
    pub fn shift_streaming_indices_for_insert_at(&mut self, inserted_at: usize) {
        for value in self.streaming_tool_call_indices.values_mut() {
            if *value >= inserted_at {
                *value += 1;
            }
        }
        for value in self.streaming_tool_result_indices.values_mut() {
            if *value >= inserted_at {
                *value += 1;
            }
        }
        if let Some(ref mut i) = self.streaming_entry_index
            && *i >= inserted_at
        {
            *i += 1;
        }
        if let Some(ref mut i) = self.streaming_thinking_entry_index
            && *i >= inserted_at
        {
            *i += 1;
        }
    }

    /// Shift every live streaming index above `removed_at` down by one.
    ///
    /// An index landing exactly on `removed_at` names the removed entry and
    /// becomes the `usize::MAX` sentinel: out of range for every history
    /// read, but still the call's identity for the next delta.
    pub fn shift_streaming_indices_after_remove_at(&mut self, removed_at: usize) {
        for value in self.streaming_tool_call_indices.values_mut() {
            *value = shift_removed_index(*value, removed_at);
        }
        for value in self.streaming_tool_result_indices.values_mut() {
            *value = shift_removed_index(*value, removed_at);
        }
        self.streaming_entry_index = self
            .streaming_entry_index
            .map(|i| shift_removed_index(i, removed_at));
        self.streaming_thinking_entry_index = self
            .streaming_thinking_entry_index
            .map(|i| shift_removed_index(i, removed_at));
    }

    /// Clear every streaming and tool index without touching the phase.
    ///
    /// Called when a turn's registrations are consumed (a stall retry's
    /// rewind) or settled.
    pub fn clear_streaming_indices(&mut self) {
        self.streaming_entry_index = None;
        self.streaming_thinking_entry_index = None;
        self.streaming_tool_call_indices.clear();
        self.streaming_tool_result_indices.clear();
    }

    /// Whether the session is tracking a tool call at the given history index.
    pub fn is_tool_call_at_history_index(&self, history_index: usize) -> bool {
        self.streaming_tool_call_indices
            .values()
            .any(|&v| v == history_index)
    }
}

/// Where a stored history index points after the entry at `removed_at` is
/// deleted.
///
/// An index *at* the removal site named the entry that just went away, so it
/// becomes `usize::MAX` — a sentinel no entry occupies, and out of range for
/// every `history.get(..)` that reads it. An index above it shifts down one.
/// An index below it is untouched.
fn shift_removed_index(value: usize, removed_at: usize) -> usize {
    match value.cmp(&removed_at) {
        std::cmp::Ordering::Less => value,
        std::cmp::Ordering::Equal => usize::MAX,
        std::cmp::Ordering::Greater => value - 1,
    }
}
