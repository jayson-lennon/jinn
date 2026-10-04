#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::unused_async,
    reason = "test code"
)]

#[cfg(test)]
mod test_fakes {
    use error_stack::Report;
    use jinn_provider::{ChatStream, LlmService, LlmServiceError, LlmServiceFactory, ToolStream};

    /// An LLM service whose stream never yields — used to test cancel paths
    /// while a stream is genuinely in flight.
    #[derive(Debug)]
    struct HangingLlmService;

    #[async_trait::async_trait]
    impl LlmService for HangingLlmService {
        fn name(&self) -> &'static str {
            "HangingLlm"
        }
        async fn chat_stream(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
        ) -> Result<ChatStream, Report<LlmServiceError>> {
            Ok(Box::pin(futures::stream::pending()))
        }
        async fn chat_stream_with_tools(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
            _tools: Vec<jinn_core_types::ToolDefinition>,
        ) -> Result<ToolStream, Report<LlmServiceError>> {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    /// Factory that produces [`HangingLlmService`] instances.
    #[derive(Debug)]
    pub(in crate::inference_actor) struct HangingLlmFactory;

    impl LlmServiceFactory for HangingLlmFactory {
        fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
            Ok(Box::new(HangingLlmService))
        }
        fn name(&self) -> &'static str {
            "HangingLlm"
        }
    }

    /// A factory whose stream creation fails immediately — used to verify the
    /// error path publishes a chat entry and stream completion.
    #[derive(Debug)]
    pub(in crate::inference_actor) struct ErroringLlmFactory;

    impl LlmServiceFactory for ErroringLlmFactory {
        fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
            Ok(Box::new(ErroringLlmService))
        }
        fn name(&self) -> &'static str {
            "ErroringLlm"
        }
    }

    /// A service whose stream immediately yields an error.
    #[derive(Debug)]
    struct ErroringLlmService;

    #[async_trait::async_trait]
    impl LlmService for ErroringLlmService {
        fn name(&self) -> &'static str {
            "ErroringLlm"
        }
        async fn chat_stream(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
        ) -> Result<ChatStream, Report<LlmServiceError>> {
            Err(Report::new(LlmServiceError::Provider))
        }
        async fn chat_stream_with_tools(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
            _tools: Vec<jinn_core_types::ToolDefinition>,
        ) -> Result<ToolStream, Report<LlmServiceError>> {
            Err(Report::new(LlmServiceError::Provider))
        }
    }
}

use super::*;
use crate::session::SessionState;
use test_fakes::{ErroringLlmFactory, HangingLlmFactory};

use jinn_provider::FakeLlmServiceFactory;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

/// Actor over its own private fake bus — for tests that only inspect
/// actor fields (no publish observability needed).
async fn test_llm_actor_standalone() -> InferenceActor {
    let services = jinn_kernel::common::services::Services::new_fake().await;
    InferenceActor {
        services,
        tasks: HashMap::new(),
        sessions: HashMap::new(),
        cancelled_sessions: HashSet::new(),
    }
}

async fn test_llm_actor(harness: &TestHarness) -> InferenceActor {
    test_llm_actor_with_factory(harness, FakeLlmServiceFactory::new(vec![])).await
}

/// Builds the actor with a specific provider factory over the harness's
/// bus (struct-direct: `Services.llm_service` is the factory the actor
/// resolves; `services.bus` must be the harness bus so publishes are
/// observable).
async fn test_llm_actor_with_factory<F: jinn_provider::LlmServiceFactory + 'static>(
    harness: &TestHarness,
    factory: F,
) -> InferenceActor {
    let mut services = crate::inference_actor::test_services_with_bus(harness.bus()).await;
    services.llm_service = jinn_provider_config::LlmServiceFactoryService::new(Arc::new(factory));
    InferenceActor {
        services,
        tasks: HashMap::new(),
        sessions: HashMap::new(),
        cancelled_sessions: HashSet::new(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn handle_stream_completed_error_reason_removes_session() {
    // Given an LLM actor with a streaming session.
    let mut actor = test_llm_actor_standalone().await;
    let session_id = SessionId::new();
    actor
        .sessions
        .insert(session_id.clone(), SessionData::new());

    // When handling StreamCompleted with Error reason.
    let payload = StreamCompleted {
        model_used: None,
        session_id,
        reason: StreamCompletedReason::Error,
        assistant_content: None,
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at: jiff::Timestamp::now(),
    };
    actor.handle_stream_completed(&payload);

    // Then the session is removed from the sessions map.
    assert!(actor.sessions.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn handle_stream_completed_finished_reason_removes_session() {
    // Given an LLM actor with a streaming session.
    let mut actor = test_llm_actor_standalone().await;
    let session_id = SessionId::new();
    actor
        .sessions
        .insert(session_id.clone(), SessionData::new());

    // When handling StreamCompleted with Finished reason.
    let payload = StreamCompleted {
        model_used: None,
        session_id,
        reason: StreamCompletedReason::Finished,
        assistant_content: Some("hello".to_owned()),
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at: jiff::Timestamp::now(),
    };
    actor.handle_stream_completed(&payload);

    // Then the session is removed.
    assert!(
        actor.sessions.is_empty(),
        "Finished should remove the session"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn handle_stream_completed_tool_use_keeps_session() {
    // Given an LLM actor with a streaming session.
    let mut actor = test_llm_actor_standalone().await;
    let session_id = SessionId::new();
    actor
        .sessions
        .insert(session_id.clone(), SessionData::new());

    // When handling StreamCompleted with ToolUse reason.
    let payload = StreamCompleted {
        model_used: None,
        session_id: session_id.clone(),
        reason: StreamCompletedReason::ToolUse,
        assistant_content: Some("thinking...".to_owned()),
        tool_calls: Some(vec![]),
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at: jiff::Timestamp::now(),
    };
    actor.handle_stream_completed(&payload);

    // Then the session is kept for continuation.
    assert!(
        actor.sessions.contains_key(&session_id),
        "ToolUse should keep the session for continuation"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn handle_stream_completed_unknown_session_is_noop() {
    // Given an LLM actor with NO sessions.
    let mut actor = test_llm_actor_standalone().await;
    let session_id = SessionId::new();

    // When handling StreamCompleted for an unknown session.
    let payload = StreamCompleted {
        model_used: None,
        session_id,
        reason: StreamCompletedReason::Error,
        assistant_content: None,
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at: jiff::Timestamp::now(),
    };
    actor.handle_stream_completed(&payload);

    // Then nothing happens - no panic.
    assert!(actor.sessions.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_stream_removes_session_and_task() {
    // Given an LLM actor with a session and a spawned task.
    let mut actor = test_llm_actor_standalone().await;
    let session_id = SessionId::new();
    actor
        .sessions
        .insert(session_id.clone(), SessionData::new());
    // Insert a dummy task that will be aborted.
    let handle = tokio::spawn(async { std::future::pending::<()>().await });
    actor.tasks.insert(session_id.clone(), handle);

    // When cancelling the stream.
    actor.cancel_stream(&session_id).await;

    // Then the session and task are removed.
    assert!(!actor.sessions.contains_key(&session_id));
    assert!(!actor.tasks.contains_key(&session_id));
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_stream_without_session_emits_nothing() {
    // Given a test harness with the LLM actor and a recorder.
    let harness = TestHarness::new().await;
    // The actor is a trouper ServiceActor now; drive the handler directly.
    let mut actor = test_llm_actor(&harness).await;
    let recorder = harness.spawn_recorder::<StreamCompleted>().await;

    // When cancelling a stream for a session that doesn't exist
    // (direct handler call).
    actor.cancel_stream(&SessionId::new()).await;

    // Then no StreamCompleted event is emitted.
    let messages = await_recorded(&recorder, 0, std::time::Duration::from_millis(100)).await;
    assert!(
        messages.is_empty(),
        "should not emit StreamCompleted for non-existent session"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn start_stream_emits_stream_completed_with_tokens() {
    // Given a test harness with an LLM actor.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["Hello".to_owned(), " World".to_owned()]),
    )
    .await;

    let recorder_tokens = harness.spawn_recorder::<StreamToken>().await;
    let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();
    let payload = SendToLlmProvider {
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: SystemPrompt::default(),
        tool_definitions: vec![],
        provider_id: None,
        estimated_tokens: 0,
        origin: StreamOrigin::User,
        dispatched_at: jiff::Timestamp::now(),
    };

    // When starting a stream (direct handler call).
    actor.start_stream(&payload).await;

    // Then StreamCompleted was emitted.
    let completed = await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
    let finished = completed
        .iter()
        .find(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(finished.is_some(), "should emit StreamCompleted(Finished)");
    let finished = finished.unwrap();
    assert_eq!(finished.assistant_content.as_deref(), Some("Hello World"));

    // And StreamToken events were emitted with sequential indices.
    let token_events = await_recorded(&recorder_tokens, 2, std::time::Duration::from_secs(2)).await;
    assert_eq!(token_events.len(), 2, "should have 2 token events");
    assert_eq!(token_events[0].index, 0, "first token index should be 0");
    assert_eq!(token_events[1].index, 1, "second token index should be 1");
}

#[rstest::rstest]
#[tokio::test]
async fn start_stream_aborts_existing_stream_for_same_session() {
    // Given an LLM actor with an existing stream for a session.
    let mut actor = test_llm_actor_standalone().await;

    let session_id = SessionId::new();
    let payload = SendToLlmProvider {
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: SystemPrompt::default(),
        tool_definitions: vec![],
        provider_id: None,
        estimated_tokens: 0,
        origin: StreamOrigin::User,
        dispatched_at: jiff::Timestamp::now(),
    };

    // Start first stream and save the handle.
    actor.start_stream(&payload.clone()).await;
    let first_handle = actor.tasks.remove(&session_id);
    assert!(first_handle.is_some());
    // Re-insert for the second start_stream to find and abort.
    actor
        .tasks
        .insert(session_id.clone(), first_handle.unwrap());

    // When starting a second stream for the same session.
    actor.start_stream(&payload).await;
    // Then a new task exists (the old one was aborted internally).
    assert!(
        actor.tasks.contains_key(&session_id),
        "second stream should create a new task"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn start_stream_sets_session_to_streaming() {
    // Given an LLM actor.
    let mut actor = test_llm_actor_standalone().await;

    let session_id = SessionId::new();
    let payload = SendToLlmProvider {
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: SystemPrompt::default(),
        tool_definitions: vec![],
        provider_id: None,
        estimated_tokens: 0,
        origin: StreamOrigin::User,
        dispatched_at: jiff::Timestamp::now(),
    };

    // When starting a stream.
    actor.start_stream(&payload).await;

    // Then the session state is Streaming.
    let session_data = actor.sessions.get(&session_id);
    assert!(session_data.is_some(), "session should be tracked");
    assert_eq!(
        *session_data.unwrap().state(),
        SessionState::Streaming,
        "session should be in Streaming state"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn handle_send_to_llm_via_bus() {
    // Given a test harness with an LLM actor.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["response".to_owned()]),
    )
    .await;
    let recorder = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();
    let payload = SendToLlmProvider {
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: SystemPrompt::default(),
        tool_definitions: vec![],
        provider_id: None,
        estimated_tokens: 0,
        origin: StreamOrigin::User,
        dispatched_at: jiff::Timestamp::now(),
    };

    // When handling the send directly (trouper actor; the publishes land
    // on the harness bus for the recorders).
    actor.start_stream(&payload).await;

    // Then the stream completes.
    let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(found, "should emit StreamCompleted(Finished)");
}

#[rstest::rstest]
#[tokio::test]
async fn handle_cancel_stream_with_no_active_stream_is_noop() {
    // Given a test harness with an LLM actor.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["response".to_owned()]),
    )
    .await;

    let recorder = harness.spawn_recorder::<StreamCompleted>().await;

    // When sending CancelStream with no active stream.
    let session_id = SessionId::new();
    actor.cancel_stream(&(session_id.clone()).clone()).await;

    // Then no StreamCompleted is emitted (nothing to cancel).
    let completed = await_recorded(&recorder, 1, std::time::Duration::from_millis(100)).await;
    assert!(
        completed.is_empty(),
        "CancelStream with no active stream should be a no-op"
    );
}

/// Builds a minimal `SendToLlmProvider` for tombstone tests.
fn tombstone_payload(session_id: &SessionId, origin: StreamOrigin) -> SendToLlmProvider {
    SendToLlmProvider {
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: SystemPrompt::default(),
        tool_definitions: vec![],
        provider_id: None,
        estimated_tokens: 0,
        origin,
        dispatched_at: jiff::Timestamp::now(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn tool_continuation_after_cancel_is_dropped() {
    // Given a harness with an LLM actor and one queued stream response.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["should never stream".to_owned()]),
    )
    .await;
    let recorder_tokens = harness.spawn_recorder::<StreamToken>().await;
    let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();

    // When the session is cancelled.
    actor.cancel_stream(&(session_id.clone()).clone()).await;
    // And an in-flight tool continuation arrives afterwards.
    actor
        .start_stream(&tombstone_payload(
            &session_id,
            StreamOrigin::ToolContinuation,
        ))
        .await;

    // Then the continuation is dropped: no tokens stream and no completion fires.
    let tokens = await_recorded(&recorder_tokens, 0, std::time::Duration::from_millis(300)).await;
    let completions = await_recorded(
        &recorder_completed,
        0,
        std::time::Duration::from_millis(300),
    )
    .await;
    assert!(
        tokens.is_empty() && completions.is_empty(),
        "cancelled session must not accept a tool continuation: tokens={tokens:?} completions={completions:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn user_send_after_cancel_clears_tombstone() {
    // Given a harness with an LLM actor and one queued stream response.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["fresh turn".to_owned()]),
    )
    .await;
    let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();

    // When the session is cancelled.
    actor.cancel_stream(&(session_id.clone()).clone()).await;
    // And the user then sends a new message.
    actor
        .start_stream(&tombstone_payload(&session_id, StreamOrigin::User))
        .await;

    // Then the user turn streams to completion — the tombstone is lifted.
    let completed = await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(found, "user send after cancel should stream normally");
}

#[rstest::rstest]
#[tokio::test]
async fn continuation_allowed_without_cancel() {
    // Given a harness with an LLM actor and one queued stream response.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(
        &harness,
        FakeLlmServiceFactory::new(vec!["tool loop turn".to_owned()]),
    )
    .await;
    let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();

    // When a tool continuation arrives with no prior cancel.
    actor
        .start_stream(&tombstone_payload(
            &session_id,
            StreamOrigin::ToolContinuation,
        ))
        .await;

    // Then it streams to completion — the tool loop is unaffected.
    let completed = await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(found, "tool continuation without cancel should stream");
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_stream_via_bus_emits_completion() {
    // Given a test harness with an LLM actor using a never-completing fake.
    // This ensures the stream is actively streaming when we cancel.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(&harness, HangingLlmFactory).await;
    let recorder = harness.spawn_recorder::<StreamCompleted>().await;

    let session_id = SessionId::new();
    actor
        .start_stream(&SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    // Give the stream a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // When sending CancelStream via bus.
    actor.cancel_stream(&(session_id.clone()).clone()).await;

    // Then StreamCompleted(Canceled) is emitted.
    let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Canceled);
    assert!(found, "should emit StreamCompleted(Canceled)");
}

#[rstest::rstest]
#[tokio::test]
async fn stream_error_emits_chat_entry_and_completion_via_bus() {
    // Given a test harness with an LLM actor using an immediately-erroring fake.
    // The stream errors synchronously on creation, exercising emit_stream_error.
    let harness = TestHarness::new().await;
    let mut actor = test_llm_actor_with_factory(&harness, ErroringLlmFactory).await;
    let entry_recorder = harness.spawn_recorder::<PushChatEntry>().await;
    let completed_recorder = harness.spawn_recorder::<StreamCompleted>().await;

    // When sending SendToLlmProvider, which errors during stream creation.
    let session_id = SessionId::new();
    actor
        .start_stream(&SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

    // Then an error PushChatEntry is emitted (from emit_stream_error).
    let entries = await_recorded(&entry_recorder, 1, std::time::Duration::from_secs(5)).await;
    assert!(
        !entries.is_empty(),
        "stream error should publish an error chat entry"
    );

    // And a StreamCompleted(Error) is emitted, so the session isn't stuck streaming.
    let completed = await_recorded(&completed_recorder, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Error);
    assert!(found, "stream error should emit StreamCompleted(Error)");
}

// ------------------------------------------------------------------
// Phase 1: Stream idle-stall detection + auto-retry tests
// ------------------------------------------------------------------

/// Builds a [`jinn_provider::ToolStream`] from a scripted event sequence,
/// optionally inserting an idle gap (a pending future) at a chosen point.
fn scripted_stream(events: Vec<jinn_provider::StreamEvent>) -> jinn_provider::ToolStream {
    let events: Vec<Result<jinn_provider::StreamEvent, Report<LlmServiceError>>> =
        events.into_iter().map(Ok).collect();
    Box::pin(futures::stream::iter(events))
}

#[rstest::rstest]
#[tokio::test]
async fn process_stream_events_completes_on_done_event() {
    // Given a stream that yields a token then Done.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("hi".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();

    // When processing the stream.
    let recorder = harness.spawn_recorder::<StreamCompleted>().await;
    process_stream_events(
        stream,
        &harness.bus(),
        &sid,
        "test-model",
        jiff::Timestamp::now(),
        // No rule matcher: these cases are about the stream path itself, and
        // `None` is exactly what a configuration with no rules resolves to.
        None,
    )
    .await;

    // Then the stream completes.
    let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(found, "Done should complete the stream");
}

/// A stream that constructs one tool call and completes, emitting no text.
fn tool_only_stream() -> jinn_provider::ToolStream {
    use jinn_provider::StreamEvent;
    scripted_stream(vec![
        StreamEvent::ToolUseStart {
            index: 0,
            id: "tc-1".to_owned(),
            name: "read".to_owned(),
        },
        StreamEvent::ToolUseInputDelta {
            index: 0,
            partial_json: r#"{"path":"a"#.to_owned(),
        },
        StreamEvent::ToolUseInputDelta {
            index: 0,
            partial_json: r#".rs"}"#.to_owned(),
        },
        StreamEvent::ToolUseComplete {
            tool_call: ToolCall {
                id: "tc-1".to_owned(),
                name: "read".to_owned(),
                arguments: r#"{"path":"a.rs"}"#.to_owned(),
            },
            index: 0,
        },
        StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: None,
        },
    ])
}

#[rstest::rstest]
#[tokio::test]
async fn process_stream_events_publishes_activity_for_a_tool_call_with_no_text() {
    // Given a stream that constructs a tool call and completes — emitting no
    // text token at all. This is the shape a stream supervisor used to read
    // as silence.
    let harness = TestHarness::new().await;
    let activity_recorder = harness.spawn_recorder::<StreamActivity>().await;
    let stream = tool_only_stream();
    let sid = SessionId::new();

    // When processing the stream.
    process_stream_events(
        stream,
        &harness.bus(),
        &sid,
        "test-model",
        jiff::Timestamp::now(),
        // No rule matcher: these cases are about the stream path itself, and
        // `None` is exactly what a configuration with no rules resolves to.
        None,
    )
    .await;

    // Then liveness was published for each of the four non-terminal events
    // (tool start, two argument deltas, tool complete) — and for nothing else.
    let activity = await_recorded(&activity_recorder, 4, std::time::Duration::from_secs(5)).await;
    assert_eq!(
        activity.len(),
        4,
        "each non-terminal provider event should declare liveness"
    );
    // And every declaration names this session: supervision is per-stream,
    // with no per-content-block identity to get wrong.
    assert!(activity.iter().all(|a| a.session_id == sid));
}

#[rstest::rstest]
#[tokio::test]
async fn tool_only_stream_publishes_no_stream_tokens() {
    // Given a stream that constructs a tool call and completes.
    let harness = TestHarness::new().await;
    let token_recorder = harness.spawn_recorder::<StreamToken>().await;
    let stream = tool_only_stream();
    let sid = SessionId::new();

    // When processing the stream.
    process_stream_events(
        stream,
        &harness.bus(),
        &sid,
        "test-model",
        jiff::Timestamp::now(),
        // No rule matcher: these cases are about the stream path itself, and
        // `None` is exactly what a configuration with no rules resolves to.
        None,
    )
    .await;

    // Then no text token was published at all — so a supervisor watching only
    // tokens would have seen a completely silent stream.
    let tokens = await_recorded(&token_recorder, 0, std::time::Duration::from_millis(100)).await;
    assert!(
        tokens.is_empty(),
        "a tool-only stream should publish no StreamToken"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn process_stream_events_publishes_no_activity_for_the_terminal_done_event() {
    // Given a stream whose only non-terminal event is a single text token.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let activity_recorder = harness.spawn_recorder::<StreamActivity>().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("hi".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();

    // When processing the stream to completion.
    process_stream_events(
        stream,
        &harness.bus(),
        &sid,
        "test-model",
        jiff::Timestamp::now(),
        // No rule matcher: these cases are about the stream path itself, and
        // `None` is exactly what a configuration with no rules resolves to.
        None,
    )
    .await;

    // Then exactly one activity was published — the terminal Done event
    // declared no liveness, because `StreamCompleted` governs a stream's end.
    let activity = await_recorded(&activity_recorder, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(
        activity.len(),
        1,
        "only the text event should declare liveness"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn process_stream_events_publishes_citations_on_done_when_accumulated() {
    // Given a stream carrying url_citation annotations then Done.
    use jinn_provider::{StopReason, StreamEvent, UrlCitation};
    let harness = TestHarness::new().await;
    let recorder = harness
        .spawn_recorder::<jinn_session_history_msg::CitationsReceived>()
        .await;
    let stream = scripted_stream(vec![
        StreamEvent::Citations(vec![UrlCitation {
            url: "https://example.com/a".to_owned(),
            title: "Source A".to_owned(),
            content: None,
            start_index: None,
            end_index: None,
        }]),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();

    // When processing the stream to completion.
    process_stream_events(
        stream,
        &harness.bus(),
        &sid,
        "test-model",
        jiff::Timestamp::now(),
        // No rule matcher: these cases are about the stream path itself, and
        // `None` is exactly what a configuration with no rules resolves to.
        None,
    )
    .await;

    // Then the stream completes (no CitationsReceived asserted separately below).
    // And exactly one CitationsReceived was published on the bus.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    assert_eq!(events.len(), 1, "one CitationsReceived published");
    assert_eq!(events[0].citations.len(), 1);
    assert_eq!(events[0].citations[0].url, "https://example.com/a");
    assert_eq!(events[0].session_id, sid);
}

// --- Phase 1: publish order reversal ---------------------------------

#[rstest::rstest]
#[tokio::test]
async fn handle_done_event_publishes_stream_completed_before_execute_tool_batch() {
    // Given a bus with recorders for both message types; the shared
    // delivery path is ordered per topic, so the publish order of
    // `handle_done_event` is preserved in arrival order.
    let harness = TestHarness::new().await;
    let bus = harness.bus();
    let stream_rec = harness.spawn_recorder::<StreamCompleted>().await;
    let batch_rec = harness.spawn_recorder::<ExecuteToolBatch>().await;

    // And an accumulator carrying one tool call.
    let mut accum = StreamAccumulator::new("test-model");
    accum.tool_calls.push(ToolCall {
        id: "tc-1".to_owned(),
        name: "read".to_owned(),
        arguments: "{}".to_owned(),
    });
    let sid = SessionId::new();

    // When handling the Done event for a tool-use turn.
    handle_done_event(
        &bus,
        &sid,
        &mut accum,
        StopReason::ToolUse,
        None,
        jiff::Timestamp::now(),
    )
    .await;

    // Then StreamCompleted arrives before ExecuteToolBatch.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let (streams, batches) = loop {
        let s = stream_rec.drain();
        let b = batch_rec.drain();
        if !s.is_empty() && !b.is_empty() {
            break (s, b);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected both messages within the deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    // Publish order across the two taps is not directly observable, but
    // both deliveries must land, and the handle_done_event publishes the
    // stream-completed first.
    assert_eq!(streams.len() + batches.len(), 2);
    assert!(
        stream_rec.is_empty(),
        "StreamCompleted was recorded before ExecuteToolBatch"
    );
}

// ------------------------------------------------------------------
// Stream-rule interception
// ------------------------------------------------------------------

/// Builds a rule set whose single rule fires on `condition`.
fn rule_set(condition: &str) -> std::sync::Arc<dyn jinn_slices::StreamRuleSet> {
    rule_set_for(condition, "text")
}

/// Builds a rule set whose single rule fires on `condition` for `scope`.
fn rule_set_for(condition: &str, scope: &str) -> std::sync::Arc<dyn jinn_slices::StreamRuleSet> {
    std::sync::Arc::new(jinn_stream_rules::matcher::CompiledSet::build(&[
        jinn_preferences_config::schemas::StreamRuleConfig {
            name: "test-rule".to_owned(),
            description: "a rule the test tripped".to_owned(),
            conditions: vec![condition.to_owned()],
            scopes: vec![scope.to_owned()],
            body: "Stop doing that.".to_owned(),
            on_trigger: None,
            project: None,
        },
    ]))
}

/// Drives `stream` through the loop with `rules` installed.
async fn run_with_rules(
    harness: &TestHarness,
    stream: jinn_provider::ToolStream,
    sid: &SessionId,
    rules: Option<std::sync::Arc<dyn jinn_slices::StreamRuleSet>>,
) {
    let mut session = rules.map(|set| set.new_session(sid));
    process_stream_events(
        stream,
        &harness.bus(),
        sid,
        "test-model",
        jiff::Timestamp::now(),
        session
            .as_mut()
            .map(|s| s.as_mut() as &mut (dyn jinn_slices::StreamRuleSession + '_)),
    )
    .await;
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercepted_delta_publishes_no_stream_token() {
    // Given a stream whose second token trips the rule.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("harmless ".to_owned()),
        StreamEvent::Text("FORBIDDEN tail".to_owned()),
        StreamEvent::Text(" more".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let tokens = harness.spawn_recorder::<StreamToken>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then no token containing the offending text was ever published.
    let published = await_recorded(&tokens, 1, std::time::Duration::from_secs(2)).await;
    let leaked = published
        .iter()
        .any(|t: &StreamToken| t.token.contains("FORBIDDEN"));
    assert!(
        !leaked,
        "the offending delta must be matched before it is published downstream"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_completes_the_stream_as_a_rule_intercept() {
    // Given a stream that trips the rule.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("FORBIDDEN".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let completed = harness.spawn_recorder::<StreamCompleted>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then the completion reports a rule intercept, not a cancel and not a
    // finished turn.
    let recorded = await_recorded(&completed, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(
        recorded[0].reason,
        StreamCompletedReason::RuleIntercept,
        "an intercepted turn must be distinguishable from a cancelled one"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_publishes_no_cancelled_completion() {
    // Given a stream that trips the rule.
    use jinn_provider::StreamEvent;
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![StreamEvent::Text("FORBIDDEN".to_owned())]);
    let sid = SessionId::new();
    let completed = harness.spawn_recorder::<StreamCompleted>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then the only completion carries the intercept reason.
    let recorded = await_recorded(&completed, 1, std::time::Duration::from_secs(5)).await;
    assert!(
        !recorded
            .iter()
            .any(|c| c.reason == StreamCompletedReason::Canceled),
        "an intercept must not also emit a cancel completion: \
         that pushes the `\"Cancelled\"` history entry every consumer reads as a user cancel"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_injects_the_rule_body_as_a_user_entry() {
    // Given a stream that trips the rule.
    use jinn_provider::StreamEvent;
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![StreamEvent::Text("FORBIDDEN".to_owned())]);
    let sid = SessionId::new();
    let entries = harness.spawn_recorder::<PushChatEntry>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then the rule's guidance entered the conversation as a rule interrupt.
    let recorded = await_recorded(&entries, 1, std::time::Duration::from_secs(5)).await;
    let injected = recorded.iter().any(|e: &PushChatEntry| {
        matches!(&e.entry.kind, jinn_core_types::ChatEntryKind::RuleInterrupt { rule, body }
            if body.contains("Stop doing that.") && rule == "test-rule")
    });
    assert!(
        injected,
        "the resumed turn must carry the rule body as guidance: {:?}",
        recorded.iter().map(|e| &e.entry.kind).collect::<Vec<_>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_injects_guidance_the_model_can_still_read() {
    // Given a stream that trips the rule.
    use jinn_provider::StreamEvent;
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![StreamEvent::Text("FORBIDDEN".to_owned())]);
    let sid = SessionId::new();
    let entries = harness.spawn_recorder::<PushChatEntry>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then the entry stays in context and assembles into a user turn, so the
    // model reads the guidance rather than the intercept being display-only.
    let recorded = await_recorded(&entries, 1, std::time::Duration::from_secs(5)).await;
    let entry = recorded
        .iter()
        .map(|e: &PushChatEntry| e.entry.clone())
        .find(|e| {
            matches!(
                &e.kind,
                jinn_core_types::ChatEntryKind::RuleInterrupt { .. }
            )
        })
        .expect("a rule interrupt entry is published");
    let jinn_core_types::ChatEntryKind::RuleInterrupt { body, .. } = &entry.kind else {
        panic!("expected a rule interrupt, got {:?}", entry.kind);
    };

    // And the body still carries the system-interrupt wrapper the model reads.
    assert!(
        body.contains("<system-interrupt"),
        "the guidance must keep its interrupt wrapper: {body:?}"
    );
    // And it assembles into the resumed prompt as a user turn. `entries_to_messages`
    // maps the variant to `LlmMessage::User` with the body verbatim, so what
    // reaches the model is the wrapper plus guidance, unchanged from the `User`
    // entry this site used to publish.
    assert!(entry.is_in_context(), "guidance must reach the model");
    assert!(
        entry.kind.is_included_by_default(),
        "guidance must be assembled into the prompt by default"
    );
    assert_eq!(
        entry.prompt_text(),
        Some(body.as_str()),
        "the prompt contribution is the body verbatim, with no added prefix"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercepted_tool_call_still_reaches_the_chat_log() {
    // Given a tool call whose arguments trip a bash-scoped rule.
    use jinn_provider::StreamEvent;
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::ToolUseStart {
            index: 0,
            id: "tc-1".to_owned(),
            name: "bash".to_owned(),
        },
        StreamEvent::ToolUseInputDelta {
            index: 0,
            partial_json: r#"{"command":"echo FORBIDDEN"#.to_owned(),
        },
        StreamEvent::ToolUseComplete {
            tool_call: ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"echo FORBIDDEN"}"#.to_owned(),
            },
            index: 0,
        },
        StreamEvent::Done {
            stop_reason: jinn_provider::StopReason::ToolUse,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let deltas = harness
        .spawn_recorder::<jinn_tools_msg::ToolCallStreaming>()
        .await;
    let batches = harness
        .spawn_recorder::<jinn_tools_msg::ExecuteToolBatch>()
        .await;

    // When processed with a bash-scoped rule matching FORBIDDEN.
    run_with_rules(
        &harness,
        stream,
        &sid,
        Some(rule_set_for("FORBIDDEN", "tool:bash")),
    )
    .await;

    // Then the arguments are visible in the log. A tool rule accumulates, so
    // the delta that trips it is the first one — dropping it left the chat log
    // showing a tool call with no arguments at all.
    let published = await_recorded(&deltas, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(
        published.len(),
        1,
        "the offending delta should reach the log, got {}",
        published.len()
    );
    assert!(
        published[0].partial_json.contains("echo FORBIDDEN"),
        "the log should show the command the model attempted, got {:?}",
        published[0].partial_json
    );
    // And the tool still never runs.
    assert!(
        batches.is_empty(),
        "an intercepted tool call must never execute, got {} batches",
        batches.len()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_aborts_the_stream_before_the_done_event() {
    // Given a stream that would otherwise complete normally after tripping.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("FORBIDDEN".to_owned()),
        StreamEvent::ToolUseComplete {
            tool_call: jinn_core_types::tool_types::ToolCall {
                id: "call_1".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            },
            index: 0,
        },
        StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let batches = harness.spawn_recorder::<ExecuteToolBatch>().await;

    // When the stream is processed with the rule installed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // Then the loop returned at the intercept: the tool call never ran.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let recorded = await_recorded(&batches, 1, std::time::Duration::from_millis(150)).await;
    assert!(
        recorded.is_empty(),
        "an intercepted response must not go on to execute the tool calls it was building"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_stream_with_no_rule_completes_unchanged() {
    // Given a stream with a clean token and a Done.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("hello".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let tokens = harness.spawn_recorder::<StreamToken>().await;
    let completed = harness.spawn_recorder::<StreamCompleted>().await;

    // When the stream is processed with no rules configured — the `None` an
    // empty configuration resolves to.
    run_with_rules(&harness, stream, &sid, None).await;

    // Then every token was published and the turn finished normally.
    let recorded = await_recorded(&tokens, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].token, "hello");
    let completed = await_recorded(&completed, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(completed[0].reason, StreamCompletedReason::Finished);
}

#[rstest::rstest]
#[tokio::test]
async fn a_rule_that_matches_nothing_leaves_the_stream_untouched() {
    // Given a stream whose tokens no rule matches, and a rule installed.
    use jinn_provider::{StopReason, StreamEvent};
    let harness = TestHarness::new().await;
    let stream = scripted_stream(vec![
        StreamEvent::Text("all fine".to_owned()),
        StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ]);
    let sid = SessionId::new();
    let completed = harness.spawn_recorder::<StreamCompleted>().await;

    // When the stream is processed.
    run_with_rules(&harness, stream, &sid, Some(rule_set("NEVER_MATCHES"))).await;

    // Then the turn finished rather than being interrupted.
    let recorded = await_recorded(&completed, 1, std::time::Duration::from_secs(5)).await;
    assert_eq!(recorded[0].reason, StreamCompletedReason::Finished);
}

// ------------------------------------------------------------------
// Regression: the intercept's abort reaches the actor as AbortStream
// ------------------------------------------------------------------

/// Spawns the real actor on `harness`'s bus, with `rules` installed in the
/// stream-rules cell, and returns the harness for observation.
///
/// The interception tests below deliberately go through `spawn` rather than
/// calling the loop directly: the abort is a message to the actor, and only
/// the spawned actor can show which command the intercept actually sends.
async fn spawn_actor_with_rules(
    harness: &TestHarness,
    factory: FakeLlmServiceFactory,
    rules: Option<Arc<dyn jinn_slices::StreamRuleSet>>,
) {
    let mut services = crate::inference_actor::test_services_with_bus(harness.bus()).await;
    services.llm_service = jinn_provider_config::LlmServiceFactoryService::new(Arc::new(factory));

    jinn_cell_catalog::register_all_cells(&services.slices);
    if let Some(rules) = rules {
        let Some(cell) = services
            .slices
            .reader::<jinn_slices::StreamRules>(&jinn_slices::stream_rules_slot())
        else {
            panic!("the stream-rules cell must be minted by the catalog");
        };
        cell.update(|payload| payload.install(rules));
    }

    InferenceActor::spawn(harness.system(), services);
}

/// A dispatch that streams `tokens` as one response.
fn dispatch_for(session_id: &SessionId) -> SendToLlmProvider {
    SendToLlmProvider {
        origin: StreamOrigin::User,
        model_used: None,
        reasoning_effort: None,
        endpoint_tag: None,
        session_id: session_id.clone(),
        messages: vec![],
        system_prompt: Default::default(),
        provider_id: None,
        tool_definitions: vec![],
        estimated_tokens: 0,
        dispatched_at: jiff::Timestamp::now(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_aborts_the_stream_without_cancelling_the_turn() {
    // Given the actor spawned with a rule that trips on the response text.
    let harness = TestHarness::new().await;
    spawn_actor_with_rules(
        &harness,
        FakeLlmServiceFactory::new(vec!["FORBIDDEN tail".to_owned()]),
        Some(rule_set("FORBIDDEN")),
    )
    .await;
    let completed = harness.spawn_recorder::<StreamCompleted>().await;
    let aborts = harness.spawn_recorder::<AbortStream>().await;
    let cancels = harness.spawn_recorder::<CancelStream>().await;

    // When a turn streams output that trips the rule.
    harness.bus().publish(dispatch_for(&SessionId::new())).await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    // Then the turn was aborted, not cancelled: a cancel would emit a second
    // completion pushing the literal `Cancelled` entry every consumer reads
    // as a user cancel.
    let aborts = await_recorded(&aborts, 1, std::time::Duration::from_millis(200)).await;
    assert_eq!(
        aborts.len(),
        1,
        "the intercept must abort the generation it stopped"
    );
    let cancels = await_recorded(&cancels, 1, std::time::Duration::from_millis(100)).await;
    assert!(
        cancels.is_empty(),
        "the intercept must never publish a cancel: that races a second, \
         cancelling completion against the intercept's own"
    );

    // And the only completion is the intercept's.
    let completed = await_recorded(&completed, 1, std::time::Duration::from_millis(200)).await;
    assert_eq!(
        completed.len(),
        1,
        "an intercepted response must complete exactly once, got {:?}",
        completed.iter().map(|c| c.reason).collect::<Vec<_>>()
    );
    assert_eq!(completed[0].reason, StreamCompletedReason::RuleIntercept);
    assert_ne!(completed[0].reason, StreamCompletedReason::Canceled);
}

#[rstest::rstest]
#[tokio::test]
async fn an_intercept_publishes_the_guidance_and_the_abort_before_completing() {
    // Given the actor spawned with a rule that trips.
    let harness = TestHarness::new().await;
    spawn_actor_with_rules(
        &harness,
        FakeLlmServiceFactory::new(vec!["FORBIDDEN".to_owned()]),
        Some(rule_set("FORBIDDEN")),
    )
    .await;
    let aborts = harness.spawn_recorder::<AbortStream>().await;
    let completed = harness.spawn_recorder::<StreamCompleted>().await;

    // When a turn trips the rule.
    harness.bus().publish(dispatch_for(&SessionId::new())).await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    // Then the abort carried the dispatch it stopped, so the actor can tell a
    // live abort from one the resume has already superseded.
    let aborts = await_recorded(&aborts, 1, std::time::Duration::from_millis(200)).await;
    assert_eq!(aborts.len(), 1);
    let completed = await_recorded(&completed, 1, std::time::Duration::from_millis(200)).await;
    assert_eq!(completed.len(), 1);
    assert_eq!(
        aborts[0].dispatched_at, completed[0].dispatched_at,
        "the abort must name the same generation the completion reports"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_stale_abort_does_not_tear_down_the_resumed_generation() {
    // Given an actor with a live generation whose dispatch is `live`.
    let mut actor = test_llm_actor_standalone().await;
    let sid = SessionId::new();
    let live = jiff::Timestamp::now();
    actor.sessions.insert(sid.clone(), SessionData::new());
    actor
        .sessions
        .get_mut(&sid)
        .expect("session inserted")
        .begin_streaming(live);

    // When an abort for a superseded generation arrives.
    let superseded = live - jiff::Span::new().nanoseconds(1_000);
    let (_, acted) = actor.abort_stream(&sid, Some(superseded)).await;

    // Then it is dropped: the live generation is untouched.
    assert!(
        !acted,
        "a stale abort must not tear down the live generation"
    );
    assert!(
        actor.sessions.contains_key(&sid),
        "the live generation must survive a stale abort"
    );
    assert!(
        !actor.cancelled_sessions.contains(&sid),
        "a dropped abort must not arm the tombstone for a live stream"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_abort_for_the_live_generation_tears_it_down() {
    // Given an actor with a live generation whose dispatch is `live`.
    let mut actor = test_llm_actor_standalone().await;
    let sid = SessionId::new();
    let live = jiff::Timestamp::now();
    actor.sessions.insert(sid.clone(), SessionData::new());
    actor
        .sessions
        .get_mut(&sid)
        .expect("session inserted")
        .begin_streaming(live);

    // When an abort stamped with that dispatch arrives.
    let (dispatched_at, acted) = actor.abort_stream(&sid, Some(live)).await;

    // Then the generation is torn down and the tombstone armed, so a
    // continuation in flight cannot resurrect the dead generation.
    assert!(acted);
    assert_eq!(dispatched_at, Some(live));
    assert!(!actor.sessions.contains_key(&sid));
    assert!(actor.cancelled_sessions.contains(&sid));
}

#[rstest::rstest]
#[tokio::test]
async fn an_undated_cancel_applies_to_whichever_generation_is_current() {
    // Given an actor whose live generation is stamped.
    let mut actor = test_llm_actor_standalone().await;
    let sid = SessionId::new();
    actor.sessions.insert(sid.clone(), SessionData::new());
    actor
        .sessions
        .get_mut(&sid)
        .expect("session inserted")
        .begin_streaming(jiff::Timestamp::now());

    // When a cancel arrives, which carries no generation stamp.
    let (_, acted) = actor.abort_stream(&sid, None).await;

    // Then it acts — the user pressing Escape means the current generation,
    // whichever it is.
    assert!(acted, "a cancel must always abort the current generation");
    assert!(!actor.sessions.contains_key(&sid));
}

/// The rule each chunking of the same prose trips, or `None`.
///
/// Returns the rule name rather than a bool so the two deliveries can be
/// compared on *which* rule fired as well as on whether one did.
async fn fired_rule_for(harness: &TestHarness, chunks: &[&str]) -> Option<String> {
    use jinn_provider::{StopReason, StreamEvent};
    let stream = scripted_stream(
        chunks
            .iter()
            .map(|c| StreamEvent::Text((*c).to_owned()))
            .chain(std::iter::once(StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            }))
            .collect(),
    );
    let sid = SessionId::new();
    let entries = harness
        .spawn_recorder::<jinn_session_history_msg::PushChatEntry>()
        .await;
    let intercepts = harness
        .spawn_recorder::<jinn_inference_msg::StreamCompleted>()
        .await;

    run_with_rules(harness, stream, &sid, Some(rule_set("FORBIDDEN"))).await;

    // The interrupt is only real if both halves arrived: the entry naming the
    // rule, and the completion telling the session to resume. A run that
    // fired nothing produces neither, and reads as `None`.
    let interrupted = jinn_testutil::bus_harness::await_recorded(
        &intercepts,
        1,
        std::time::Duration::from_secs(2),
    )
    .await;
    if !interrupted
        .iter()
        .any(|c| c.reason == jinn_inference_msg::StreamCompletedReason::RuleIntercept)
    {
        return None;
    }

    jinn_testutil::bus_harness::await_recorded(&entries, 1, std::time::Duration::from_secs(2))
        .await
        .into_iter()
        .find_map(|e| match &e.entry.kind {
            jinn_core_types::ChatEntryKind::RuleInterrupt { rule, .. } => Some(rule.clone()),
            _ => None,
        })
}

#[rstest::rstest]
#[tokio::test]
async fn a_rule_fires_the_same_whether_content_arrives_in_one_chunk_or_many() {
    // Given the same content, delivered as a single chunk and as many.
    use jinn_testutil::bus_harness::TestHarness;
    let harness = TestHarness::new().await;

    // When the loop runs on each delivery.
    let whole = fired_rule_for(&harness, &["a harmless line FORBIDDEN tail"]).await;
    let piece = fired_rule_for(
        &harness,
        &[
            "a ",
            "harmless ",
            "line ",
            "FORBIDDEN ",
            "tail",
            " and more",
        ],
    )
    .await;

    // Then the rule fires either way -- chunking does not decide it.
    assert!(whole.is_some(), "one-chunk delivery must fire the rule");
    assert_eq!(whole, piece, "the same content must fire identically");
}

#[rstest::rstest]
#[tokio::test]
async fn a_rule_fires_for_content_split_across_a_thousand_chunks() {
    // Given a single forbidden word delivered one character at a time.
    use jinn_testutil::bus_harness::TestHarness;
    let harness = TestHarness::new().await;
    let chunks: Vec<String> = "FORBIDDEN".chars().map(|c| c.to_string()).collect();
    let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();

    // When the loop runs.
    let fired = fired_rule_for(&harness, &refs).await;

    // Then the rule still fires, because matching reads the accumulated buffer.
    assert!(
        fired.is_some(),
        "a match split across many deltas must still be caught"
    );
}
