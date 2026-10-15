//! Contract tests for OpenAI-compatible provider using mockito.
//!
//! Verifies streaming chat, tool calls, model listing, extra_body,
//! base_url override, auth headers, and error handling.

use futures::StreamExt as _;
use jinn_provider::{
    LlmMessage, LlmServiceFactory, OpenAiCompatibleFactory, ProviderConfig, StreamEvent,
    ToolDefinition,
};

fn make_factory(server: &mockito::ServerGuard) -> OpenAiCompatibleFactory {
    let config = ProviderConfig::openai();
    OpenAiCompatibleFactory::new(
        config,
        "gpt-4".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        None,
        "test-openai".to_owned(),
    )
}

fn make_factory_with_extra(
    server: &mockito::ServerGuard,
    extra: serde_json::Value,
) -> OpenAiCompatibleFactory {
    let config = ProviderConfig::openai();
    OpenAiCompatibleFactory::new(
        config,
        "gpt-4".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        Some(extra),
        None,
        "test-openai".to_owned(),
    )
}

// ---------------------------------------------------------------------------
// Text streaming
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn text_streaming_yields_text_events() {
    // Given a mock OpenAI server that streams two text deltas.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then both text tokens appear in the event stream.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(t) if t == "Hello"))
    );
    // And the second text token is also present.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(t) if t == " world"))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn text_streaming_yields_done_event() {
    // Given a mock OpenAI server that streams text followed by a stop.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a Done event is emitted.
    assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
}

// ---------------------------------------------------------------------------
// Tool-call streaming
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_yields_tool_use_start() {
    // Given a mock OpenAI server that streams a tool-call response.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = [
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":1}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ].join("");

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "call echo".into(),
                attachments: Vec::new(),
            }],
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo".into(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object"}),
                server_tool_type: None,
            }],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a ToolUseStart event with the tool name is emitted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "echo"))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_yields_input_delta() {
    // Given a mock OpenAI server that streams a tool-call response.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = [
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":1}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ].join("");

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "call echo".into(),
                attachments: Vec::new(),
            }],
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo".into(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object"}),
                server_tool_type: None,
            }],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a ToolUseInputDelta event is emitted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseInputDelta { .. }))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_yields_tool_use_complete() {
    // Given a mock OpenAI server that streams a tool-call response.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = [
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":1}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ].join("");

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "call echo".into(),
                attachments: Vec::new(),
            }],
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo".into(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object"}),
                server_tool_type: None,
            }],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a ToolUseComplete event is emitted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseComplete { .. }))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_yields_done_event() {
    // Given a mock OpenAI server that streams a tool-call response.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = [
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":1}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ].join("");

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "call echo".into(),
                attachments: Vec::new(),
            }],
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo".into(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object"}),
                server_tool_type: None,
            }],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a Done event is emitted.
    assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
}

// ---------------------------------------------------------------------------
// Model listing
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn list_models_returns_model_ids() {
    // Given a mock OpenAI server that returns a model list.
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/models")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{\"data\":[{\"id\":\"gpt-4\"},{\"id\":\"gpt-3.5-turbo\"}]}")
        .create_async()
        .await;

    let config = ProviderConfig::openai();
    let svc = jinn_provider::OpenAiCompatibleService::new(
        config,
        "gpt-4".into(),
        Some(server.url()),
        "test-key".into(),
        None,
        None,
    );

    // When listing models.
    let models = svc.list_models().await.unwrap();

    // Then the expected model IDs are returned.
    assert!(models.iter().any(|m| m.id == "gpt-4"));
    // And the second model is also present.
    assert!(models.iter().any(|m| m.id == "gpt-3.5-turbo"));
}

#[rstest::rstest]
#[tokio::test]
async fn list_models_parses_context_length() {
    // Given a mock OpenAI server that returns models with context_length.
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/models")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            "{\"data\":[{\"id\":\"gpt-4\",\"context_length\":128000},{\"id\":\"gpt-3.5-turbo\",\"context_length\":16385}]}",
        )
        .create_async()
        .await;

    let config = ProviderConfig::openai();
    let svc = jinn_provider::OpenAiCompatibleService::new(
        config,
        "gpt-4".into(),
        Some(server.url()),
        "test-key".into(),
        None,
        None,
    );

    // When listing models.
    let models = svc.list_models().await.unwrap();

    // Then context_length is parsed for each model.
    let gpt4 = models.iter().find(|m| m.id == "gpt-4").expect("gpt-4");
    assert_eq!(gpt4.context_length, Some(128_000));
    let gpt35 = models
        .iter()
        .find(|m| m.id == "gpt-3.5-turbo")
        .expect("gpt-3.5");
    assert_eq!(gpt35.context_length, Some(16385));
}

#[rstest::rstest]
#[tokio::test]
async fn list_models_handles_missing_context_length() {
    // Given a mock server that returns models without context_length.
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{\"data\":[{\"id\":\"local-model\"}]}")
        .create_async()
        .await;

    let config = ProviderConfig::openai();
    let svc = jinn_provider::OpenAiCompatibleService::new(
        config,
        "local-model".into(),
        Some(server.url()),
        "test-key".into(),
        None,
        None,
    );

    // When listing models.
    let models = svc.list_models().await.unwrap();

    // Then context_length is None when not provided.
    let model = models.first().expect("at least one model");
    assert_eq!(model.id, "local-model");
    assert_eq!(model.context_length, None);
}

// ---------------------------------------------------------------------------
// Extra body fields
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn extra_body_fields_included_in_request() {
    // Given a mock OpenAI server and a factory with extra_body fields.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory_with_extra(
        &server,
        serde_json::json!({"enable_thinking": true, "tool_stream": true}),
    );

    let mock = server
        .mock("POST", "/chat/completions")
        .match_body(mockito::Matcher::PartialJsonString(
            "{\"enable_thinking\":true,\"tool_stream\":true}".to_owned(),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "test".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When consuming the stream.
    let _events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then the request included the extra_body fields.
    mock.assert_async().await;
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn error_response_401_mapped_to_provider_error() {
    // Given a mock OpenAI server that returns a 401 status.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    server
        .mock("POST", "/chat/completions")
        .with_status(401)
        .with_body("{\"error\":\"unauthorized\"}")
        .create_async()
        .await;

    let service = factory.create().unwrap();

    // When starting a chat stream.
    let result = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await;

    // Then the result is an error.
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Reasoning content
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn reasoning_content_produces_reasoning_event() {
    // Given a mock OpenAI server that streams reasoning content.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking...\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "think".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a Reasoning event with the expected content is emitted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Reasoning(t) if t == "thinking..."))
    );
}

// ---------------------------------------------------------------------------
// Base URL override
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn base_url_override_routes_to_correct_host() {
    // Given an OpenRouter config pointing at a mock server.
    let mut server = mockito::Server::new_async().await;
    let config = ProviderConfig::openrouter();
    let factory = OpenAiCompatibleFactory::new(
        config,
        "moonshotai/kimi-k2:free".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        None,
        "test-openrouter".to_owned(),
    );

    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then events are received from the overridden host.
    assert_ne!(events, [] as [jinn_provider::StreamEvent; 0]);
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn factory_rejects_empty_api_key() {
    // Given an OpenAI-compatible factory with an empty API key.
    let config = ProviderConfig::openai();
    let factory = OpenAiCompatibleFactory::new(
        config,
        "gpt-4".to_owned(),
        None,
        String::new(),
        None,
        None,
        "test-openai".to_owned(),
    );

    // When creating the service.
    let result = factory.create();

    // Then creation fails.
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Simple streaming (chat_stream)
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn chat_stream_yields_text_tokens_only() {
    // Given a mock OpenAI server that streams two text deltas.
    let mut server = mockito::Server::new_async().await;
    let factory = make_factory(&server);

    let body = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";

    server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
        )
        .await
        .unwrap();

    // When collecting all tokens from the stream.
    let tokens: Vec<String> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then exactly the expected text tokens are returned.
    assert_eq!(tokens, vec!["Hello", " world"]);
}

// ---------------------------------------------------------------------------
// Custom headers
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn custom_headers_from_config_are_sent() {
    // Given an OpenAI-compatible factory with custom headers.
    let mut server = mockito::Server::new_async().await;

    let config = ProviderConfig {
        name: "TestCustom",
        default_base_url: "https://example.com/v1/",
        chat_endpoint: "chat/completions",
        models_endpoint: "models",
        custom_headers: vec![("X-Custom-Header".to_owned(), "custom-value".to_owned())],
    };

    let factory = OpenAiCompatibleFactory::new(
        config,
        "test-model".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        None,
        "test-custom".to_owned(),
    );

    server
        .mock("POST", "/chat/completions")
        .match_header("x-custom-header", "custom-value")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When consuming the stream, the custom header was matched by the mock.
    let _events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then the request succeeded (mock assertion is implicit via match_header).
}

#[rstest::rstest]
#[tokio::test]
async fn openrouter_attribution_headers_are_sent_on_chat_requests() {
    // Given an OpenRouter factory pointed at a mock server.
    let mut server = mockito::Server::new_async().await;
    let config = ProviderConfig::openrouter();
    let factory = OpenAiCompatibleFactory::new(
        config,
        "test-model".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        None,
        "test-openrouter".to_owned(),
    );

    // The mock only matches when all three attribution headers ride on the request.
    server
        .mock("POST", "/chat/completions")
        .match_header("http-referer", "https://jaysonlennon.dev")
        .match_header("x-openrouter-title", "jinn")
        .match_header("x-openrouter-categories", "cli-agent")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();

    // When consuming the stream, the attribution headers were matched by the mock.
    let _events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then the request succeeded (mock assertion is implicit via match_header).
}

// ---------------------------------------------------------------------------
// Reasoning effort wire shape
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn openrouter_emits_nested_reasoning_effort_in_request() {
    // Given a mock OpenRouter server and a factory with High reasoning effort.
    let mut server = mockito::Server::new_async().await;
    let factory = OpenAiCompatibleFactory::new(
        ProviderConfig::openrouter(),
        "openai/gpt-4".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        Some(jinn_provider::ReasoningEffort::High),
        "openrouter".to_owned(),
    );

    let mock = server
        .mock("POST", "/chat/completions")
        .match_body(mockito::Matcher::PartialJsonString(
            "{\"reasoning\":{\"effort\":\"high\",\"enabled\":true}}".to_owned(),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    // When streaming a message through the service.
    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "test".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();
    let _events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then the request body used the nested OpenRouter wire shape.
    mock.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn non_openrouter_emits_flat_reasoning_effort_in_request() {
    // Given a mock non-OpenRouter (OpenAI direct) server and a factory with High effort.
    let mut server = mockito::Server::new_async().await;
    let factory = OpenAiCompatibleFactory::new(
        ProviderConfig::openai(),
        "gpt-4".to_owned(),
        Some(server.url()),
        "test-key".to_owned(),
        None,
        Some(jinn_provider::ReasoningEffort::High),
        "openai".to_owned(),
    );

    let mock = server
        .mock("POST", "/chat/completions")
        .match_body(mockito::Matcher::PartialJsonString(
            "{\"reasoning_effort\":\"high\"}".to_owned(),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
        .create_async()
        .await;

    // When streaming a message through the service.
    let service = factory.create().unwrap();
    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "test".into(),
                attachments: Vec::new(),
            }],
            vec![],
        )
        .await
        .unwrap();
    let _events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then the request body used the flat (non-nested) wire shape for this backend.
    mock.assert_async().await;
}
