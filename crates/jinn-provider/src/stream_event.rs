//! Stream events from LLM chat with tool support.
//!
//! [`StreamEvent`] is the unified streaming output type for LLM responses,
//! decoupled from any specific provider's stream format.

use jinn_core_types::tool_types::ToolCall;

/// Why the stream stopped.
///
/// Parsed from the provider's raw string at the conversion boundary.
/// Unknown values are preserved via [`StopReason::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model requested tool use.
    ToolUse,
    /// The model finished generating normally.
    EndTurn,
    /// An unrecognized stop reason from the provider.
    Other(String),
}

impl From<&str> for StopReason {
    fn from(s: &str) -> Self {
        match s {
            "tool_use" => Self::ToolUse,
            "end_turn" => Self::EndTurn,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolUse => write!(f, "tool_use"),
            Self::EndTurn => write!(f, "end_turn"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Usage and cost data from a provider's streaming response.
///
/// Populated when the provider reports token counts or cost in its streaming
/// response. Not all providers report all fields - `None` means the provider
/// did not include that data.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamUsage {
    /// Tokens in the prompt (provider-reported, not estimated).
    pub prompt_tokens: Option<u64>,
    /// Tokens in the completion (provider-reported, not estimated).
    pub completion_tokens: Option<u64>,
    /// Cost in USD reported by the provider (e.g. OpenRouter).
    pub cost: Option<f64>,
    /// Provider-reported count of prompt tokens served from a cache
    /// (`usage.prompt_tokens_details.cached_tokens`).
    /// OpenAI-compat only (e.g. OpenRouter); `None` for providers that
    /// don't report cache details.
    pub cached_tokens: Option<u64>,
}

/// A single web-search source citation from OpenRouter.
///
/// Carries the fields of a `url_citation` annotation attached to an assistant
/// message. `content` and the index range are frequently omitted by the
/// provider, hence `Option` with `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UrlCitation {
    /// The source URL.
    pub url: String,
    /// A human-readable title for the source.
    pub title: String,
    /// Snippet text, when the provider includes it.
    #[serde(default)]
    pub content: Option<String>,
    /// Start of the cited character span in the assistant text, if reported.
    #[serde(default)]
    pub start_index: Option<u32>,
    /// End of the cited character span in the assistant text, if reported.
    #[serde(default)]
    pub end_index: Option<u32>,
}

/// A streaming event from an LLM chat response.
///
/// Produced by [`LlmService::chat_stream_with_tools`](super::LlmService::chat_stream_with_tools).
/// The stream always ends with a [`Done`](StreamEvent::Done) event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A text content delta.
    Text(String),
    /// A reasoning/thinking content delta.
    Reasoning(String),
    /// A tool use block started (ID and name known, arguments streaming).
    ToolUseStart {
        /// The index of this content block in the response.
        index: usize,
        /// The unique ID for this tool call (assigned by the LLM provider).
        id: String,
        /// The name of the tool being called.
        name: String,
    },
    /// A partial JSON delta for tool call arguments.
    ToolUseInputDelta {
        /// The index of this content block.
        index: usize,
        /// Partial JSON string for the tool input.
        partial_json: String,
    },
    /// A tool use block completed with an assembled tool call.
    ToolUseComplete {
        /// The index of this content block.
        index: usize,
        /// The complete tool call.
        tool_call: ToolCall,
    },
    /// The stream ended.
    Done {
        /// Why the stream stopped.
        stop_reason: StopReason,
        /// Usage and cost data from the provider, if reported.
        usage: Option<StreamUsage>,
    },
    /// An error event from the provider mid-stream.
    ///
    /// Some providers (e.g., Anthropic) send error events within the SSE stream
    /// for issues like `overloaded_error`. This variant surfaces those errors
    /// so the LLM actor can display them to the user and terminate gracefully.
    Error {
        /// The error type from the provider (e.g., "overloaded_error").
        error_type: String,
        /// The human-readable error message from the provider.
        message: String,
    },
    /// Accumulated source citations from server-side tooling (e.g. the
    /// OpenRouter `openrouter:web_search` tool, surfaced as `url_citation`
    /// annotations). Emitted once, immediately before [`Done`](StreamEvent::Done),
    /// carrying every citation gathered across the stream.
    Citations(Vec<UrlCitation>),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    #[case("tool_use", StopReason::ToolUse)]
    #[case("end_turn", StopReason::EndTurn)]
    #[case("something_else", StopReason::Other("something_else".to_owned()))]
    fn from_str_parses_known_reasons(#[case] input: &str, #[case] expected: StopReason) {
        assert_eq!(StopReason::from(input), expected);
    }

    #[rstest::rstest]
    #[case(StopReason::ToolUse, "tool_use")]
    #[case(StopReason::EndTurn, "end_turn")]
    #[case(StopReason::Other("max_tokens".to_owned()), "max_tokens")]
    fn display_formats_correctly(#[case] reason: StopReason, #[case] expected: &str) {
        assert_eq!(format!("{reason}"), expected);
    }
}
