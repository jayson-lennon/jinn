//! Google Gemini streaming chat service.

use error_stack::{Report, ResultExt as _};
use futures::StreamExt as _;
use reqwest::Client;

use crate::ModelInfo;
use crate::google::models;
use crate::google::request;
use crate::google::response::GeminiStreamParser;
use crate::llm_message::LlmMessage;
use crate::openai_compat::sse::{SseEvent, SseParser};
use crate::service::{ChatStream, LlmService, LlmServiceError, ToolStream};
use crate::stream_event::StreamEvent;
use jinn_core_types::tool_types::ToolDefinition;

const PROVIDER_NAME: &str = "Google";

/// An LLM service that talks to Google's Gemini API.
pub struct GoogleService {
    /// HTTP client.
    client: Client,
    /// Model identifier.
    model: String,
    /// API key for authentication (passed as query parameter).
    api_key: String,
    /// Base URL override (for testing).
    base_url: Option<String>,
}

impl GoogleService {
    /// Create a new Google service instance.
    #[must_use]
    pub fn new(model: String, api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            api_key,
            base_url: None,
        }
    }

    /// Create a new Google service with a custom client.
    #[must_use]
    pub fn with_client(client: Client, model: String, api_key: String) -> Self {
        Self {
            client,
            model,
            api_key,
            base_url: None,
        }
    }

    /// Create a new Google service with a custom base URL (for testing).
    #[must_use]
    pub fn with_base_url(model: String, api_key: String, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            api_key,
            base_url: Some(base_url),
        }
    }

    /// Fetch available model IDs from Google.
    ///
    /// # Errors
    ///
    /// Returns [`LlmServiceError::Provider`] on HTTP or parse errors.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, Report<LlmServiceError>> {
        let base_url = self
            .base_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com");
        models::list_models_with_base_url(&self.client, &self.api_key, base_url).await
    }

    /// Build the streaming URL.
    fn stream_url(&self) -> String {
        if let Some(ref base) = self.base_url {
            format!("{base}?key={}", self.api_key)
        } else {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
                self.model, self.api_key
            )
        }
    }

    /// Send a streaming request to Gemini.
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

        let body = request::build_request(system_prompt, messages, tools);
        let url = self.stream_url();

        let response = self
            .client
            .post(&url)
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
impl LlmService for GoogleService {
    fn name(&self) -> &'static str {
        "google"
    }

    async fn chat_stream(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>> {
        let response = self
            .send_streaming_request(system_prompt, &messages, &[])
            .await?;
        let parser = GeminiStreamParser::new();
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
                                    for ev in parser.parse_data(&json) {
                                        if let StreamEvent::Text(text) = ev {
                                            tokens.push(Ok(text));
                                        }
                                    }
                                }
                                SseEvent::Done => {
                                    for ev in parser.handle_done() {
                                        // Text-only stream ignores Done.
                                        let _ = ev;
                                    }
                                }
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
        let parser = GeminiStreamParser::new();
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
                                    for ev in parser.parse_data(&json) {
                                        stream_events.push(Ok(ev));
                                    }
                                }
                                SseEvent::Done => {
                                    for ev in parser.handle_done() {
                                        stream_events.push(Ok(ev));
                                    }
                                }
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

impl std::fmt::Debug for GoogleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleService")
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
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1beta/models?key=test-key")
            .with_status(200)
            .with_body(
                serde_json::json!({"models": [{"name": "gemini-pro", "input_token_limit": 32000}]})
                    .to_string(),
            )
            .create_async()
            .await;

        let svc = GoogleService::with_base_url(
            "gemini-pro".to_owned(),
            "test-key".to_owned(),
            server.url(),
        );

        let result = svc.list_models().await;
        let models = result.expect("should succeed");
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0],
            ModelInfo {
                id: "gemini-pro".to_owned(),
                context_length: Some(32_000),
                input_modalities: InputModalities::text(),
            }
        );
        mock.assert_async().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn service_list_models_returns_error_on_http_failure() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1beta/models?key=test-key")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let svc = GoogleService::with_base_url(
            "gemini-pro".to_owned(),
            "test-key".to_owned(),
            server.url(),
        );

        let result = svc.list_models().await;
        assert!(result.is_err(), "should return Err on HTTP failure");
        mock.assert_async().await;
    }
}
