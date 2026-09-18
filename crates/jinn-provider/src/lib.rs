//! LLM provider abstraction - streaming chat completions and model discovery.
//!
//! Defines the [`LlmService`] trait for streaming LLM responses and
//! [`LlmServiceFactory`] for creating per-call service instances.
//! Includes test doubles (fake, sample, no-providers) for use in tests.

/// Installs the process-wide rustls crypto provider (ring) in this crate's
/// test binary. reqwest is built with `rustls-no-provider` (see the workspace
/// `Cargo.toml`), so without a default provider every `reqwest::Client` panics
/// with "No provider set" at construction. Test binaries never run `main()`.
/// `install_default` errors on the second call; the result is deliberately
/// ignored.
#[cfg(test)]
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

mod attachment;
mod backend;
mod fake;
mod input_modalities;
mod llm_message;
mod no_providers;
mod openai_compat;
mod retry;
mod sample;
mod service;
mod stream_event;

// Custom provider implementations (not OpenAI-compatible).
pub mod anthropic;
pub mod google;

pub use anthropic::AnthropicFactory;
pub use google::GoogleFactory;

pub use attachment::Attachment;
pub use backend::{Backend, BackendError};
pub use fake::{FakeLlmServiceFactory, ScriptedResponse, TOOL_LOOP_TRIGGER};
pub use input_modalities::{InputModalities, Modality};
pub use jinn_core_types::reasoning::ReasoningEffort;
pub use jinn_core_types::tool_types::{ServerToolType, ToolCall, ToolDefinition, ToolResult};
pub use llm_message::LlmMessage;
pub use no_providers::NoProvidersAvailableFactory;
pub use openai_compat::{
    EndpointInfo, OpenAiCompatibleFactory, OpenAiCompatibleService, ProviderConfig, list_endpoints,
    list_endpoints_default_client,
};
pub use sample::SampleLlmServiceFactory;
pub use service::{ChatStream, LlmService, LlmServiceError, LlmServiceFactory, ToolStream};
pub use stream_event::{StopReason, StreamEvent, StreamUsage, UrlCitation};

pub use retry::{NoOpOnRetry, OnRetry, RetryConfig, RetryingLlmService};

/// Rich model metadata returned by provider model listing endpoints.
///
/// Each provider's `list_models` returns a `Vec<ModelInfo>`. The `context_length`
/// is populated when the provider returns it (e.g. OpenRouter's `context_length`,
/// Anthropic's `context_window`, Google's `inputTokenLimit`). It is `None` when
/// the provider does not supply it or the field is missing/null.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    /// The model identifier (e.g. `"openai/gpt-4"`, `"claude-sonnet-4-20250514"`).
    pub id: String,
    /// Maximum context length in tokens, if the provider reports it.
    pub context_length: Option<u32>,
    /// Input modalities the model accepts. Text is always present;
    /// the image bit is stamped from models.dev during discovery.
    /// Defaults to text-only for legacy caches lacking the field.
    #[serde(default = "InputModalities::text")]
    pub input_modalities: InputModalities,
}

#[cfg(test)]
mod fake_tests;
