//! Contract tests for Google Gemini provider using mockito.

use futures::StreamExt as _;
use jinn_provider::{
    GoogleFactory, LlmMessage, LlmService, LlmServiceFactory, StreamEvent, ToolDefinition,
};

fn make_service(server: &mockito::ServerGuard) -> jinn_provider::google::GoogleService {
    jinn_provider::google::GoogleService::with_base_url(
        "gemini-pro".into(),
        "test-key".into(),
        server.url(),
    )
}

// ---------------------------------------------------------------------------
// Text streaming
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn text_streaming_yields_text_events() {
    // Given a mock Google server that streams a text response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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

    // Then a Text event with the expected content is emitted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(t) if t == "Hello"))
    );
}

// ---------------------------------------------------------------------------
// Tool-call streaming
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_yields_tool_use_start() {
    // Given a mock Google server that streams a function call response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"echo\",\"args\":{\"x\":1}}}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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
    // Given a mock Google server that streams a function call response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"echo\",\"args\":{\"x\":1}}}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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
    // Given a mock Google server that streams a function call response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"echo\",\"args\":{\"x\":1}}}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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
    // Given a mock Google server that streams a function call response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"echo\",\"args\":{\"x\":1}}}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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
    // Given a Google factory with an empty API key.
    let factory = GoogleFactory::new("gemini-pro".into(), String::new(), "test".into());

    // When creating the service.
    let result = factory.create();

    // Then creation fails.
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn error_response_401_returns_error() {
    // Given a mock Google server that returns a 401 status.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
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

// ---------------------------------------------------------------------------
// Simple streaming (chat_stream)
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn chat_stream_yields_text_tokens_only() {
    // Given a mock Google server that streams a text response.
    let mut server = mockito::Server::new_async().await;
    let service = make_service(&server);

    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\"finishReason\":\"STOP\"}]}\n\n";

    server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"\?key=test-key".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
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

    // Then exactly the expected text token is returned.
    assert_eq!(tokens, vec!["Hello"]);
}
