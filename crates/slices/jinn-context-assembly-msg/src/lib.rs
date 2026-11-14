//! Shared contracts for the context-assembly slice.
//!
//! This crate defines the inputs, requests, and responses exchanged across
//! the context-assembly boundary without depending on its implementation.

use std::collections::HashSet;

use jinn_context::ContextFile;
use jinn_core_types::{ChatEntry, NameFilter, SessionId, ToolDefinition};
use jinn_skills_msg::Skill;
use jinn_slices::AssembledPrompt;
use jinn_slices::Persona;
use marked_path::{Absolute, MarkedPathBuf};
use serde::{Deserialize, Serialize};

/// Everything assembly needs, provided by the caller.
///
/// The context-assembly service never reads `AppState`: whoever
/// dispatches a turn snapshots the session state it can see into this
/// struct and sends it with the [`AssembleContext`] message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyInputs {
    /// The session being assembled for.
    pub session_id: SessionId,
    /// The session's working directory (rendered into the env block).
    pub cwd: MarkedPathBuf<Absolute>,
    /// The resolved persona payload (`None` renders no persona section).
    pub persona: Option<Persona>,
    /// The session's full history (pins included; partitioned here).
    pub history: Vec<ChatEntry>,
    /// Merged (global + session-override) tool definitions, unfiltered.
    pub tools: Vec<ToolDefinition>,
    /// Which tools the session may use. Its own gate, applied here and again
    /// at dispatch — both read the same filter so they cannot disagree.
    /// `None` means the session has no filter and inherits.
    pub tool_filter: Option<NameFilter>,
    /// The provider the request will go to (for server-tool filtering).
    pub provider_name: String,
    /// Discovered skills, unfiltered.
    pub skills: Vec<Skill>,
    /// Which skills the session may load, by the same one predicate. `None`
    /// means the session has no filter and inherits.
    pub skill_filter: Option<NameFilter>,
    /// Skill names whose bodies are loaded.
    pub loaded_skills: HashSet<String>,
    /// Discovered context files (rendered into the env block).
    pub context_files: Vec<ContextFile>,
    /// Whether handoff compaction is enabled for this session. `true`
    /// appends the resume-instructions section to the system prompt —
    /// the handoff resume instructions: how to continue from a
    /// handoff recap and what a recap must carry.
    #[serde(default)]
    pub handoff_enabled: bool,
    /// The static system-prompt section (`_static.md`), discovered with
    /// the session's resources. `None` renders no section. The serde
    /// default keeps older serialized inputs deserializing.
    #[serde(default)]
    pub static_section: Option<String>,
}

/// Ask message: assemble a prompt from these inputs.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Assemble a system prompt + messages from caller-provided inputs.")]
pub struct AssembleContext {
    pub inputs: AssemblyInputs,
}

/// Reply: the assembled prompt (deserialized from the trouper reply
/// payload). `AssembledPrompt` is the shared type in `jinn-slices`.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The assembled prompt reply from the context-assembly service.")]
pub struct AssembledResponse {
    pub session_id: SessionId,
    pub prompt: AssembledPrompt,
}

/// Emitted when a chat entry's context override is toggled (e.g. via the `x` keybind).
///
/// The intent handler emits this after toggling an entry's inclusion in
/// the LLM context. The `ContextSizeActor` subscribes to this event to
/// recalculate the context size for the status bar.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A chat entry's context override changed.")]
pub struct ContextOverrideChanged {
    /// The session whose entry was toggled.
    pub session_id: SessionId,
    /// The entry whose context override changed.
    pub entry_id: jinn_core_types::ChatEntryId,
}

impl jinn_slices::BusMessage for ContextOverrideChanged {}

#[cfg(test)]
mod tests {
    use super::*;
    use trouper::schema::{Schema, SchemaKind};

    #[rstest::rstest]
    #[test]
    fn crossing_schema_contract_is_stable() {
        // Given the canonical context-assembly crossing contracts.
        // When reading their schema definitions.
        let command = <AssembleContext as Schema>::schema_def();
        let reply = <AssembledResponse as Schema>::schema_def();
        let event = <ContextOverrideChanged as Schema>::schema_def();

        // Then their names and command/event kinds are stable.
        assert_eq!(command.name, "AssembleContext");
        assert!(matches!(command.kind, SchemaKind::Command));
        assert_eq!(reply.name, "AssembledResponse");
        assert!(matches!(reply.kind, SchemaKind::Event));
        assert_eq!(event.name, "ContextOverrideChanged");
        assert!(matches!(event.kind, SchemaKind::Event));
    }
}
