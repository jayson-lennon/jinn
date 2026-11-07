//! Tool calling commands.

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition};

use jiff::Timestamp;

/// Register tools that an actor can execute.
///
/// Sent by actors at startup to declare which tools they provide.
/// When `session_id` is `None`, the tools are global (visible to every
/// session); when `Some`, they are scoped to that session only (e.g.
/// per-session MCP server tools).
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Register tool definitions globally or for one session.")]
pub struct RegisterTools {
    /// The name of the actor providing these tools.
    pub provider: String,
    /// The tool definitions being registered.
    pub definitions: Vec<ToolDefinition>,
    /// The session these tools belong to. `None` registers them globally.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// Request execution of a batch of tool calls for a session.
///
/// Sent by the LLM actor when the LLM produces tool calls.
/// Routed to the tool orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Execute a batch of tool calls for a session.")]
pub struct ExecuteToolBatch {
    /// The session requesting tool execution.
    pub session_id: SessionId,
    /// The tool calls to execute.
    pub tool_calls: Vec<ToolCall>,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
    /// The turn this batch belongs to, as minted by the turn ledger.
    ///
    /// Zero is the unset serde default; see `SendToLlmProvider.turn`.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
}

/// Execute a single tool call.
///
/// Sent by the tool orchestrator to the actor that registered the tool.
/// Carries the session ID so the provider actor can include it in its
/// response event.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Execute one tool call for a session.")]
pub struct ExecuteTool {
    /// The session this execution belongs to.
    pub session_id: SessionId,
    /// The turn this execution belongs to, as minted by the turn ledger.
    ///
    /// Zero is the unset serde default; see `SendToLlmProvider.turn`.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
    /// The tool call to execute.
    pub tool_call: ToolCall,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
    /// Max output lines for truncating results. Forwarded from user
    /// preferences by the orchestrator; `None` lets the provider pick its
    /// own default. MCP tool results are truncated the same way builtins are.
    #[serde(default)]
    pub max_output_lines: Option<usize>,
    /// Max output bytes for truncating results.
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    /// The session's working directory, carried so the provider actor can
    /// resolve the reserved `dump_to_disk` path the same way the builtin
    /// path would. `None` in tests and stragglers — a dump without a cwd is
    /// skipped, never guessed.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Cancel all pending tool executions for a session.
///
/// Sent by the LLM actor when a stream is cancelled while tool results
/// are pending. Routed to the tool orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Cancel a session's in-flight tool batch.")]
pub struct CancelToolBatch {
    /// The session whose tool executions should be cancelled.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for RegisterTools {}

impl jinn_slices::BusMessage for ExecuteToolBatch {}

impl jinn_slices::BusMessage for ExecuteTool {}

impl jinn_slices::BusMessage for CancelToolBatch {}
