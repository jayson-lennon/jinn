//! Runtime-only state for one live session.

use jiff::Timestamp;
use jinn_context::{ContextFile, PromptTemplateStore};
use jinn_core_types::{HistoryMutation, tool_types::ToolResult};
use jinn_session_msg::phase_machine::SessionPhaseMachine;
use jinn_skills_msg::Skill;
use jinn_turn_dispatch_msg::TurnQueue;
use serde::{Deserialize, Serialize};

use crate::mutation_accumulator::MutationAccumulator;
use crate::steering_buffer::SteeringBuffer;

/// Session state that exists only for the current process and is never persisted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCoreEphemeral {
    /// Validated phase transition state for the active turn.
    pub machine: SessionPhaseMachine,
    /// Buffered work waiting for the next safe dispatch boundary.
    pub message_queue: TurnQueue,
    /// Most recently assembled prompt size, in tokens.
    pub cached_context_size: Option<u32>,
    /// Generation marker for the currently active inference stream.
    #[serde(skip)]
    pub stream_dispatched_at: Option<Timestamp>,
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
    /// Tool results that arrived before the matching phase transition completed.
    #[serde(skip)]
    pub pending_tool_batch: Option<Vec<ToolResult>>,
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
