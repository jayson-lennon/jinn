//! Factory for creating [`OpenAiCompatibleService`] instances.
//!
//! [`OpenAiCompatibleFactory`] implements [`LlmServiceFactory`] for any
//! OpenAI-compatible backend. It stores the resolved configuration and
//! creates fresh service instances on each call.

use error_stack::Report;
use reqwest::Client;

use crate::openai_compat::provider_config::ProviderConfig;
use crate::openai_compat::service::OpenAiCompatibleService;
use crate::service::{LlmService, LlmServiceError, LlmServiceFactory};
use jinn_core_types::reasoning::ReasoningEffort;

/// Factory for OpenAI-compatible LLM services.
///
/// Stores a provider config, model, base URL override, API key, and optional
/// extra body parameters. Creates a new [`OpenAiCompatibleService`] on each
/// [`create`](LlmServiceFactory::create) call.
#[derive(Debug)]
pub struct OpenAiCompatibleFactory {
    /// Per-backend configuration.
    config: ProviderConfig,
    /// Model identifier.
    model: String,
    /// Base URL override (uses provider default if `None`).
    base_url: Option<String>,
    /// API key for authentication.
    api_key: String,
    /// Extra body fields (e.g., `enable_thinking` for ZAI).
    extra_body: Option<serde_json::Value>,
    /// Resolved reasoning effort (session-override merged).
    reasoning: Option<ReasoningEffort>,
    /// Shared HTTP client.
    client: Client,
    /// Display name for this factory instance.
    name: String,
}

impl OpenAiCompatibleFactory {
    /// Create a new factory with an internally created HTTP client.
    #[must_use]
    pub fn new(
        config: ProviderConfig,
        model: String,
        base_url: Option<String>,
        api_key: String,
        extra_body: Option<serde_json::Value>,
        reasoning: Option<ReasoningEffort>,
        name: String,
    ) -> Self {
        Self {
            config,
            model,
            base_url,
            api_key,
            extra_body,
            reasoning,
            client: Client::new(),
            name,
        }
    }

    /// Create a new factory with a shared HTTP client.
    #[must_use]
    #[expect(clippy::too_many_arguments, reason = "no big deal")]
    pub fn with_client(
        client: Client,
        config: ProviderConfig,
        model: String,
        base_url: Option<String>,
        api_key: String,
        extra_body: Option<serde_json::Value>,
        reasoning: Option<ReasoningEffort>,
        name: String,
    ) -> Self {
        Self {
            config,
            model,
            base_url,
            api_key,
            extra_body,
            reasoning,
            client,
            name,
        }
    }
}

impl LlmServiceFactory for OpenAiCompatibleFactory {
    fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
        if self.api_key.is_empty() {
            return Err(Report::new(LlmServiceError::ApiKey)
                .attach(format!("Missing {} API key", self.config.name)));
        }

        let service = OpenAiCompatibleService::with_client(
            self.client.clone(),
            self.config.clone(),
            self.model.clone(),
            self.base_url.clone(),
            self.api_key.clone(),
            self.extra_body.clone(),
            self.reasoning,
        );

        Ok(Box::new(service))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Clone for OpenAiCompatibleFactory {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            extra_body: self.extra_body.clone(),
            reasoning: self.reasoning,
            client: self.client.clone(),
            name: self.name.clone(),
        }
    }
}
