//! Tool calling events.

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

use jiff::Timestamp;

/// A single tool execution completed.
///
/// Emitted by provider actors after executing a tool. The orchestrator
/// closes the call in the turn ledger; the session actor finalizes the
/// result entry — both keyed on the event's turn identity.
///
/// A `synthesized` completion is the cancel drain's bookkeeping: the
/// orchestrator emits one per open call at cancel time so the work set
/// empties without waiting on an unabortable tool. It finalizes only a
/// still-`Pending` entry and never pushes one — a real result that also
/// arrives always wins.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A single tool execution completed.")]
pub struct ToolExecutionCompleted {
    /// The session this execution belongs to.
    pub session_id: SessionId,
    /// The tool execution result.
    pub result: ToolResult,
    /// The turn this execution belongs to, as minted by the turn ledger.
    ///
    /// Zero is the unset serde default; see `SendToLlmProvider.turn`.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
    /// Whether this completion was synthesized by the cancel drain rather
    /// than produced by the tool. Old persisted events default to `false`.
    #[serde(default)]
    pub synthesized: bool,
}

/// A batch of tool calls was dispatched for execution.
///
/// Emitted by the tools orchestrator when it accepts an
/// `ExecuteToolBatch`. This is the read-only observation point for the
/// call's arguments: the batch itself is a Command routed to one handler
/// (the orchestrator), and the completion event carries only the result,
/// so consumers that need both halves of a call — the arguments and the
/// result — join them by `ToolCallId` across this event and
/// [`ToolExecutionCompleted`].
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A batch of tool calls was dispatched for execution.")]
pub struct ToolCallsDispatched {
    /// The session the batch belongs to.
    pub session_id: SessionId,
    /// The calls, in batch order, with their arguments.
    pub tool_calls: Vec<ToolCall>,
    /// The turn this batch belongs to, as minted by the turn ledger.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
}

/// Tools were registered by an actor.
///
/// Emitted after an actor sends `RegisterTools` to confirm registration.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "An actor registered tool definitions.")]
pub struct ToolsRegistered {
    /// The name of the actor that registered tools.
    pub provider: String,
    /// The tool definitions that were registered.
    pub definitions: Vec<ToolDefinition>,
    /// `None` for global tools (broadcast to all sessions).
    /// `Some(session_id)` for attached tools (only that session should store them).
    pub session_id: Option<SessionId>,
}

/// Session-scoped tools were removed by their provider.
///
/// Emitted when a provider tears down its tool registrations for a session
/// (e.g. an `McpActor` stops on disable, session close, or restart). Tool
/// registries and context caches prune the provider's matching entries.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A provider's session-scoped tools were removed.")]
pub struct ToolsUnregistered {
    /// The name of the actor that previously registered the tools
    /// (e.g. `mcp__excalimate`).
    pub provider: String,
    /// The session the tools were registered for. Always `Some`-shaped:
    /// only session-scoped registrations are unregistered this way.
    pub session_id: SessionId,
}

/// One session's view of a globally-registered provider changed.
///
/// A singleton MCP server registers its tools globally — one shared
/// connection serves every session — so a session disabling it cannot
/// remove the registration (other sessions still use it). The MCP
/// coordinator emits this when a reconcile finds the singleton absent
/// from (`hidden = true`) or present in (`hidden = false`) a session's
/// enabled set. Tool registries withhold (or restore) that provider's
/// global tools for exactly this session.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's view of a globally-registered provider changed.")]
pub struct ProviderVisibilityChanged {
    /// The globally-registered provider (e.g. `mcp__bundled-example`).
    pub provider: String,
    /// The session whose view changed.
    pub session_id: SessionId,
    /// `true` withholds the provider's global tools from this session;
    /// `false` restores them.
    pub hidden: bool,
}

/// A tool call has started in the LLM stream (name and ID known, arguments pending).
///
/// Emitted by the LLM actor when the backend signals tool use start.
/// The chat log creates a placeholder entry for this tool call.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A tool call started in the LLM stream.")]
pub struct ToolUseStarted {
    /// The session this tool call belongs to.
    pub session_id: SessionId,
    /// The index of the tool call in the response.
    pub index: usize,
    /// The unique ID for this tool call (assigned by the LLM provider).
    pub id: jinn_core_types::ToolCallId,
    /// The name of the tool being called.
    pub name: String,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
}

/// A complete tool call received from the LLM stream.
///
/// Emitted by the LLM actor when a complete tool call arrives in the stream.
/// The chat log uses this to finalize the tool call entry.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A complete tool call was received from the stream.")]
pub struct ToolCallReceived {
    /// The session this tool call belongs to.
    pub session_id: SessionId,
    /// The assembled tool call.
    pub tool_call: ToolCall,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
}

/// Streaming update for a tool call being assembled.
///
/// Emitted by the LLM actor as tool call arguments stream in.
/// The chat log uses this to render in-progress tool call arguments.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Tool call arguments streamed in as a partial JSON delta.")]
pub struct ToolCallStreaming {
    /// The session this tool call belongs to.
    pub session_id: SessionId,
    /// The index of the tool call in the response.
    pub index: usize,
    /// Partial JSON string for the tool arguments (accumulated so far).
    pub partial_json: String,
}

/// A tool has started executing.
///
/// Emitted by the tool orchestrator when a streaming tool begins actual execution
/// (after arguments are complete). The session actor creates a pending
/// ToolResult entry. Only emitted for streaming tools (e.g., bash).
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A streaming tool began executing.")]
pub struct ToolExecutionStarted {
    /// The session this execution belongs to.
    pub session_id: SessionId,
    /// The unique ID for this tool call.
    pub tool_call_id: jinn_core_types::ToolCallId,
    /// The name of the tool being executed.
    pub name: String,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
    /// The turn this execution belongs to, as minted by the turn ledger.
    ///
    /// Zero is the unset serde default; see `SendToLlmProvider.turn`.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
}

/// How a streamed tool output should be surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ToolOutputKind {
    /// Ordinary streamed output.
    #[default]
    #[serde(rename = "normal")]
    Normal,
    /// Attention-grabbing output — e.g. a detected bot challenge the user
    /// must solve in the browser window. Rendered with alert styling.
    #[serde(rename = "alert")]
    Alert,
}

/// Incremental output from a running tool.
///
/// Emitted by streaming tools as they produce output. Each event carries
/// a delta (new lines), not the accumulated total. The session actor
/// appends to the pending ToolResult entry's content.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A streamed tool produced incremental output.")]
pub struct ToolExecutionOutput {
    /// The session this output belongs to.
    pub session_id: SessionId,
    /// The turn this output belongs to, as minted by the turn ledger.
    ///
    /// Zero is the unset serde default; see `SendToLlmProvider.turn`.
    #[serde(default)]
    pub turn: jinn_core_types::turn_identity::TurnToken,
    /// The tool call ID this output is for.
    pub tool_call_id: jinn_core_types::ToolCallId,
    /// New output text (delta, not accumulated).
    pub output: String,
    /// How the output should be surfaced. Old persisted events default to
    /// [`ToolOutputKind::Normal`].
    #[serde(default)]
    pub kind: ToolOutputKind,
}

impl jinn_slices::BusMessage for ToolExecutionCompleted {}

impl jinn_slices::BusMessage for ToolCallsDispatched {}

impl jinn_slices::BusMessage for ToolsRegistered {}

impl jinn_slices::BusMessage for ToolsUnregistered {}

impl jinn_slices::BusMessage for ProviderVisibilityChanged {}

impl jinn_slices::BusMessage for ToolUseStarted {}

impl jinn_slices::BusMessage for ToolCallReceived {}

impl jinn_slices::BusMessage for ToolCallStreaming {}

impl jinn_slices::BusMessage for ToolExecutionStarted {}

impl jinn_slices::BusMessage for ToolExecutionOutput {}
