//! Context assembly - building LLM-ready prompts from chat history.
//!
//! # Assembly Pipeline
//!
//! Chat history is assembled into LLM-ready messages through these stages:
//!
//! 1. **Pin splitting** - entries are separated into TOP pins, BOTTOM pins,
//!    and working history based on [`PinPosition`](crate::protocol::PinPosition).
//! 2. **Compaction** - if the working history exceeds the session's token budget,
//!    entries are trimmed newest-to-oldest (preserving pinned entries); compaction
//!    summaries ride in the working history as ordinary messages.
//! 3. **System prompt construction** - the system prompt is composed from
//!    dedicated per-section builders in fixed order:
//!    - Persona body
//!    - Project context files
//!    - Tool context block
//!    - Skills block (`<available_skills>` XML catalog)
//!    - Current date
//!    - Working directory
//!
//!    Sections with no content are omitted entirely.
//! 4. **Message ordering** - the final array is pure conversation history:
//!    `[TOP pins] → [compacted working history] → [BOTTOM pins] → [last message]`.
//!    The system prompt travels separately from the assembled messages.
//!
//! # Contents
//!
//! This module defines the compaction assembly logic and supporting types.
//! Also contains the **ContextActor** (prompt assembly, pinning, templates).

pub mod assemble;
pub mod assembly_state;
pub mod context_size_actor;
pub mod env_context;

pub mod prompt_template;
pub mod protocol;
pub mod strategy;
pub mod tool_prompt;

pub use strategy::token_estimator::{
    CharRatioEstimator, TokenEstimator, estimate_entry_content_tokens, estimate_entry_tokens,
    estimate_tool_schema_tokens,
};
