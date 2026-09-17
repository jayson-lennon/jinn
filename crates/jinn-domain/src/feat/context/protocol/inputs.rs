//! Crossing contracts for the context-assembly service.
//!
//! The kernel defines [`AssemblyInputs`] (the caller-provided snapshot)
//! and the ask/reply message types; the context-assembly slice consumes
//! them. Kernel callers never name slice types.

use std::collections::HashSet;
use std::path::PathBuf;

use jinn_core_types::SessionId;

use crate::feat::context::env_context::ContextFile;
use crate::feat::persona::Persona;
use crate::feat::skills::Skill;
use crate::protocol::ChatEntry;
use jinn_core_types::ToolDefinition;

/// Everything assembly needs, provided by the caller.
///
/// The context-assembly service never reads `AppState`: whoever
/// dispatches a turn snapshots the session state it can see into this
/// struct and sends it with the [`crate::AssembleContext`] message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssemblyInputs {
    /// The session being assembled for.
    pub session_id: SessionId,
    /// The session's working directory (rendered into the env block).
    pub cwd: PathBuf,
    /// The resolved persona payload (`None` renders no persona section).
    pub persona: Option<Persona>,
    /// The session's full history (pins included; partitioned here).
    pub history: Vec<ChatEntry>,
    /// Merged (global + session-override) tool definitions, unfiltered.
    pub tools: Vec<ToolDefinition>,
    /// Tool names the session disabled.
    pub disabled_tools: HashSet<String>,
    /// The provider the request will go to (for server-tool filtering).
    pub provider_name: String,
    /// Discovered skills, unfiltered.
    pub skills: Vec<Skill>,
    /// Skill names the session disabled.
    pub disabled_skills: HashSet<String>,
    /// Skill names whose bodies are loaded.
    pub loaded_skills: HashSet<String>,
    /// Discovered context files (rendered into the env block).
    pub context_files: Vec<ContextFile>,
}

/// Ask message: assemble a prompt from these inputs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssembleContext {
    pub inputs: AssemblyInputs,
}

/// Reply: the assembled prompt (deserialized from the trouper reply
/// payload). `AssembledPrompt` is the shared type in `jinn-slices`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssembledResponse {
    pub session_id: SessionId,
    pub prompt: crate::AssembledPrompt,
}

jinn_slices::crossing_schema!(AssembleContext, "AssembleContext",
trouper::schema::SchemaKind::Command,
description: "Assemble a system prompt + messages from caller-provided inputs.",
fields: [
    "inputs" => trouper::schema::FieldTy::Json,
]);

jinn_slices::crossing_schema!(AssembledResponse, "AssembledResponse",
trouper::schema::SchemaKind::Event,
description: "The assembled prompt reply from the context-assembly service.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "prompt" => trouper::schema::FieldTy::Json,
]);
