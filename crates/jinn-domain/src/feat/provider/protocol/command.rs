//! Provider commands.

use std::path::PathBuf;

use crate::common::bus::BusMessage;
use serde::{Deserialize, Serialize};

use crate::feat::provider::llm_message::LlmMessage;
use jinn_slices::SystemPrompt;

use crate::protocol::SessionId;
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::tool_types::ToolDefinition;

use jiff::Timestamp;

/// Switch the active LLM provider.
///
/// Carries the target provider ID. The handler validates it against the registry,
/// swaps the factory, and emits [`ProviderSwitched`](super::ProviderSwitched).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSwitch {
    /// The session to switch provider for.
    pub session_id: SessionId,
    /// The model selection to switch to.
    pub provider_id: ModelSelection,
}

impl BusMessage for ProviderSwitch {}

/// Send a message to the AI provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessage {
    /// The session this message belongs to.
    pub session_id: SessionId,
    /// The message text.
    pub text: String,
}

/// Cancel the active provider stream for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelStream {
    /// The session whose stream should be cancelled.
    pub session_id: SessionId,
}
impl BusMessage for CancelStream {}

/// Command to send conversation context to the LLM provider.
///
/// Emitted by `LlmRequestHandler` when a user message is submitted.
/// Carries the assembled system prompt and the conversation history as
/// pre-converted messages; the message array never contains system content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendToLlmProvider {
    /// The session this request belongs to.
    pub session_id: SessionId,
    /// The full conversation history, converted to LLM messages.
    pub messages: Vec<LlmMessage>,
    /// The assembled system prompt for this request.
    #[serde(default)]
    pub system_prompt: SystemPrompt,
    /// Tool definitions available for the LLM to call.
    #[serde(default)]
    pub tool_definitions: Vec<ToolDefinition>,
    /// Optional provider override for per-message routing (future).
    /// Currently always `None` - uses the active provider.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Estimated token count of all messages + tool schemas.
    #[serde(default)]
    pub estimated_tokens: u32,
    /// The concrete model ID that will handle this request.
    /// Set by the dispatch layer after resolving alloys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    /// Resolved reasoning effort for this request (session override
    /// merged with the global default). `None` means send no effort
    /// field (provider default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<jinn_provider::ReasoningEffort>,
    /// Pinned OpenRouter routing endpoint tag, for prefix-cache affinity.
    ///
    /// Populated only for a `Single` model whose profile has an endpoint pin;
    /// the dispatch layer leaves this `None` for alloys (the pin is
    /// model-specific and incoherent across a rotating set). The factory gates
    /// this further: it is only injected when the resolved backend is
    /// OpenRouter. Legacy sessions without a pin deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_tag: Option<String>,
    /// When this request was dispatched to the LLM.
    pub dispatched_at: Timestamp,
    /// Whether this request was user-initiated or an automatic tool-loop
    /// continuation. The LLM actor drops `ToolContinuation` requests while a
    /// session is tombstoned by a cancel.
    #[serde(default)]
    pub origin: StreamOrigin,
}

impl crate::common::bus::BusMessage for SendToLlmProvider {}

/// Where an LLM request originated, from the tool loop's perspective.
///
/// The LLM actor uses this to enforce the cancel tombstone: after
/// [`CancelStream`], further `ToolContinuation` requests are dropped until the
/// next `User` request clears it. This closes the race where a cancel lands
/// while a tool-loop continuation is already in flight — without the gate, the
/// continuation re-dispatches a stream the user (or the tool-call watchdog)
/// just cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StreamOrigin {
    /// A user-initiated turn (submitted message, queue drain, retry).
    #[default]
    User,
    /// The automatic continuation dispatched after a tool batch completes.
    ToolContinuation,
}

impl crate::common::bus::BusMessage for StreamOrigin {}

/// Refresh the model list from all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshModels;
impl BusMessage for RefreshModels {}

/// Rescan prompt templates for a specific session.
///
/// Carries the session's cwd: the worker scans user/system plus project-local
/// `.agents/prompts` dirs (most-local wins), and emits `PromptTemplatesLoaded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanPromptTemplates {
    /// The session whose scan this is.
    pub session_id: crate::SessionId,
    /// The working directory driving the scan.
    #[serde(default)]
    pub cwd: PathBuf,
}
impl BusMessage for RescanPromptTemplates {}

jinn_slices::crossing_schema!(RescanPromptTemplates, "RescanPromptTemplates",
trouper::schema::SchemaKind::Command,
description: "Rescan prompt templates for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "cwd" => trouper::schema::FieldTy::Str,
]);

/// Load entries for the provider/model picker.
///
/// The provider actor receives this, loads entries from the provider registry,
/// and writes them into `AppState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadProviderPickerEntries;

impl BusMessage for LoadProviderPickerEntries {}

/// The provider actor receives this, resolves the active session's model
/// backend, and either fetches the model's OpenRouter routing endpoints via
/// `list_endpoints` or — for a non-OpenRouter backend — populates a single
/// explanatory "not served via OpenRouter" row. The entries are written into
/// `AppState`'s endpoint picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadEndpointPickerEntries;

impl BusMessage for LoadEndpointPickerEntries {}

/// Force-refresh the OpenRouter endpoint picker entries for the active model,
/// bypassing the in-memory cache (used by the `<c-r>` keybind).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshEndpointPickerEntries;

impl BusMessage for RefreshEndpointPickerEntries {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn send_to_llm_provider_deserializes_without_provider_id() {
        // Given JSON without the provider_id field (old format).
        let json = r#"{"session_id":"00000000-0000-0000-0000-000000000001","messages":[],"dispatched_at":"2024-01-01T00:00:00Z"}"#;

        // When deserializing.
        let cmd: SendToLlmProvider = serde_json::from_str(json).expect("deserialize");

        // Then provider_id is None (backwards compatible).
        assert!(cmd.provider_id.is_none());
    }
}
