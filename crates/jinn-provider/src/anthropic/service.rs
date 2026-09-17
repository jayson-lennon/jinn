//! Anthropic streaming chat service.
//!
//! [`AnthropicService`] implements [`LlmService`] using `reqwest`
//! to stream responses from Anthropic's Messages API.

use error_stack::{Report, ResultExt as _};
use futures::StreamExt as _;
use reqwest::Client;

use crate::ModelInfo;
use crate::anthropic::models;
use crate::anthropic::request;
use crate::anthropic::response::AnthropicStreamParser;
use crate::llm_message::LlmMessage;
use crate::openai_compat::sse::{SseEvent, SseParser};
use crate::service::{ChatStream, LlmService, LlmServiceError, ToolStream};
use crate::stream_event::StreamEvent;
use jinn_core_types::tool_types::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const PROVIDER_NAME: &str = "Anthropic";

/// An LLM service that talks to Anthropic's Messages API.
///
/// The system prompt is passed per request; no prompt state lives here.
pub struct AnthropicService {
    /// HTTP client.
    client: Client,
    /// Model identifier.
    model: String,
    /// API key for authentication.
    api_key: String,
    /// Base URL override (for testing).
    base_url: String,
}

impl AnthropicService {
    /// Create a new Anthropic service instance.
    #[must_use]
    pub fn new(model: String, api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            api_key,
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Create a new Anthropic service with a custom client.
    #[must_use]
    pub fn with_client(client: reqwest::Client, model: String, api_key: String) -> Self {
        Self {
            client,
            model,
            api_key,
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Create a new Anthropic service instance with a custom base URL (for testing).
    #[must_use]
    pub fn with_base_url(model: String, api_key: String, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            api_key,
            base_url,
        }
    }

    /// Fetch available model IDs from Anthropic.
    ///
    /// # Errors
    ///
    /// Returns [`LlmServiceError::Provider`] on HTTP or parse errors.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, Report<LlmServiceError>> {
        // Derive the base URL from the messages endpoint URL.
        let base_url = self
            .base_url
            .strip_suffix("/v1/messages")
            .unwrap_or("https://api.anthropic.com")
            .to_owned();
        models::list_models_with_base_url(&self.client, &self.api_key, &base_url).await
    }

    /// Send a streaming request to Anthropic's Messages API.
    async fn send_streaming_request(
        &self,
        system_prompt: Option<&str>,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> Result<reqwest::Response, Report<LlmServiceError>> {
        if self.api_key.is_empty() {
            return Err(Report::new(LlmServiceError::ApiKey)
                .attach(format!("Missing {PROVIDER_NAME} API key")));
        }

        let body = request::build_request(&self.model, messages, tools, system_prompt);

        let response = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .change_context(LlmServiceError::Provider)
            .attach(format!("{PROVIDER_NAME} streaming request failed"))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after_header = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(crate::service::parse_retry_after_header);
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "(no body)".to_owned());
            return Err(crate::service::classify_http_error(
                status,
                &error_text,
                PROVIDER_NAME,
                retry_after_header,
            ));
        }

        Ok(response)
    }
}

#[async_trait::async_trait]
impl LlmService for AnthropicService {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat_stream(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>> {
        let response = self
            .send_streaming_request(system_prompt, &messages, &[])
            .await?;
        let parser = AnthropicStreamParser::new();
        let sse = SseParser::new();

        let stream = response
            .bytes_stream()
            .scan((parser, sse), |(parser, sse), chunk| {
                let results: Vec<Result<String, Report<LlmServiceError>>> = match chunk {
                    Ok(bytes) => {
                        let events = sse.feed(&bytes);
                        let mut tokens = Vec::new();
                        for event in events {
                            match event {
                                SseEvent::Data(json) => {
                                    if let Some(StreamEvent::Text(text)) = parser.parse_data(&json)
                                    {
                                        tokens.push(Ok(text));
                                    }
                                }
                                SseEvent::Done => {}
                            }
                        }
                        tokens
                    }
                    Err(e) => {
                        vec![Err(Report::new(LlmServiceError::Provider)
                            .attach(format!("{PROVIDER_NAME} stream error"))
                            .attach(e.to_string()))]
                    }
                };
                async move { Some(results) }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }

    async fn chat_stream_with_tools(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolStream, Report<LlmServiceError>> {
        let response = self
            .send_streaming_request(system_prompt, &messages, &tools)
            .await?;
        let parser = AnthropicStreamParser::new();
        let sse = SseParser::new();

        let stream = response
            .bytes_stream()
            .scan((parser, sse), |(parser, sse), chunk| {
                let results: Vec<Result<StreamEvent, Report<LlmServiceError>>> = match chunk {
                    Ok(bytes) => {
                        let events = sse.feed(&bytes);
                        let mut stream_events = Vec::new();
                        for event in events {
                            match event {
                                SseEvent::Data(json) => {
                                    if let Some(ev) = parser.parse_data(&json) {
                                        stream_events.push(Ok(ev));
                                    }
                                }
                                SseEvent::Done => {}
                            }
                        }
                        stream_events
                    }
                    Err(e) => {
                        vec![Err(Report::new(LlmServiceError::Provider)
                            .attach(format!("{PROVIDER_NAME} stream error"))
                            .attach(e.to_string()))]
                    }
                };
                async move { Some(results) }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for AnthropicService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicService")
            .field("model", &self.model)
            .field("api_key", &self.api_key)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_in_result,
        reason = "test code, panics are acceptable"
    )]
    use crate::InputModalities;
    use crate::ModelInfo;

    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn service_list_models_returns_models_via_mock() {
        // Given a mock server and an AnthropicService pointed at it.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .match_header("x-api-key", "test-key")
            .with_status(200)
            .with_body(
                serde_json::json!({"data": [{"id": "claude-3", "context_window": 200_000}]})
                    .to_string(),
            )
            .create_async()
            .await;

        let base_url = format!("{}/v1/messages", server.url());
        let svc =
            AnthropicService::with_base_url("claude-3".to_owned(), "test-key".to_owned(), base_url);

        // When listing models.
        let result = svc.list_models().await;

        // Then models are returned.
        let models = result.expect("should succeed");
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0],
            ModelInfo {
                id: "claude-3".to_owned(),
                context_length: Some(200_000),
                input_modalities: InputModalities::text(),
            }
        );
        mock.assert_async().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn service_list_models_returns_error_on_http_failure() {
        // Given a mock server returning 500.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let base_url = format!("{}/v1/messages", server.url());
        let svc =
            AnthropicService::with_base_url("claude-3".to_owned(), "test-key".to_owned(), base_url);

        // When listing models.
        let result = svc.list_models().await;

        // Then it returns an error (not Ok(vec![])).
        assert!(result.is_err(), "should return Err on HTTP failure");
        mock.assert_async().await;
    }
}
