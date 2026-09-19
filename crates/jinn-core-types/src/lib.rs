//! Foundational, domain-agnostic value types shared across the jinn workspace.
//!
//! Residents here are pure value types (newtypes over primitives) with no
//! dependency on domain logic, actors, or app state. They exist so that leaf
//! crates can reference a shared type without depending on `jinn-domain`.
//!
//! Types are added as-needed. This is not a dumping ground: only types that are
//! both foundational and domain-agnostic belong here.
//!
//! `chat_entry`/`chat_history`/`history_mutation`/`tool_result_status`/
//! `entry_timing` (the `ChatEntry` vocabulary, promoted from the kernel
//! session feature, 2026-09-19) are serde-only data: every field is a
//! `jinn-core-types` value or a plain serde scalar. Their tests and the
//! `HistoryEditor` write path stay kernel-side (the editor mutates session
//! state, not these types).

pub mod actor_lifecycle;
pub mod attachment;
pub mod chat_entry;
pub mod chat_entry_id;
pub mod chat_history;
pub mod context_override;
pub mod entry_timing;
pub mod history_mutation;
pub mod llm_message;
pub mod model_selection;
pub mod reasoning;
pub mod session_id;
pub mod tool_result_status;
pub mod tool_types;
pub mod url_citation;

#[cfg(test)]
#[path = "chat_entry_tests.rs"]
mod chat_entry_tests;

pub use actor_lifecycle::ActorLifecycle;
pub use attachment::Attachment;
pub use chat_entry::{
    AttachmentOutcome, ChangeSource, ChatEntry, ChatEntryKind, ContextChangeEvent, PinPosition,
    ResolvedToken,
};
pub use chat_entry_id::ChatEntryId;
pub use chat_history::ChatHistory;
pub use context_override::ContextOverride;
pub use entry_timing::EntryTiming;
pub use history_mutation::HistoryMutation;
pub use llm_message::LlmMessage;
pub use model_selection::{AlloyData, AlloyStrategy, ModelSelection, NO_PROVIDER_ID};
pub use reasoning::ReasoningEffort;
pub use session_id::SessionId;
pub use tool_result_status::ToolResultStatus;
pub use tool_types::{
    ServerToolType, ToolCall, ToolDefinition, ToolResult, ToolResultPinPosition, TruncatedBy,
    TruncationMeta,
};
pub use url_citation::UrlCitation;
