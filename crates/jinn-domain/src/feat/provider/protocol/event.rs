//! Provider events.

use serde::{Deserialize, Serialize};

use crate::feat::context::protocol::prompt_template::PromptTemplate;
use crate::protocol::SessionId;
use jinn_core_types::tool_types::ToolCall;

use jiff::Timestamp;

/// Why the stream completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCompletedReason {
    /// The stream finished normally (all tokens received).
    Finished,
    /// The stream was cancelled by the user.
    Canceled,
    /// The stream stopped because the model requested tool use.
    ToolUse,
    /// The stream failed due to a provider error.
    Error,
}

/// Streaming response completed for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCompleted {
    /// The session whose stream completed.
    pub session_id: SessionId,
    /// Why the stream completed.
    pub reason: StreamCompletedReason,
    /// Accumulated text content from the assistant response (populated when reason is `ToolUse`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_content: Option<String>,
    /// Tool calls requested by the assistant (populated when reason is `ToolUse`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Cost in USD reported by the provider for this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Provider-reported completion token count (includes thinking/reasoning tokens).
    ///
    /// When present, this is used directly as `tokens_received` instead of local
    /// counting, because it matches the provider's billing exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_completion_tokens: Option<u64>,
    /// Provider-reported prompt token count for this request.
    ///
    /// When present, the session ledger records this alongside the pre-send
    /// local estimate (`tokens_sent`); the estimate is never overwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_prompt_tokens: Option<u64>,
    /// Provider-reported count of prompt tokens served from a cache
    /// (`usage.prompt_tokens_details.cached_tokens`).
    ///
    /// OpenAI-compat only (e.g. OpenRouter); `None` when the provider did not
    /// report cache details (e.g. a cancelled turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    /// Accumulated thinking/reasoning content for local token counting fallback.
    ///
    /// Populated when the stream produced reasoning tokens and the provider did
    /// not report `completion_tokens`. Used by the local counter to include
    /// thinking tokens in `tokens_received`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_content: Option<String>,
    /// The concrete model ID that handled this request.
    ///
    /// For alloys, this is the resolved model that was picked for this particular
    /// request. For single-model sessions, this is the model itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
}

impl crate::common::bus::BusMessage for StreamCompleted {}

/// A single token from a streaming LLM response.
///
/// Emitted by the LLM actor during streaming. Handlers append
/// the token to the active session's assistant entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamToken {
    /// The session this token belongs to.
    pub session_id: SessionId,
    /// The zero-based index of this token in the stream.
    pub index: usize,
    /// The token text.
    pub token: String,
    /// Whether this token contains reasoning/thinking content.
    ///
    /// When `true`, the session actor routes this token to the `Thinking`
    /// chat entry instead of the `Assistant` entry.
    #[serde(default)]
    pub is_thinking: bool,
    /// When the original LLM request was dispatched.
    pub dispatched_at: Timestamp,
}

impl crate::common::bus::BusMessage for StreamToken {}

/// The active provider was switched.
///
/// Emitted after a successful [`ProviderSwitch`](super::ProviderSwitch) command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSwitched {
    /// The session that switched provider.
    pub session_id: SessionId,
    /// The display name of the new provider.
    pub provider_name: String,
}

impl crate::common::bus::BusMessage for ProviderSwitched {}

/// Models refresh completed with results and errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsRefreshed {
    /// The session that triggered the refresh (for routing the result back).
    pub session_id: SessionId,
    /// Provider name to list of discovered model metadata.
    pub results: std::collections::HashMap<String, Vec<jinn_provider::ModelInfo>>,
    /// Provider name to error message for providers that failed.
    pub errors: std::collections::HashMap<String, String>,
}

impl crate::common::bus::BusMessage for ModelsRefreshed {}

/// Model cache loaded from disk at startup.
///
/// Emitted by `ProviderInitActor` after loading the cache from disk.
/// `ProviderActor` handles this by writing the cache into AppState and
/// reloading picker entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheLoaded {
    /// The loaded model cache.
    pub cache: crate::feat::provider_infra::ModelCache,
}

impl crate::common::bus::BusMessage for ModelCacheLoaded {}

/// Prompt templates loaded after a rescan.
///
/// Emitted by the prompt scan actor after scanning the prompts directory.
/// On success, `templates` contains the loaded templates and `error` is `None`.
/// On failure, `templates` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplatesLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: crate::SessionId,
    /// The loaded prompt templates.
    pub templates: Vec<PromptTemplate>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl crate::common::bus::BusMessage for PromptTemplatesLoaded {}

jinn_slices::crossing_schema!(PromptTemplatesLoaded, "PromptTemplatesLoaded",
trouper::schema::SchemaKind::Event,
description: "Prompt templates loaded after a rescan.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "templates" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json)),
]);
