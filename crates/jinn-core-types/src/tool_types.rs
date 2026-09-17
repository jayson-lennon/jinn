//! Tool calling types - definitions, calls, and results.
//!
//! Layer-0 vocabulary: the payload nouns shared by provider-selection
//! (which builds requests from them), the tools family (which executes
//! them), and the TUI (which renders them).

use serde::{Deserialize, Serialize};

/// Where a tool result's session entry should be pinned in the assembled prompt.
///
/// Mirrors `jinn_domain::session::chat_entry::PinPosition`; duplicated here so
/// provider-side code can express pinning without depending on `jinn-domain`. The
/// session actor converts to the domain type at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultPinPosition {
    /// Always appear at the very beginning of the assembled prompt.
    Top,
    /// Always appear just before the most recent message.
    Bottom,
    /// Stay at this entry's original position in history.
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TruncatedBy {
    /// The line limit was exceeded.
    Lines,
    /// The byte limit was exceeded.
    Bytes,
}

/// Metadata about a truncation operation on tool output.
///
/// Carried in [`ToolResult`] when the tool's output exceeded the configured
/// limits. The `content` field holds the truncated text; `full_content`
/// holds the original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncationMeta {
    /// Which limit was hit.
    pub truncated_by: TruncatedBy,
    /// Total lines in the original content.
    pub total_lines: usize,
    /// Total bytes in the original content.
    pub total_bytes: usize,
    /// Number of complete lines in the truncated output.
    pub output_lines: usize,
    /// Number of bytes in the truncated output.
    pub output_bytes: usize,
}

/// Server-side tool types handled by the provider (not by jinn).
///
/// These tools are included in API requests but never dispatched locally.
/// The provider handles execution and returns results inline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServerToolType {
    OpenrouterWebSearch,
}

impl ServerToolType {
    /// Returns the provider-specific tool type string for API requests.
    pub fn as_str(&self) -> &str {
        match self {
            Self::OpenrouterWebSearch => "openrouter:web_search",
        }
    }

    /// Whether a provider with the given name can execute this server tool.
    ///
    /// The provider name is the prefix of the active model string
    /// (`{provider_name}/{model}`, see `ModelSelection::provider_name`). Only
    /// OpenRouter can run OpenRouter server tools.
    #[must_use]
    pub fn supports_provider(&self, provider_name: &str) -> bool {
        match self {
            Self::OpenrouterWebSearch => provider_name == "openrouter",
        }
    }
}

/// A tool definition that describes a tool the LLM can invoke.
///
/// Actors register these at startup via `RegisterTools`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    /// The unique name of the tool (e.g., "`file_read`").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub parameters: serde_json::Value,
    /// A one-line summary shown in the "Available tools" section of the system prompt.
    #[serde(default)]
    pub prompt_snippet: Option<String>,
    /// Behavioral guidelines injected into the "Tool guidelines" section of the system prompt.
    #[serde(default)]
    pub prompt_guidelines: Vec<String>,
    /// For server-side tools, the provider tool type. `None` for function tools.
    #[serde(default)]
    pub server_tool_type: Option<ServerToolType>,
}

impl ToolDefinition {
    /// Whether this tool is usable when the active provider is `provider_name`.
    ///
    /// Plain function tools are always available. Server tools delegate to
    /// their [`ServerToolType`], which knows which providers can execute them.
    ///
    /// `provider_name` is the prefix of the model string (e.g. `"openrouter"`
    /// from `openrouter/openai/gpt-oss-120b`). The caller is expected to derive it
    /// from the session's active model (in `jinn-domain`, that is
    /// `ModelSelection::provider_name`).
    #[must_use]
    pub fn available_for_provider(&self, provider_name: &str) -> bool {
        match &self.server_tool_type {
            None => true,
            Some(server_type) => server_type.supports_provider(provider_name),
        }
    }
}

/// A tool call requested by the LLM during a streaming response.
///
/// Contains the function name and JSON arguments the LLM wants to invoke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    /// Unique identifier for this tool call (assigned by the LLM provider).
    pub id: String,
    /// The name of the function to call.
    pub name: String,
    /// The arguments as a JSON string.
    pub arguments: String,
}

/// The result of executing a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// The ID of the tool call this result is for.
    pub tool_call_id: String,
    /// The name of the tool that was executed.
    pub name: String,
    /// The output content (truncated if output exceeded limits).
    pub content: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Original untruncated output. `Some(...)` only when truncation occurred.
    #[serde(default)]
    pub full_content: Option<String>,
    /// Truncation metadata. `Some(...)` only when truncation occurred.
    #[serde(default)]
    pub truncation: Option<TruncationMeta>,
    /// If set, the session actor pins the resulting `ChatEntryKind::ToolResult`
    /// entry at this position when it is pushed/finalized. `None` (default)
    /// leaves the entry in normal working history.
    #[serde(default)]
    pub pin_position: Option<ToolResultPinPosition>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn tool_definition_roundtrips_through_serde() {
        let def = ToolDefinition {
            name: "file_read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            prompt_snippet: Some("Read file contents".to_owned()),
            prompt_guidelines: vec!["Use read to examine files.".to_owned()],
            server_tool_type: None,
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let back: ToolDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, def);
    }

    #[rstest::rstest]
    fn tool_definition_deserializes_without_new_fields() {
        // Given a JSON ToolDefinition without prompt_snippet or prompt_guidelines.
        let json = r#"{"name":"file_read","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}"#;

        // When deserializing.
        let def: ToolDefinition = serde_json::from_str(json).expect("deserialize");

        // Then the new fields default to None / empty.
        assert_eq!(def.name, "file_read");
        assert_eq!(def.prompt_snippet, None);
        assert!(def.prompt_guidelines.is_empty());
    }

    #[rstest::rstest]
    fn tool_call_roundtrips_through_serde() {
        let call = ToolCall {
            id: "call_123".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"input":"hi"}"#.to_owned(),
        };
        let json = serde_json::to_string(&call).expect("serialize");
        let back: ToolCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, call);
    }

    #[rstest::rstest]
    fn tool_result_roundtrips_through_serde() {
        let result = ToolResult {
            tool_call_id: "call_123".to_owned(),
            name: "echo".to_owned(),
            content: "hi".to_owned(),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: ToolResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, result);
    }

    #[rstest::rstest]
    fn tool_result_with_truncation_roundtrips() {
        let result = ToolResult {
            tool_call_id: "call_456".to_owned(),
            name: "bash".to_owned(),
            content: "last line".to_owned(),
            success: true,
            full_content: Some("first line\nlast line".to_owned()),
            truncation: Some(TruncationMeta {
                truncated_by: TruncatedBy::Lines,
                total_lines: 2,
                total_bytes: 19,
                output_lines: 1,
                output_bytes: 9,
            }),
            pin_position: None,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: ToolResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, result);
    }

    #[rstest::rstest]
    fn tool_result_deserializes_without_new_fields() {
        // Given JSON without full_content or truncation (pre-existing data).
        let json = r#"{"tool_call_id":"call_1","name":"bash","content":"ok","success":true}"#;

        // When deserializing.
        let result: ToolResult = serde_json::from_str(json).expect("deserialize");

        // Then the new fields default to None.
        assert_eq!(result.tool_call_id, "call_1");
        assert_eq!(result.content, "ok");
        assert!(result.full_content.is_none());
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    fn server_tool_type_as_str_returns_correct_string() {
        assert_eq!(
            ServerToolType::OpenrouterWebSearch.as_str(),
            "openrouter:web_search"
        );
    }

    #[rstest::rstest]
    fn tool_definition_with_server_tool_type_roundtrips() {
        let def = ToolDefinition {
            name: "openrouter:web_search".to_owned(),
            description: "Search the web".to_owned(),
            parameters: serde_json::json!({"engine": "exa"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let back: ToolDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, def);
        assert_eq!(
            back.server_tool_type,
            Some(ServerToolType::OpenrouterWebSearch)
        );
    }

    #[rstest::rstest]
    fn tool_definition_without_server_tool_type_deserializes_as_none() {
        // Given JSON without server_tool_type (pre-existing data).
        let json = r#"{"name":"file_read","description":"Read a file","parameters":{"type":"object"},"prompt_snippet":null,"prompt_guidelines":[]}"#;

        // When deserializing.
        let def: ToolDefinition = serde_json::from_str(json).expect("deserialize");

        // Then server_tool_type defaults to None.
        assert_eq!(def.name, "file_read");
        assert_eq!(def.server_tool_type, None);
    }

    #[rstest::rstest]
    fn supports_provider_true_for_openrouter() {
        // Given the OpenRouter web search server tool.
        let tool = ServerToolType::OpenrouterWebSearch;

        // Then it is supported when the provider is "openrouter".
        assert!(tool.supports_provider("openrouter"));
    }

    #[rstest::rstest]
    fn supports_provider_false_for_non_openrouter() {
        // Given the OpenRouter web search server tool.
        let tool = ServerToolType::OpenrouterWebSearch;

        // Then it is NOT supported for other providers.
        assert!(!tool.supports_provider("zai"));
        assert!(!tool.supports_provider("ollama"));
        assert!(!tool.supports_provider(""));
    }

    fn function_tool() -> ToolDefinition {
        ToolDefinition {
            name: "file_read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        }
    }

    fn web_search_tool() -> ToolDefinition {
        ToolDefinition {
            name: "openrouter:web_search".to_owned(),
            description: "Search the web".to_owned(),
            parameters: serde_json::json!({}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
        }
    }

    #[rstest::rstest]
    fn available_for_provider_true_for_function_tool_on_any_provider() {
        // Given a plain function tool.
        let def = function_tool();

        // Then it is available regardless of provider.
        assert!(def.available_for_provider("zai"));
        assert!(def.available_for_provider("openrouter"));
        assert!(def.available_for_provider(""));
    }

    #[rstest::rstest]
    fn available_for_provider_true_for_web_search_on_openrouter() {
        // Given the OpenRouter web search tool.
        let def = web_search_tool();

        // Then it is available on the openrouter provider.
        assert!(def.available_for_provider("openrouter"));
    }

    #[rstest::rstest]
    fn available_for_provider_false_for_web_search_on_non_openrouter() {
        // Given the OpenRouter web search tool.
        let def = web_search_tool();

        // Then it is NOT available on a non-openrouter provider.
        assert!(!def.available_for_provider("zai"));
        assert!(!def.available_for_provider("ollama"));
        assert!(!def.available_for_provider(""));
    }
}
