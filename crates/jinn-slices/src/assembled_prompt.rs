//! Assembled prompt types shared between kernel actors and the
//! context-assembly slice.

use jinn_core_types::SessionId;
use jinn_core_types::ToolDefinition;
use jinn_provider::LlmMessage;

/// The assembled system prompt for one LLM request.
///
/// A newtype over `Option<String>`: `None` when the assembly produced no
/// system content at all. Renders as an empty string when absent so
/// `to_string()` is always safe.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemPrompt(Option<String>);

impl SystemPrompt {
    /// Wraps prompt content. An empty string becomes [`None`].
    #[must_use]
    pub fn new(content: String) -> Self {
        Self(if content.is_empty() {
            None
        } else {
            Some(content)
        })
    }

    /// The prompt content, if any.
    #[must_use]
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl std::fmt::Display for SystemPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_deref().unwrap_or(""))
    }
}

/// Fully assembled LLM prompt - everything a provider needs to make a request.
///
/// Produced by [`assemble_prompt`]. Token count is computed at construction time
/// via the provided [`TokenCounter`]. Contains messages, tool definitions,
/// and the estimated token count.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssembledPrompt {
    /// The session this prompt was assembled for.
    pub session_id: SessionId,
    /// The assembled system prompt, separate from the conversation messages.
    pub system_prompt: SystemPrompt,
    /// The assembled conversation messages ready for the LLM. Contains no
    /// system-level content; pins ride in conversation order.
    pub messages: Vec<LlmMessage>,
    /// Tool definitions to include in the API request.
    pub tool_definitions: Vec<ToolDefinition>,
    /// Estimated token count (tiktoken o200k_base) of the system prompt,
    /// all messages, and the shipped tool schemas.
    pub estimated_tokens: u32,
    /// The constant floor's share of [`Self::estimated_tokens`], computed in
    /// the same counting pass: system-prompt text (which carries the skills
    /// block) plus the shipped tool-schema tokens. Consumers subtracting it
    /// from the total get the request's work size — the conversation
    /// content alone — without recounting.
    #[serde(default)]
    pub system_tokens: u32,
}

impl AssembledPrompt {
    /// Returns the estimated token count of this assembled prompt.
    #[must_use]
    pub fn estimated_tokens(&self) -> u32 {
        self.estimated_tokens
    }

    /// The system prompt's token share of the estimate.
    #[must_use]
    pub fn system_tokens(&self) -> u32 {
        self.system_tokens
    }
}
