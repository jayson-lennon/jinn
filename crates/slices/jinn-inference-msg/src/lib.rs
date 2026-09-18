//! Inference crossing contracts.
//!
//! The EXPORT surface of the inference slice: the dispatch commands that
//! start or cancel a provider stream and the stream events the actor
//! emits while driving one. Owned by the producing slice (§1 rule 5 —
//! remove the inference actor and these messages have no producer).
//!
//! Kernel publishers of the commands: the queue actor's dispatch bodies
//! (user turns, tool-loop continuations, resume turns) and the stall
//! re-dispatch; the intent handler's cancel arm and the plugin
//! coordinator's mirrored cancel request. Kernel consumers of the
//! events: the session actor's stream/tool folds and the plugin
//! coordinator's stream mirror (string-based wire).
//!
//! The stream-phase tool events (`ToolUseStarted`/`ToolCallReceived`/
//! `ToolCallStreaming`) are *also* published by the inference actor but
//! live in `jinn-tools-msg` — an accepted divergence (see
//! actor-migration/slices.md §4), avoided here to keep the tools crate
//! from depending on this one.

use jinn_core_types::SessionId;
use jinn_core_types::llm_message::LlmMessage;
use jinn_core_types::tool_types::ToolCall;
use jinn_core_types::tool_types::ToolDefinition;
use jinn_slices::SystemPrompt;
use serde::{Deserialize, Serialize};

/// Cancel the active provider stream for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelStream {
    /// The session whose stream should be cancelled.
    pub session_id: SessionId,
}
impl jinn_slices::BusMessage for CancelStream {}

jinn_slices::crossing_schema!(CancelStream, "CancelStream",
trouper::schema::SchemaKind::Command,
description: "Cancel the active provider stream for a session.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);

/// Command to send conversation context to the LLM provider.
///
/// Emitted by the dispatch layer when a turn becomes sendable.
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
    pub reasoning_effort: Option<jinn_core_types::ReasoningEffort>,
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
    pub dispatched_at: jiff::Timestamp,
    /// Whether this request was user-initiated or an automatic tool-loop
    /// continuation. The inference actor drops `ToolContinuation` requests
    /// while a session is tombstoned by a cancel.
    #[serde(default)]
    pub origin: StreamOrigin,
}

impl jinn_slices::BusMessage for SendToLlmProvider {}

jinn_slices::crossing_schema!(SendToLlmProvider, "SendToLlmProvider",
trouper::schema::SchemaKind::Command,
description: "Send assembled conversation context to the LLM for streaming.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "messages" => trouper::schema::FieldTy::Json,
    "system_prompt" => trouper::schema::FieldTy::Json,
    "tool_definitions" => trouper::schema::FieldTy::Json,
    "provider_id" => trouper::schema::FieldTy::Json,
    "estimated_tokens" => trouper::schema::FieldTy::Json,
    "model_used" => trouper::schema::FieldTy::Json,
    "reasoning_effort" => trouper::schema::FieldTy::Json,
    "endpoint_tag" => trouper::schema::FieldTy::Json,
    "dispatched_at" => trouper::schema::FieldTy::Json,
    "origin" => trouper::schema::FieldTy::Json
]);

/// Where an LLM request originated, from the tool loop's perspective.
///
/// The inference actor uses this to enforce the cancel tombstone: after
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

impl jinn_slices::BusMessage for StreamOrigin {}

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
    pub dispatched_at: jiff::Timestamp,
}

impl jinn_slices::BusMessage for StreamCompleted {}

jinn_slices::crossing_schema!(StreamCompleted, "StreamCompleted",
trouper::schema::SchemaKind::Event,
description: "A provider stream reached its terminal boundary for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "reason" => trouper::schema::FieldTy::Json,
    "assistant_content" => trouper::schema::FieldTy::Json,
    "tool_calls" => trouper::schema::FieldTy::Json,
    "cost" => trouper::schema::FieldTy::Json,
    "provider_completion_tokens" => trouper::schema::FieldTy::Json,
    "provider_prompt_tokens" => trouper::schema::FieldTy::Json,
    "cached_tokens" => trouper::schema::FieldTy::Json,
    "thinking_content" => trouper::schema::FieldTy::Json,
    "model_used" => trouper::schema::FieldTy::Json,
    "dispatched_at" => trouper::schema::FieldTy::Json
]);

/// A single token from a streaming LLM response.
///
/// Emitted by the inference actor during streaming. Handlers append
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
    pub dispatched_at: jiff::Timestamp,
}

impl jinn_slices::BusMessage for StreamToken {}

jinn_slices::crossing_schema!(StreamToken, "StreamToken",
trouper::schema::SchemaKind::Event,
description: "A fragment of streamed text or thinking content from the LLM.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "index" => trouper::schema::FieldTy::Json,
    "token" => trouper::schema::FieldTy::Json,
    "is_thinking" => trouper::schema::FieldTy::Json,
    "dispatched_at" => trouper::schema::FieldTy::Json
]);

/// The trouper topic the inference slice's crossing messages travel on.
#[must_use]
pub fn inference_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.inference")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn send_to_llm_provider_roundtrips_through_json() {
        // Given a minimal SendToLlmProvider built via its serde shape.
        let original = SendToLlmProvider {
            session_id: SessionId::new(),
            messages: vec![LlmMessage::User {
                content: "hello".to_owned(),
                attachments: vec![],
            }],
            system_prompt: SystemPrompt::new("system".to_owned()),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 10,
            model_used: Some("m".to_owned()),
            reasoning_effort: None,
            endpoint_tag: None,
            dispatched_at: jiff::Timestamp::from_second(0).unwrap(),
            origin: StreamOrigin::User,
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&original).expect("serialize");
        let back: SendToLlmProvider = serde_json::from_str(&json).expect("deserialize");

        // Then it roundtrips.
        assert_eq!(back.session_id, original.session_id);
        assert_eq!(back.estimated_tokens, 10);
        assert_eq!(back.origin, StreamOrigin::User);
    }

    #[rstest::rstest]
    #[test]
    fn stream_completed_reason_serializes_snake_case() {
        // Given each completion reason.
        // When serializing.
        // Then the wire form is snake_case.
        let json = serde_json::to_string(&StreamCompletedReason::ToolUse).expect("serialize");
        assert_eq!(json, r#""tool_use""#);
        let json = serde_json::to_string(&StreamCompletedReason::Canceled).expect("serialize");
        assert_eq!(json, r#""canceled""#);
    }

    #[rstest::rstest]
    #[test]
    fn stream_completed_defaults_optional_fields_on_deserialize() {
        // Given JSON for a StreamCompleted carrying only required fields.
        let json = format!(
            r#"{{"session_id":"{}","reason":"finished","dispatched_at":"1970-01-01T00:00:00Z"}}"#,
            SessionId::new()
        );

        // When deserializing.
        let back: StreamCompleted = serde_json::from_str(&json).expect("deserialize");

        // Then the optional fields default to None.
        assert_eq!(back.reason, StreamCompletedReason::Finished);
        assert!(back.assistant_content.is_none());
        assert!(back.tool_calls.is_none());
        assert!(back.cost.is_none());
        assert!(back.model_used.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn schemas_are_declared_for_every_crossing_message() {
        // Given the crossing messages.
        // When reading their schema ids.
        // Then each has a schema (compile-time proof of the impls).
        let _ = <CancelStream as trouper::schema::Schema>::schema_id();
        let _ = <SendToLlmProvider as trouper::schema::Schema>::schema_id();
        let _ = <StreamCompleted as trouper::schema::Schema>::schema_id();
        let _ = <StreamToken as trouper::schema::Schema>::schema_id();
    }
}
