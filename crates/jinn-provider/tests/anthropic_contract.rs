//! Contract tests for Anthropic provider using mockito.
//!
//! Verifies streaming chat, tool calls, auth headers, and response parsing.

use futures::StreamExt as _;
use jinn_provider::{
    AnthropicFactory, LlmMessage, LlmService, LlmServiceFactory, StreamEvent, ToolDefinition,
};

fn make_service(server: &mockito::ServerGuard) -> jinn_provider::anthropic::AnthropicService {
    jinn_provider::anthropic::AnthropicService::with_base_url(
        "claude-3".into(),
        "test-key".into(),
        format!("{}/v1/messages", server.url()),
    )
}

// ---------------------------------------------------------------------------
// Text streaming
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn text_streaming_yields_text_events() {
    // Given a mock Anthropic server that streams two text deltas.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .match_header("x-api-key", "test-key")
        .match_header("anthropic-version", "2023-06-01")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
            jinn_provider::ToolChoice::Auto,
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
    // Given a mock Anthropic server that streams text followed by a stop.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .match_header("x-api-key", "test-key")
        .match_header("anthropic-version", "2023-06-01")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

    let stream = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
            jinn_provider::ToolChoice::Auto,
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
    // Given a mock Anthropic server that streams a tool-use response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"echo\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

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
            jinn_provider::ToolChoice::Auto,
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
    // Given a mock Anthropic server that streams a tool-use response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"echo\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

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
            jinn_provider::ToolChoice::Auto,
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
    // Given a mock Anthropic server that streams a tool-use response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"echo\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

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
            jinn_provider::ToolChoice::Auto,
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
    // Given a mock Anthropic server that streams a tool-use response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"echo\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    ].join("");

    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

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
            jinn_provider::ToolChoice::Auto,
        )
        .await
        .unwrap();

    // When collecting all stream events.
    let events: Vec<StreamEvent> = stream.filter_map(|r| async move { r.ok() }).collect().await;

    // Then a Done event is emitted.
    assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn factory_rejects_empty_api_key() {
    // Given an Anthropic factory with an empty API key.
    let factory = AnthropicFactory::new("claude-3".into(), String::new(), "test".into());

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
    // Given a mock Anthropic server that streams two text deltas.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = [
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    ]
    .join("");

    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&body)
        .create_async()
        .await;

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
// Error handling
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn error_response_401_returns_error() {
    // Given a mock Anthropic server that returns a 401 status.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    server
        .mock("POST", "/v1/messages")
        .with_status(401)
        .with_body("{\"error\":\"unauthorized\"}")
        .create_async()
        .await;

    // When starting a chat stream.
    let result = service
        .chat_stream_with_tools(
            None,
            vec![LlmMessage::User {
                content: "hi".into(),
                attachments: Vec::new(),
            }],
            vec![],
            jinn_provider::ToolChoice::Auto,
        )
        .await;

    // Then the result is an error.
    assert!(result.is_err());
}
