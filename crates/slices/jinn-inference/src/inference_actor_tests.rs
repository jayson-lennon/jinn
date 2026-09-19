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

use jinn_domain::common::bus::test_harness::{TestHarness, await_recorded};
use jinn_provider::FakeLlmServiceFactory;

/// Actor over its own private fake bus — for tests that only inspect
/// actor fields (no publish observability needed).
async fn test_llm_actor_standalone() -> InferenceActor {
    let services = jinn_domain::common::services::Services::new_fake().await;
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
    services.llm_service =
        jinn_domain::feat::provider_infra::LlmServiceFactoryService::new(Arc::new(factory));
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
    )
    .await;

    // Then the stream completes.
    let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
    let found = completed
        .iter()
        .any(|sc| sc.reason == StreamCompletedReason::Finished);
    assert!(found, "Done should complete the stream");
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
//
// A single actor that records the arrival order of `StreamCompleted` and
// `ExecuteToolBatch` into a shared `Arc<Mutex<Vec<String>>>`. Because kameo's
// bus delivers each message as a separate mailbox message, the order of
// `publish` calls in `handle_done_event` is preserved in arrival order.
struct OrderLog(std::sync::Mutex<Vec<&'static str>>);

impl kameo::Actor for OrderLog {
    type Args = ();
    type Error = kameo::error::Infallible;
    async fn on_start(
        _args: Self::Args,
        _actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self(std::sync::Mutex::new(Vec::new())))
    }
}

impl kameo::prelude::Message<StreamCompleted> for OrderLog {
    type Reply = ();
    async fn handle(
        &mut self,
        _msg: StreamCompleted,
        _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
    ) {
        self.0.lock().unwrap().push("StreamCompleted");
    }
}

impl kameo::prelude::Message<ExecuteToolBatch> for OrderLog {
    type Reply = ();
    async fn handle(
        &mut self,
        _msg: ExecuteToolBatch,
        _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
    ) {
        self.0.lock().unwrap().push("ExecuteToolBatch");
    }
}
struct GetOrder;
impl kameo::prelude::Message<GetOrder> for OrderLog {
    type Reply = Vec<&'static str>;
    async fn handle(
        &mut self,
        _msg: GetOrder,
        _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.0.lock().unwrap().clone()
    }
}

#[rstest::rstest]
#[tokio::test]
async fn handle_done_event_publishes_stream_completed_before_execute_tool_batch() {
    // Given a bus with an OrderLog recording both message types.
    use kameo::actor::Spawn;
    let harness = TestHarness::new().await;
    let bus = harness.bus();
    let order_actor = OrderLog::spawn(());
    harness
        .register(order_actor.clone().recipient::<StreamCompleted>())
        .await;
    harness
        .register(order_actor.clone().recipient::<ExecuteToolBatch>())
        .await;

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
    let recorded = loop {
        let v: Vec<&'static str> = order_actor.ask(GetOrder).await.expect("get order");
        if v.len() == 2 {
            break v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(recorded, vec!["StreamCompleted", "ExecuteToolBatch"]);
}
