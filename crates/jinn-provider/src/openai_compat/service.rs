//! OpenAI-compatible streaming chat service.
//!
//! [`OpenAiCompatibleService`] implements [`LlmService`] using `reqwest`
//! to stream chat completions from any OpenAI-compatible provider.

use error_stack::{Report, ResultExt as _};
use futures::StreamExt as _;
use reqwest::Client;

use crate::ModelInfo;
use crate::llm_message::LlmMessage;
use crate::openai_compat::models;
use crate::openai_compat::provider_config::ProviderConfig;
use crate::openai_compat::reasoning_body::emit_reasoning_into;
use crate::openai_compat::request;
use crate::openai_compat::response::StreamResponseParser;
use crate::openai_compat::sse::{SseEvent, SseParser};
use crate::reasoning::ReasoningEffort;
use crate::service::{ChatStream, LlmService, LlmServiceError, ToolStream};
use crate::stream_event::StreamEvent;
use jinn_core_types::tool_types::ToolDefinition;

/// An LLM service that talks to an OpenAI-compatible API.
pub struct OpenAiCompatibleService {
    /// HTTP client (shared, connection-pooled).
    client: Client,
    /// Per-backend configuration.
    config: ProviderConfig,
    /// Model identifier.
    model: String,
    /// Base URL (override or default).
    base_url: String,
    /// API key for authentication.
    api_key: String,
    /// Extra body fields merged into every request.
    extra_body: serde_json::Map<String, serde_json::Value>,
}

impl OpenAiCompatibleService {
    /// Create a new service instance with an internally created HTTP client.
    #[must_use]
    pub fn new(
        config: ProviderConfig,
        model: String,
        base_url: Option<String>,
        api_key: String,
        extra_body: Option<serde_json::Value>,
        reasoning: Option<ReasoningEffort>,
    ) -> Self {
        Self::with_client(
            reqwest::Client::new(),
            config,
            model,
            base_url,
            api_key,
            extra_body,
            reasoning,
        )
    }

    /// Create a new service instance with a shared HTTP client.
    #[must_use]
    pub fn with_client(
        client: Client,
        config: ProviderConfig,
        model: String,
        base_url: Option<String>,
        api_key: String,
        extra_body: Option<serde_json::Value>,
        reasoning: Option<ReasoningEffort>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| config.default_base_url.to_owned());
        let mut extra_body = match extra_body {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };

        // Emit reasoning-effort fields in the shape appropriate to this backend.
        // Never clobbers a user-provided reasoning/reasoning_effort field.
        emit_reasoning_into(&mut extra_body, reasoning, &config);

        Self {
            client,
            config,
            model,
            base_url,
            api_key,
            extra_body,
        }
    }

    /// Fetch available model IDs from the provider.
    ///
    /// # Errors
    ///
    /// Returns [`LlmServiceError::Provider`] on HTTP or parse errors.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, Report<LlmServiceError>> {
        models::list_models(
            &self.client,
            &self.base_url,
            self.config.models_endpoint,
            &self.api_key,
            &self.config.custom_headers,
        )
        .await
    }

    /// Build and send a streaming chat completion request, returning the raw response.
    async fn send_streaming_request(
        &self,
        system_prompt: Option<&str>,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> Result<reqwest::Response, Report<LlmServiceError>> {
        if self.api_key.is_empty() {
            return Err(Report::new(LlmServiceError::ApiKey)
                .attach(format!("Missing {} API key", self.config.name)));
        }

        let body = request::build_request(
            &self.model,
            system_prompt,
            messages,
            tools,
            &self.extra_body,
        );

        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.config.chat_endpoint
        );

        let mut req = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body);

        for (key, value) in &self.config.custom_headers {
            req = req.header(key.as_str(), value.as_str());
        }

        tracing::debug!("{} streaming request: POST {}", self.config.name, url);
        tracing::debug!(
            provider = %self.config.name,
            model = %self.model,
            request_body = %serde_json::to_string(&body).unwrap_or_else(|e| format!("<serialize error: {e}>")),
            custom_headers = ?self.config.custom_headers,
            extra_body = ?self.extra_body,
            "full request details"
        );

        let response = req
            .send()
            .await
            .change_context(LlmServiceError::Provider)
            .attach(format!("{} streaming request failed", self.config.name))?;

        let status = response.status();
        if !status.is_success() {
            // Read Retry-After header before consuming the body.
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
                self.config.name,
                retry_after_header,
            ));
        }

        Ok(response)
    }
}

fn create_tool_stream(response: reqwest::Response, provider_name: &str) -> ToolStream {
    let parser = StreamResponseParser::new();
    let sse = SseParser::new();
    let mut chunk_index: usize = 0;

    let stream = response
        .bytes_stream()
        .scan(
            (parser, sse, provider_name.to_owned()),
            move |(parser, sse, name), chunk| {
                let ci = chunk_index;
                chunk_index += 1;
                let now = std::time::Instant::now();
                let results = match chunk {
                    Ok(bytes) => {
                        tracing::info!(
                            provider = %name,
                            chunk_index = ci,
                            raw_bytes = bytes.len(),
                            elapsed_ms = now.elapsed().as_millis(),
                            "STREAM CHUNK received from provider"
                        );
                        let events = sse.feed(&bytes);
                        tracing::info!(
                            provider = %name,
                            chunk_index = ci,
                            sse_events = events.len(),
                            "STREAM CHUNK parsed SSE events"
                        );
                        let mut stream_events = Vec::new();
                        for event in events {
                            match event {
                                SseEvent::Data(json) => {
                                    let parsed = parser.parse_data(&json);
                                    tracing::info!(
                                        provider = %name,
                                        chunk_index = ci,
                                        parsed_events = parsed.len(),
                                        "STREAM CHUNK parsed StreamEvents"
                                    );
                                    stream_events.extend(parsed.into_iter().map(Ok));
                                }
                                SseEvent::Done => {
                                    tracing::info!(provider = %name, chunk_index = ci, "STREAM [DONE]");
                                    stream_events.extend(parser.handle_done().into_iter().map(Ok));
                                }
                            }
                        }
                        stream_events
                    }
                    Err(e) => {
                        tracing::info!(
                            provider = %name,
                            chunk_index = ci,
                            error = %e,
                            "STREAM CHUNK error - connection likely reset"
                        );
                        vec![Err(Report::new(LlmServiceError::Provider)
                            .attach(format!("{name} stream error"))
                            .attach(e.to_string()))]
                    }
                };
                async move { Some(results) }
            },
        )
        .flat_map(futures::stream::iter);

    Box::pin(stream)
}

#[async_trait::async_trait]
impl LlmService for OpenAiCompatibleService {
    fn name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn chat_stream(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>> {
        let response = self
            .send_streaming_request(system_prompt, &messages, &[])
            .await?;
        let parser = StreamResponseParser::new();
        let sse = SseParser::new();
        let name = self.config.name.to_owned();

        let stream = response
            .bytes_stream()
            .scan((parser, sse, name), |(parser, sse, name), chunk| {
                let results: Vec<Result<String, Report<LlmServiceError>>> = match chunk {
                    Ok(bytes) => {
                        let events = sse.feed(&bytes);
                        let mut tokens = Vec::new();
                        for event in events {
                            match event {
                                SseEvent::Data(json) => {
                                    for stream_event in parser.parse_data(&json) {
                                        match stream_event {
                                            StreamEvent::Text(text) => tokens.push(Ok(text)),
                                            _ => {}
                                        }
                                    }
                                }
                                SseEvent::Done => {
                                    // Done is the stream terminator - no action needed for text-only.
                                }
                            }
                        }
                        tokens
                    }
                    Err(e) => {
                        vec![Err(Report::new(LlmServiceError::Provider)
                            .attach(format!("{name} stream error"))
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
        Ok(create_tool_stream(response, self.config.name))
    }
}

impl std::fmt::Debug for OpenAiCompatibleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleService")
            .field("provider", &self.config.name)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .field("extra_body", &self.extra_body)
            .finish_non_exhaustive()
    }
}
