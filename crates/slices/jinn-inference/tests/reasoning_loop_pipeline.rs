//! The reasoning-loop pipeline, composed end to end on one bus.
//!
//! Spawns the pipeline's real actors over a [`jinn_testutil::bus_harness::TestHarness`]
//! — context assembly, turn dispatch, the session actor, the reasoning-loop
//! watchdog, and the inference actor with a scripted provider — and drives a
//! looping turn through the full message graph: enqueue → dispatch mint →
//! provider stream → loop detection → completion → watchdog marker → silent
//! retry → (budget) surrender marker + cancel. The fake provider hands the
//! mantra loop sample's signature to every request, so each retry loops
//! again, exactly as the captured incident samples show.
//!
//! This is the full integration the unit suites cannot be: the watchdog's
//! marker text is not asserted anywhere else against the *published*
//! [`PushChatEntry`], and the queue's `Resume` mint — the step that turns
//! the retry handler's `DispatchTurn` into a fresh `SendToLlmProvider` —
//! runs nowhere else at all.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code: a missing fixture is a failed test"
)]

use std::sync::Arc;

use jinn_cell_catalog::register_all_cells;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::ChatEntryKind;
use jinn_inference_msg::{CancelTurn, SendToLlmProvider, StreamCompleted, StreamCompletedReason};
use jinn_llm_support::token_estimator::TiktokenCounter;
use jinn_provider::{FakeLlmServiceFactory, ScriptedResponse};
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::{TurnCompleted, TurnOutcome};
use jinn_session_turn::session_actor::SessionPersistenceActor;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;
use jinn_turn_dispatch::queue_actor::QueueActor;
use jinn_turn_dispatch_msg::DispatchTurn;
use jinn_watchdog::reasoning_loop_watchdog_actor::{
    ReasoningLoopWatchdogActor, ReasoningLoopWatchdogActorDeps,
};

use jinn_kernel::common::services::Services;
use jinn_kernel::common::state::State;

/// The captured assistant-response-loop sample's signature: a ~194-char
/// planning mantra. Repeated 6 times per scripted response, it clears the
/// detector thresholds (5 repeats, 400 bytes) with margin.
const MANTRA: &str = "Let me carefully re-examine the situation. \
     I should reflect on what has been done so far. \
     The next step is to proceed methodically and without haste. \
     I need to remain focused on the task at hand. ";

/// Varied prose past the run threshold — the negative control stream for
/// the debit test.
const VARIED_PROSE: &str = "The cache layer invalidates entries on write, not on read. \
     Config lives in two files, and only one of them is user-facing. \
     Sessions persist across restarts, while in-flight turns do not. \
     The renderer draws on a fixed tick and skips frames when busy. \
     Tool output is truncated twice, by lines first and then by bytes. \
     A session id names the storage shard, and a turn token names the attempt.";

/// The composed pipeline: every actor on one harness bus plus the handles
/// the scenarios assert through.
struct Pipeline {
    harness: TestHarness,
    state: State,
    factory: Arc<FakeLlmServiceFactory>,
    session_id: jinn_core_types::SessionId,
}

/// Spawns the pipeline the way composition does — production order, one
/// shared `Services`/`State` — with a scripted provider whose response
/// queue the scenario fills. `EnqueueUserMessage` is the sole entry: no
/// `SendToLlmProvider` is published directly, so every stream the test
/// observes was minted and dispatched by the real queue actor.
async fn pipeline(scripted: Vec<ScriptedResponse>) -> Pipeline {
    let harness = TestHarness::new().await;
    let services = {
        let bus = harness.bus();
        Services::new_fake_with_bus(bus).await
    };
    let factory = Arc::new(FakeLlmServiceFactory::new(vec![]));
    let services = {
        let mut services = services;
        services.llm_service = jinn_provider_config::LlmServiceFactoryService::new(factory.clone());
        services
    };
    let state = State::new(jinn_kernel::AppState::default_with_scope_focus());

    register_all_cells(&services.slices);
    // The queue's assembly ask addresses `services.trouper_system` — the
    // system `Services` carries, not the harness bus's broadcast system.
    // Pipeline actors subscribe on the harness system (bus broadcasts);
    // the ask service lives where asks are addressed.
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
    QueueActor::spawn(harness.system(), state.clone(), services.clone());
    SessionPersistenceActor::spawn(
        harness.system(),
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            counter: TiktokenCounter::o200k_base(),
            token_cache: HistoryWorkerChatEntryTokenCache::default(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::unavailable(),
        },
    );
    ReasoningLoopWatchdogActor::spawn(
        harness.system(),
        ReasoningLoopWatchdogActorDeps {
            services: services.clone(),
            max_failures: 3,
        },
    );
    jinn_inference::inference_actor::InferenceActor::spawn(
        harness.system(),
        state.clone(),
        services.clone(),
    );

    let session_id = state.read().session.active_session_id().clone();

    // The response queue is filled by the caller through the shared
    // factory — `create()` clones the factory's Arc'd queue, so every
    // spawned service pops from the one scenario-owned list.
    for resp in scripted {
        factory.push_scripted_response(resp);
    }

    Pipeline {
        harness,
        state,
        factory,
        session_id,
    }
}

/// Enqueues the user message that starts the turn, the production entry.
async fn start_turn(pipeline: &Pipeline, text: &str) {
    pipeline
        .harness
        .publish(EnqueueUserMessage {
            session_id: pipeline.session_id.clone(),
            entry: jinn_core_types::ChatEntry::user(text),
        })
        .await;
}

/// Pushes one looping scripted response, in case a scenario needs to
/// re-arm the provider mid-scenario.
#[expect(dead_code, reason = "scenario helper kept for follow-up scenarios")]
fn push_loop(pipeline: &Pipeline) {
    pipeline.factory.push_scripted_response(ScriptedResponse {
        tokens: mantra_chunks(),
        tool_calls: vec![],
    });
}

/// The mantra script chunked the way a provider chunks text deltas.
fn mantra_chunks() -> Vec<String> {
    let run = MANTRA.repeat(6);
    run.as_bytes()
        .chunks(32)
        .map(|chunk| String::from_utf8(chunk.to_vec()).expect("ascii chunks"))
        .collect()
}

/// Drains a recorder's full buffer — a snapshot helper, since
/// `await_recorded` empties as it reads.
fn drain_all<M: Clone + Send + 'static>(
    recorder: &jinn_testutil::bus_harness::Recorder<M>,
) -> Vec<M> {
    let mut all = Vec::new();
    loop {
        let batch = recorder.drain();
        if batch.is_empty() {
            return all;
        }
        all.extend(batch);
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_looping_turn_retries_silently_with_a_visible_marker() {
    // Given the composed pipeline whose provider always loops.
    let pipeline = pipeline(vec![ScriptedResponse {
        tokens: mantra_chunks(),
        tool_calls: vec![],
    }])
    .await;
    let completions = pipeline.harness.spawn_recorder::<StreamCompleted>().await;
    let entries = pipeline.harness.spawn_recorder::<PushChatEntry>().await;
    let dispatches = pipeline.harness.spawn_recorder::<DispatchTurn>().await;
    let outcomes = pipeline.harness.spawn_recorder::<TurnCompleted>().await;
    let sends = pipeline.harness.spawn_recorder::<SendToLlmProvider>().await;

    // When the user's message starts the turn and the stream retries once
    // (the outcome of the FIRST loop is the terminal event of this
    // scenario: LoopRetried is the retry handler's last publish).
    start_turn(&pipeline, "do the thing").await;
    let outcomes = await_recorded(&outcomes, 1, std::time::Duration::from_secs(10)).await;
    assert!(
        !outcomes.is_empty(),
        "the retry handler must report LoopRetried; an empty record means it \
         refused (guard or ledger admission)"
    );
    assert_eq!(
        outcomes.first().map(|t| t.outcome),
        Some(TurnOutcome::LoopRetried)
    );

    // Drain the rest once each — await_recorded empties buffers, so all
    // remaining assertions read from snapshots.
    let handed = drain_all(&dispatches);
    let sent = drain_all(&sends);
    let completed = drain_all(&completions);
    let recorded = drain_all(&entries);

    // Then the turn reached the dispatch queue twice — the enqueue's
    // dispatch and the retry's.
    assert!(
        handed.len() >= 2,
        "enqueue dispatch + retry dispatch, got {handed:?}"
    );
    assert!(
        handed.iter().all(|d| d.session_id == pipeline.session_id),
        "both dispatches are for the looping session"
    );

    // And the provider saw the dispatches (2 sends: original + retry).
    assert!(
        sent.len() >= 2,
        "original + retried dispatch must both reach the provider"
    );

    // And the stream completed as a repetition loop.
    assert!(
        completed
            .iter()
            .any(|c| c.reason == StreamCompletedReason::RepetitionLoop),
        "the loop must complete as a repetition loop, got: {:?}",
        completed.iter().map(|c| c.reason).collect::<Vec<_>>()
    );

    // And the watchdog pushed exactly one visible system marker (the
    // retry attempt counter), and no other entry kind — the model is
    // never told it looped.
    let marker = recorded.first().expect("exactly one marker entry");
    assert_eq!(
        recorded.len(),
        1,
        "exactly one marker entry for the first loop, got: {:?}",
        recorded.iter().map(|e| &e.entry.kind).collect::<Vec<_>>()
    );
    assert!(
        matches!(&marker.entry.kind, ChatEntryKind::System(t) if t.contains("1 of 3")),
        "the marker must be a system entry naming attempt 1: {:?}",
        marker.entry.kind
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_looped_partials_stay_visible_but_leave_the_context() {
    // Given the composed pipeline whose provider always loops.
    let pipeline = pipeline(vec![ScriptedResponse {
        tokens: mantra_chunks(),
        tool_calls: vec![],
    }])
    .await;
    let entries = pipeline.harness.spawn_recorder::<PushChatEntry>().await;

    // When the first loop retries.
    start_turn(&pipeline, "do the thing").await;
    let _ = await_recorded(&entries, 1, std::time::Duration::from_secs(5)).await;

    // Then the looped attempt is still in the chat log — the user watched
    // it stream — but the retried request carries no assistant content:
    // only the user entry reaches the provider.
    let state = pipeline.state.read();
    let session = state.session.get(&pipeline.session_id).expect("session");
    let looped_visible = session.history().iter().any(|e| {
        matches!(&e.kind, ChatEntryKind::Assistant(t) if t.contains("re-examine the situation"))
    });
    assert!(
        looped_visible,
        "the looped attempt must stay visible in the log"
    );
    let messages = jinn_llm_support::entries_to_messages::entries_to_messages(session.history());
    assert!(
        !messages.iter().any(|m| matches!(
            m,
            jinn_provider::LlmMessage::Assistant { content, .. }
                if content.contains("re-examine the situation")
        )),
        "the retried request must not carry the looped attempt"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn three_loops_surrender_with_a_marker_then_a_cancel() {
    // Given the composed pipeline with three looping responses queued.
    let pipeline = pipeline(vec![
        ScriptedResponse {
            tokens: mantra_chunks(),
            tool_calls: vec![],
        },
        ScriptedResponse {
            tokens: mantra_chunks(),
            tool_calls: vec![],
        },
        ScriptedResponse {
            tokens: mantra_chunks(),
            tool_calls: vec![],
        },
    ])
    .await;
    let entries = pipeline.harness.spawn_recorder::<PushChatEntry>().await;
    let cancels = pipeline.harness.spawn_recorder::<CancelTurn>().await;
    let outcomes = pipeline.harness.spawn_recorder::<TurnCompleted>().await;
    let sends = pipeline.harness.spawn_recorder::<SendToLlmProvider>().await;

    // When all three loops run their course.
    start_turn(&pipeline, "do the thing").await;
    let recorded = await_recorded(&entries, 3, std::time::Duration::from_secs(10)).await;

    // Then the markers escalate: retry, retry, surrender.
    let texts: Vec<&str> = recorded
        .iter()
        .filter_map(|e| match &e.entry.kind {
            ChatEntryKind::System(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 3, "retry, retry, surrender: {texts:?}");
    assert!(
        texts.first().is_some_and(|t| t.contains("1 of 3"))
            && texts.get(1).is_some_and(|t| t.contains("2 of 3")),
        "retries first: {texts:?}"
    );
    assert!(
        texts.get(2).is_some_and(|t| t.contains("3 times")),
        "the third marker is the surrender: {texts:?}"
    );

    // And the surrender cancels the turn with buffers intact.
    let cancels = await_recorded(&cancels, 1, std::time::Duration::from_secs(5)).await;
    let cancel = cancels.first().expect("the surrender cancels the turn");
    assert!(
        !cancel.drain_buffers_to_input,
        "a watchdog stop is not ESC-ESC"
    );

    // And the turn ends terminally — the surrender's cancel settles it.
    // Three loop completions produce two retry outcomes plus a third from
    // the racing retry (the retry that beat the cancel to the ledger also
    // reports LoopRetried before the cancel settles), so: exactly one
    // terminal outcome, everything else LoopRetried.
    let outcomes = await_recorded(&outcomes, 3, std::time::Duration::from_secs(5)).await;
    assert!(
        outcomes.len() >= 3,
        "two retries then the surrender's settle, got: {:?}",
        outcomes.iter().map(|t| t.outcome).collect::<Vec<_>>()
    );
    let terminal: Vec<_> = outcomes
        .iter()
        .filter(|t| t.outcome.is_terminal())
        .collect();
    assert_eq!(
        terminal.len(),
        1,
        "the turn ends exactly once: {:?}",
        outcomes.iter().map(|t| t.outcome).collect::<Vec<_>>()
    );
    assert_ne!(
        terminal.first().map(|t| t.outcome),
        Some(TurnOutcome::LoopRetried)
    );

    // And the racing retry that landed before the cancel was killed by it:
    // after a settle window the pipeline is quiescent — the send count
    // stops, and no marker arrived after the surrender (the latch holds;
    // a budget that restarted would retry-mark the killed stream's loop).
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let sends_now = sends.len();
    let _ = drain_all(&entries);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        sends.len(),
        sends_now,
        "the pipeline must go quiet after the surrender's settle"
    );
    assert!(
        (2..=4).contains(&sends_now),
        "enqueue + retries (the racing retry may or may not land before the \
         cancel), but never an unbounded chain: got {sends_now}"
    );
    let markers_after_quiescence = drain_all(&entries).len();
    assert_eq!(
        markers_after_quiescence, 0,
        "nothing publishes after the settle — the surrender latched the budget"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_clean_response_ends_the_turn_before_the_budget_trips() {
    // Given a pipeline whose second response is ordinary varied prose.
    let pipeline = pipeline(vec![
        ScriptedResponse {
            tokens: mantra_chunks(),
            tool_calls: vec![],
        },
        ScriptedResponse::text(VARIED_PROSE),
    ])
    .await;
    let entries = pipeline.harness.spawn_recorder::<PushChatEntry>().await;
    let cancels = pipeline.harness.spawn_recorder::<CancelTurn>().await;
    let outcomes = pipeline.harness.spawn_recorder::<TurnCompleted>().await;

    // When loop → clean runs.
    start_turn(&pipeline, "do the thing").await;
    let recorded = await_recorded(&entries, 1, std::time::Duration::from_secs(10)).await;

    // Then exactly one marker was pushed (the first loop's retry marker)…
    let markers: Vec<&str> = recorded
        .iter()
        .filter_map(|e| match &e.entry.kind {
            ChatEntryKind::System(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(markers.len(), 1, "one loop, one marker: {markers:?}");

    // …the turn settled successfully after the clean response…
    let outcomes = await_recorded(&outcomes, 2, std::time::Duration::from_secs(10)).await;
    assert!(
        outcomes.iter().any(|t| t.outcome == TurnOutcome::Succeeded),
        "the clean response ends the turn: {outcomes:?}"
    );

    // …and the watchdog never surrendered: no cancel, and the budget was
    // cleared with the turn (a fresh turn starts at zero).
    let cancels = await_recorded(&cancels, 1, std::time::Duration::from_millis(300)).await;
    assert!(cancels.is_empty(), "a clean turn is never cancelled");
}
