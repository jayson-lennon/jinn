#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

use futures::StreamExt;

use jinn_core_types::tool_types::ToolCall;
use crate::{FakeLlmServiceFactory, LlmMessage, LlmServiceFactory, StopReason, StreamEvent};

#[rstest::rstest]
#[tokio::test]
async fn fake_service_yields_configured_tokens() {
    // Given a fake factory with specific tokens.
    let factory = FakeLlmServiceFactory::new(vec![
        "Hello".to_owned(),
        " world".to_owned(),
        "!".to_owned(),
    ]);

    // When creating a service and streaming.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream(None, vec![])
        .await
        .expect("chat_stream");
    let tokens: Vec<String> = StreamExt::map(stream, |r| r.expect("token"))
        .collect()
        .await;

    // Then the tokens match the configured list.
    assert_eq!(tokens, vec!["Hello", " world", "!"]);
}

#[rstest::rstest]
#[tokio::test]
async fn fake_service_empty_tokens() {
    // Given a fake factory with no tokens.
    let factory = FakeLlmServiceFactory::new(vec![]);

    // When creating a service and streaming.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream(None, vec![])
        .await
        .expect("chat_stream");
    let tokens: Vec<String> = stream.map(|r| r.expect("token")).collect().await;

    // Then no tokens are produced.
    assert!(tokens.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn fake_service_yields_text_events_and_done() {
    // Given a fake factory with no tool calls.
    let factory = FakeLlmServiceFactory::new(vec!["Hello".to_owned()]);

    // When streaming with tools.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then text tokens are wrapped in StreamEvent::Text and stream ends with Done.
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], StreamEvent::Text("Hello".to_owned()));
    assert_eq!(
        events[1],
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn fake_service_emits_text_events() {
    // Given a fake factory with tool calls.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_calls(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
    );

    // When streaming with tools.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits text.
    assert!(matches!(&events[0], StreamEvent::Text(t) if t == "Let me check"));
}

#[rstest::rstest]
#[tokio::test]
async fn fake_service_emits_tool_events() {
    // Given a fake factory with tool calls.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_calls(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
    );

    // When streaming with tools.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits tool events.
    assert!(matches!(
        &events[1],
        StreamEvent::ToolUseStart { index: 0, .. }
    ));
    assert!(matches!(
        &events[2],
        StreamEvent::ToolUseInputDelta { index: 0, .. }
    ));
    assert!(matches!(
        &events[3],
        StreamEvent::ToolUseComplete { index: 0, .. }
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn fake_service_emits_done_with_tool_use() {
    // Given a fake factory with tool calls.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_calls(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
    );

    // When streaming with tools.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream ends with Done(tool_use).
    assert_eq!(
        events[4],
        StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: None,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_emits_text_token() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits a text token.
    assert!(matches!(&events[0], StreamEvent::Text(t) if t == "Let me check"));
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_emits_tool_use_start() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits a ToolUseStart event.
    assert!(matches!(
        &events[1],
        StreamEvent::ToolUseStart { index: 0, .. }
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_emits_tool_use_delta() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits a ToolUseInputDelta event.
    assert!(matches!(
        &events[2],
        StreamEvent::ToolUseInputDelta { index: 0, .. }
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_emits_tool_use_complete() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits a ToolUseComplete event.
    assert!(matches!(
        &events[3],
        StreamEvent::ToolUseComplete { index: 0, .. }
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_emits_done_with_tool_use() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call.clone()],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream ends with Done(tool_use).
    assert_eq!(
        events[4],
        StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: None,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_second_call_returns_text_only() {
    // Given a tool loop factory.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["Let me check".to_owned()],
        vec![tool_call],
        vec!["Here is the answer".to_owned()],
    );

    // When creating a service and making two calls with the trigger.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];

    // First call - consume the tool_use response.
    let stream = service
        .chat_stream_with_tools(None, messages.clone(), vec![])
        .await
        .expect("first call");
    let _events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Second call - should return text only.
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("second call");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits subsequent tokens and Done with end_turn.
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        StreamEvent::Text("Here is the answer".to_owned())
    );
    assert_eq!(
        events[1],
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn non_trigger_produces_default_events() {
    // Given a tool loop factory.
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["normal response".to_owned()],
        vec![],
        vec!["subsequent".to_owned()],
    );

    // When streaming with a non-trigger message.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "regular message".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits the default tokens (not the tool loop ones).
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], StreamEvent::Text("normal response".to_owned()));
    assert_eq!(
        events[1],
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn non_trigger_does_not_enter_tool_loop() {
    // Given a tool loop factory.
    let factory = FakeLlmServiceFactory::with_tool_loop(
        vec!["normal response".to_owned()],
        vec![],
        vec!["subsequent".to_owned()],
    );

    // When streaming with a non-trigger message.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "regular message".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let _events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the tool loop call count is zero.
    assert_eq!(factory.tool_loop_call_count(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn with_tool_calls_and_empty_tokens_yields_tool_events_only() {
    // Given a factory with tool calls but no text tokens.
    let tool_call = ToolCall {
        id: "call_1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"input":"hi"}"#.to_owned(),
    };
    let factory = FakeLlmServiceFactory::with_tool_calls(vec![], vec![tool_call]);

    // When streaming with tools.
    let service = factory.create().expect("create service");
    let stream = service
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream has no Text events, only tool events and Done(tool_use).
    assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolUseComplete { .. }))
    );
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: None,
        })
    );
}

#[rstest::rstest]
#[tokio::test]
async fn tool_loop_with_multiple_tool_calls_on_first_response() {
    // Given a tool loop factory with multiple tool calls.
    let tc1 = ToolCall {
        id: "call_1".to_owned(),
        name: "read_file".to_owned(),
        arguments: r#"{"path":"a.rs"}"#.to_owned(),
    };
    let tc2 = ToolCall {
        id: "call_2".to_owned(),
        name: "read_file".to_owned(),
        arguments: r#"{"path":"b.rs"}"#.to_owned(),
    };
    let factory =
        FakeLlmServiceFactory::with_tool_loop(vec![], vec![tc1, tc2], vec!["Done.".to_owned()]);

    // When creating a service and streaming with the trigger prompt.
    let service = factory.create().expect("create service");
    let messages = vec![LlmMessage::User {
        content: "__tool_loop_test__".to_owned(),
        attachments: Vec::new(),
    }];
    let stream = service
        .chat_stream_with_tools(None, messages, vec![])
        .await
        .expect("chat_stream_with_tools");
    let events: Vec<StreamEvent> = stream.map(|r| r.expect("event")).collect().await;

    // Then the stream emits two ToolUseStart events (one per tool call).
    let starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
        .collect();
    assert_eq!(starts.len(), 2);

    // And two ToolUseComplete events.
    let completes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolUseComplete { .. }))
        .collect();
    assert_eq!(completes.len(), 2);
}

#[rstest::rstest]
fn factory_name_returns_fake_llm() {
    let factory = FakeLlmServiceFactory::new(vec![]);
    assert_eq!(factory.name(), "FakeLlm");
}

/// Collect a tool stream into (text-joined, stop-reason).
async fn collect_tool_stream(stream: crate::service::ToolStream) -> (String, Option<StopReason>) {
    let mut text = String::new();
    let mut stop = None;
    let mut s = stream;
    while let Some(ev) = s.next().await {
        match ev.expect("stream event") {
            StreamEvent::Text(t) => text.push_str(&t),
            StreamEvent::Done { stop_reason, .. } => stop = Some(stop_reason),
            _ => {}
        }
    }
    (text, stop)
}

#[rstest::rstest]
#[tokio::test]
async fn scripted_queue_serves_responses_in_fifo_order() {
    // Given a factory with two scripted responses queued.
    let factory = FakeLlmServiceFactory::new(vec![]);
    factory.push_scripted_response(crate::fake::ScriptedResponse {
        tokens: vec!["first".to_owned()],
        tool_calls: vec![],
    });
    factory.push_scripted_response(crate::fake::ScriptedResponse {
        tokens: vec!["second".to_owned()],
        tool_calls: vec![],
    });

    // When two calls drain the queue.
    let s1 = factory
        .create()
        .expect("create")
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("stream1");
    let s2 = factory
        .create()
        .expect("create")
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("stream2");
    let (t1, r1) = collect_tool_stream(s1).await;
    let (t2, r2) = collect_tool_stream(s2).await;

    // Then the first call yields "first", the second yields "second" (FIFO).
    assert_eq!(t1, "first");
    assert_eq!(t2, "second");
    assert_eq!(r1, Some(StopReason::EndTurn));
    assert_eq!(r2, Some(StopReason::EndTurn));
}

#[rstest::rstest]
#[tokio::test]
async fn scripted_queue_tool_call_uses_tooluse_stop_reason() {
    // Given a scripted verdict tool call.
    let factory = FakeLlmServiceFactory::new(vec![]);
    factory.push_scripted_response(crate::fake::ScriptedResponse {
        tokens: vec![],
        tool_calls: vec![ToolCall {
            id: "tc1".to_owned(),
            name: "judgment_failed".to_owned(),
            arguments: r#"{"message":"bad"}"#.to_owned(),
        }],
    });

    // When streaming.
    let stream = factory
        .create()
        .expect("create")
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("stream");
    let (text, reason) = collect_tool_stream(stream).await;

    // Then the stop reason is ToolUse (so the tool loop fires the handler).
    assert!(text.is_empty());
    assert_eq!(reason, Some(StopReason::ToolUse));
}

#[rstest::rstest]
#[tokio::test]
async fn scripted_queue_exhaustion_falls_back_to_static_text_stream() {
    // Given a factory with an empty queue but configured tokens.
    let factory = FakeLlmServiceFactory::new(vec!["fallback".to_owned()]);

    // When a call drains nothing (queue empty).
    let stream = factory
        .create()
        .expect("create")
        .chat_stream_with_tools(None, vec![], vec![])
        .await
        .expect("stream");
    let (text, _reason) = collect_tool_stream(stream).await;

    // Then the static token path is used as the fallback.
    assert_eq!(text, "fallback");
}
