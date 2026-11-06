//! End-to-end crossing test for the provider-error watchdog: a composed
//! app whose provider streams die mid-flight with a retryable error; the
//! watchdog counts the errors, waits its configured schedule, re-dispatches
//! through the real stall-retry path, and after exhausting the schedule the
//! give-up command settles the turn — every hop through the real fabric
//! (watchdog → `RetryStalledSession` → session actor surgery → `DispatchTurn`
//! → queue actor → inference actor → `StreamCompleted` → …).
//!
//! The harness (`launch_for_test`) activates every slice but does NOT spawn
//! the kernel's `SessionPersistenceActor` — the fold that consumes
//! `StreamCompleted` / `RetryStalledSession` / `GiveUpProviderRetries`. This
//! file needs the full crossing, so its composed helper spawns that actor
//! itself, exactly as the sibling `watchdog.rs` and `phase_control.rs` do.
//!
//! The provider error is built at the provider-stream layer (an `Err` item
//! mid-stream), the class the fast `[provider.request_retry]` tier cannot
//! see: by the time the error surfaces, the stream the decorator returned is
//! already being consumed, and the retryability verdict rides the
//! `StreamCompleted` the inference actor stamps (`true` for a chunk `Err`).

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use error_stack::Report;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntry, SessionId};
use jinn_inference_msg::{SendToLlmProvider, StreamCompleted};
use jinn_kernel::AppCore;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::turn_ledger::MintKind;
use jinn_llm_support::token_estimator::TiktokenCounter;
use jinn_preferences_config::ProviderErrorWatchdogConfig;
use jinn_provider::{
    LlmMessage, LlmService, LlmServiceError, LlmServiceFactory, StreamEvent, ToolStream,
};
use jinn_session_msg::TurnCompleted;
use jinn_slices::PhaseView;
use jinn_testutil::bus_harness::{Recorder, TestHarness};
use jinn_tui::TuiApp;

use crate::common::launch_for_test;

/// How long any wait-for may run before its assertion fails.
const WAIT: Duration = Duration::from_secs(20);

/// A factory whose every stream yields one token and then dies with an
/// `Err` item — the transport-class failure the inference actor classifies
/// retryable and stamps onto `StreamCompleted`.
#[derive(Debug)]
struct DyingStreamFactory {
    /// How many streams have been opened, so the test can prove the retry
    /// actually re-dispatched (the count grows past one only if the
    /// watchdog's retry landed and the queue actor re-sent).
    streams_opened: std::sync::Arc<AtomicUsize>,
}

impl LlmServiceFactory for DyingStreamFactory {
    fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
        self.streams_opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(DyingStreamService))
    }
    fn name(&self) -> &'static str {
        "DyingStream"
    }
}

#[derive(Debug)]
struct DyingStreamService;

#[async_trait::async_trait]
impl LlmService for DyingStreamService {
    fn name(&self) -> &'static str {
        "DyingStream"
    }
    async fn chat_stream(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<LlmMessage>,
    ) -> Result<jinn_provider::ChatStream, Report<LlmServiceError>> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok("partial".to_owned()),
            Err(Report::new(LlmServiceError::Retryable)),
        ])))
    }
    async fn chat_stream_with_tools(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<LlmMessage>,
        _tools: Vec<jinn_core_types::tool_types::ToolDefinition>,
    ) -> Result<ToolStream, Report<LlmServiceError>> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::Text("partial".to_owned())),
            Err(Report::new(LlmServiceError::Retryable)),
        ])))
    }
}

/// The extra typed recorders a test asks [`composed_app`] to tap.
trait ExtraRecorders {
    /// The recorders this marker spawns, in declaration order.
    type Recorders: Send + 'static;

    /// Spawns one recorder per element onto the harness.
    async fn spawn(&self, harness: &TestHarness) -> Self::Recorders;
}

impl ExtraRecorders for () {
    type Recorders = ();

    async fn spawn(&self, _harness: &TestHarness) {}
}

/// The marker naming one `TurnCompleted` recorder.
struct TurnCompletedTap;

impl<A: ExtraRecorders, B: ExtraRecorders> ExtraRecorders for (A, B) {
    type Recorders = (A::Recorders, B::Recorders);

    async fn spawn(&self, harness: &TestHarness) -> Self::Recorders {
        (self.0.spawn(harness).await, self.1.spawn(harness).await)
    }
}

impl<A: ExtraRecorders> ExtraRecorders for (A,) {
    type Recorders = (A::Recorders,);

    async fn spawn(&self, harness: &TestHarness) -> Self::Recorders {
        (self.0.spawn(harness).await,)
    }
}

impl ExtraRecorders for TurnCompletedTap {
    type Recorders = Recorder<TurnCompleted>;

    async fn spawn(&self, harness: &TestHarness) -> Self::Recorders {
        harness.spawn_recorder::<TurnCompleted>().await
    }
}

/// A composed app whose provider streams die mid-flight, with the
/// provider-error watchdog on a fast schedule (`[1, 2]` seconds — two
/// retries, surrender on the third error). The section is written into
/// the services' configuration layer **before** `launch_for_test`
/// activates the slices — activation reads the knobs from the layer once.
///
/// Also spawns the kernel session actor over the SAME `State` and the
/// SAME trouper system the harness wires, so the watchdog's markers,
/// retry command, and give-up command have their real consumer.
///
/// The stall watchdog is configured with a huge window so its own trip
/// cannot confound the observation: a provider-error wait is a hold with
/// no dispatch in flight, and the stall watchdog must stay silent through
/// it.
async fn composed_app<M: ExtraRecorders>(
    extra: M,
) -> (TuiApp, SessionId, std::sync::Arc<AtomicUsize>, M::Recorders) {
    use jinn_kernel::common::bus::HarnessServices;

    let harness = TestHarness::new().await;
    let extras = extra.spawn(&harness).await;
    let services = harness.services().await;

    let streams_opened = std::sync::Arc::new(AtomicUsize::new(0));
    services
        .llm_service
        .swap(std::sync::Arc::new(DyingStreamFactory {
            streams_opened: streams_opened.clone(),
        }));

    let state = jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus());
    services
        .config
        .put::<ProviderErrorWatchdogConfig>(&ProviderErrorWatchdogConfig {
            waits_secs: vec![1, 2],
        })
        .expect("write the provider-error watchdog section");
    services
        .config
        .put::<jinn_preferences_config::StallWatchdogConfig>(
            &jinn_preferences_config::StallWatchdogConfig {
                timeout_secs: 3600,
                max_restarts: 0,
            },
        )
        .expect("write the stall watchdog section");

    let core = AppCore {
        state: state.clone(),
        bridge: services.bridge.clone(),
    };

    // The context-assembly service answers the queue actor's assemble ask
    // on the retry re-dispatch path (must exist before any ask).
    let _assembly = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);

    jinn_session_turn::activate(
        &services.trouper_system,
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps: ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            counter: TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::system(),
        },
    );

    let app = launch_for_test(core, services).await;
    let session_id = app.core.state.read().session.active_session_id().clone();
    (app, session_id, streams_opened, extras)
}

/// Drives the turn the way production does: the user's message dispatches
/// through the queue actor, the inference actor opens a stream, the stream
/// dies with a retryable `Err` item, and the retryable-error completion
/// reaches every declarant.
async fn send_user_message(app: &TuiApp, session_id: &SessionId) {
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("provoke a dying stream"),
        })
        .await;
}

/// Polls the session's history until a system marker containing `needle`
/// lands, panicking with diagnostics at the deadline.
async fn wait_for_marker(app: &TuiApp, session_id: &SessionId, needle: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let landed = app
            .core
            .state
            .read()
            .session
            .get(session_id)
            .is_some_and(|s| {
                s.history()
                    .iter()
                    .any(|e| e.kind_str() == "system" && e.text().contains(needle))
            });
        if landed {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            let history = app
                .core
                .state
                .read()
                .session
                .get(session_id)
                .map(|s| {
                    s.history()
                        .iter()
                        .map(|e| (e.kind_str().to_owned(), e.text().to_owned()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let dead = app.services.trouper_system.dead_letter_count().await;
            panic!(
                "marker {needle:?} never landed; diagnostics: dead letters = {dead}, \
                 history now = {history:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Polls until the session's phase is `wanted`, `false` at the deadline.
async fn wait_until_phase(app: &TuiApp, session_id: &SessionId, wanted: PhaseView) -> bool {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if app
            .core
            .state
            .read()
            .session
            .get(session_id)
            .map(|s| s.phase())
            == Some(wanted)
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// R1: a retryable provider error holds the turn — the deferral keeps the
/// session in `Streaming` (no settle) — and the watchdog's first retry
/// re-dispatches through the real path, opening a second stream.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn a_retryable_error_is_held_then_retried_through_the_real_path() {
    // Given a composed app on a [1s, 2s] provider-error schedule whose
    // streams die mid-flight with a retryable error.
    let (app, session_id, streams_opened, ()) = composed_app(()).await;
    send_user_message(&app, &session_id).await;

    // When the first retry window (1s) plus tick latency elapses.
    wait_for_marker(&app, &session_id, "retrying (attempt 1 of 2)").await;

    // Then the retry actually re-dispatched: a second provider stream was
    // opened (watchdog → RetryStalledSession → session surgery →
    // DispatchTurn → queue actor → inference actor).
    let deadline = tokio::time::Instant::now() + WAIT;
    while streams_opened.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the retry never re-dispatched: streams opened = {}",
            streams_opened.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // And the session stayed live through the hold: `Streaming`, not
    // settled to `Idle` — the deferral's whole point.
    let phase = app
        .core
        .state
        .read()
        .session
        .get(&session_id)
        .map(|s| s.phase());
    assert_eq!(
        phase,
        Some(PhaseView::Streaming),
        "a deferred retryable error must hold the turn open, not settle it"
    );
}

/// R2: consecutive retryable errors escalate the schedule and the third
/// error surrenders — the give-up marker lands, the turn settles to
/// `Idle`, and exactly one `TurnCompleted(Error)` reports it.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn an_exhausted_schedule_surrenders_and_settles_the_turn() {
    // Given the same dying-stream app.
    let (app, session_id, _streams, (turns,)) = composed_app((TurnCompletedTap,)).await;
    send_user_message(&app, &session_id).await;

    // When the schedule runs out — retry at 1s, retry at 2s, surrender on
    // the third error.
    wait_for_marker(&app, &session_id, "retrying (attempt 1 of 2)").await;
    wait_for_marker(&app, &session_id, "retrying (attempt 2 of 2)").await;
    wait_for_marker(&app, &session_id, "giving up").await;

    // Then the turn settles: `Idle`, with the queued-drain shape of the
    // error arm (no queue existed, so the input buffer is untouched).
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(
        settled,
        "surrender must settle the deferred turn to Idle; phase is {:?}",
        app.core
            .state
            .read()
            .session
            .get(&session_id)
            .map(|s| s.phase())
    );

    // And exactly one `TurnCompleted(Error)` reported the turn — the
    // settle-once guarantee, through the real fabric.
    jinn_testutil::wait::await_settled(&turns, Duration::from_millis(300), Duration::from_secs(5))
        .await;
    let reports = turns.drain();
    assert_eq!(
        reports.len(),
        1,
        "the give-up settle must report the turn exactly once; got {reports:?}"
    );
    assert_eq!(reports[0].outcome, jinn_session_msg::TurnOutcome::Error);

    // And the ledger entry is swept: the session takes a new turn.
    let grant = app
        .services
        .turn_ledger
        .mint(&session_id, MintKind::FreshTurn);
    assert!(
        grant.is_some(),
        "a surrendered turn's entry must be swept; a fresh mint must be admitted"
    );
}

/// R3: a retryable error on a turn that is *not* live for this session's
/// admission (no armed guard) does not defer — the completion falls through
/// to today's error settle. Publishes the completion directly to test the
/// gate without a real dispatch.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn a_gate_miss_settles_ordinarily_through_the_real_path() {
    // Given a composed app with no live turn: nothing was ever dispatched,
    // so the ledger has no entry and no guard is armed.
    let (app, session_id, streams_opened, (turns,)) = composed_app((TurnCompletedTap,)).await;

    // When a retryable-error completion for the session arrives anyway —
    // a straggler naming a turn that does not exist (the unset identity is
    // the wire default, refused by every live-turn admission).
    let event = StreamCompleted {
        session_id: session_id.clone(),
        reason: jinn_inference_msg::StreamCompletedReason::Error,
        retryable: true,
        assistant_content: None,
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at: jiff::Timestamp::now(),
        model_used: None,
        turn: jinn_core_types::turn_identity::TurnToken::default(),
        stream: jinn_core_types::turn_identity::StreamId::default(),
    };
    app.services.bus.publish(event).await;

    // Then the gate refuses: no settle can fire for a turn that never
    // started, and no retry re-dispatches. (The watchdog may announce the
    // retry from the raw completion — announce-then-admit, the
    // stall-watchdog precedent — but the retry command is what the phase
    // and ledger admission refuse, so nothing dispatches.)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut reported = Vec::new();
    while tokio::time::Instant::now() < deadline {
        reported.extend(turns.drain());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        reported.is_empty(),
        "a straggler completion must settle nothing: got {reported:?}"
    );
    let still_idle = app
        .core
        .state
        .read()
        .session
        .get(&session_id)
        .map(|s| s.phase());
    assert_eq!(
        still_idle,
        Some(PhaseView::Idle),
        "the refused completion must leave the idle session untouched"
    );
    let streams = streams_opened.load(Ordering::SeqCst);
    assert_eq!(
        streams, 0,
        "a refused retry must not open a provider stream"
    );
}

/// R4: the user's ESC during a hold cancels cleanly — the settle runs
/// (today's cancel path, untouched by the deferral), the watchdog's
/// schedule disarms on the terminal `TurnCompleted`, and the pending
/// retry no-ops against the dead turn.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn a_cancel_during_a_hold_settles_and_disarms_the_schedule() {
    // Given a composed app mid-hold: the dying stream's retryable error
    // was deferred, the 1s wait is running.
    let (app, session_id, _streams, (turns,)) = composed_app((TurnCompletedTap,)).await;
    send_user_message(&app, &session_id).await;

    // When the user cancels before the wait elapses (ESC's publish path).
    app.services
        .bus
        .publish(jinn_inference_msg::CancelTurn {
            session_id: session_id.clone(),
            // The ESC-ESC shape: buffered work lands in the input box.
            drain_buffers_to_input: true,
        })
        .await;

    // Then the cancel settles the session today-style.
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(
        settled,
        "a cancel during a hold must settle the session; phase is {:?}",
        app.core
            .state
            .read()
            .session
            .get(&session_id)
            .map(|s| s.phase())
    );

    // And the cancel reported the turn, exactly once — the terminal
    // `TurnCompleted` the watchdog disarms on.
    jinn_testutil::wait::await_settled(&turns, Duration::from_millis(300), Duration::from_secs(5))
        .await;
    let reports = turns.drain();
    assert_eq!(
        reports.len(),
        1,
        "the cancel settle must report the turn exactly once; got {reports:?}"
    );

    // And no retry marker ever lands: the schedule was disarmed by the
    // terminal `TurnCompleted`, and the pending deadline died with it.
    // The hold was well under the 1s wait, so any retry would have shown
    // within this window had the disarm failed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut retried = false;
    while tokio::time::Instant::now() < deadline {
        retried = app
            .core
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|s| {
                s.history()
                    .iter()
                    .any(|e| e.text().contains("retrying (attempt"))
            });
        if retried {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !retried,
        "a cancel during a hold must disarm the pending retry"
    );
}

/// R5: a *deterministic* (non-retryable) provider error never schedules —
/// the completion settles today-style and no marker lands. Driven through
/// the real stream path with a factory whose stream dies with a
/// `Provider`-class construction error (the auth/invalid-request class).
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn a_deterministic_error_settles_ordinarily_with_no_retry() {
    // Given a composed app whose factory refuses stream construction with
    // a non-retryable `Provider` error.
    let (app, session_id, _streams, ()) = composed_app(()).await;
    let refusing = {
        #[derive(Debug)]
        struct RefusingFactory;
        impl LlmServiceFactory for RefusingFactory {
            fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
                Err(Report::new(LlmServiceError::Provider))
            }
            fn name(&self) -> &'static str {
                "Refusing"
            }
        }
        RefusingFactory
    };
    app.services.llm_service.swap(std::sync::Arc::new(refusing));

    // When the user's message dispatches and the construction fails.
    send_user_message(&app, &session_id).await;

    // Then the turn settles today-style — no hold, no schedule, no retry
    // marker (a deterministic error is ignored by the watchdog's policy;
    // the settle is the ordinary error path).
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(
        settled,
        "a deterministic provider error must settle the turn; phase is {:?}",
        app.core
            .state
            .read()
            .session
            .get(&session_id)
            .map(|s| s.phase())
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let retried = app
        .core
        .state
        .read()
        .session
        .get(&session_id)
        .is_some_and(|s| {
            s.history()
                .iter()
                .any(|e| e.text().contains("retrying (attempt"))
        });
    assert!(
        !retried,
        "a deterministic provider error must never schedule a retry"
    );
}

/// R6: the re-dispatch that follows a retry carries the failed attempt's
/// exclusion — the retried request does not carry the partial text the
/// dead generation produced. The dying streams hang their partials in
/// history (visible), but each `SendToLlmProvider` the queue actor assembles
/// is built from the rewound context.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn the_retried_dispatch_excludes_the_failed_attempt() {
    // Given a recorder tapped for every dispatch the fabric carries.
    use jinn_testutil::bus_harness::await_recorded;

    struct DispatchTap;
    impl ExtraRecorders for DispatchTap {
        type Recorders = Recorder<SendToLlmProvider>;
        async fn spawn(&self, harness: &TestHarness) -> Self::Recorders {
            harness.spawn_recorder::<SendToLlmProvider>().await
        }
    }

    let (app, session_id, streams_opened, (dispatches,)) = composed_app((DispatchTap,)).await;
    send_user_message(&app, &session_id).await;

    // When the first retry re-dispatches (a second stream opens).
    wait_for_marker(&app, &session_id, "retrying (attempt 1 of 2)").await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while streams_opened.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the retry never re-dispatched"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let all = await_recorded(&dispatches, 2, WAIT).await;
    let retried = all
        .iter()
        .filter(|d| d.session_id == session_id)
        .collect::<Vec<_>>();
    assert!(
        retried.len() >= 2,
        "the retry must have produced a second dispatch; got {}",
        retried.len()
    );

    // Then the retried request does not carry the dead generation's
    // partial: the excluded attempt is display-only history, and the
    // rewind projection kept it out of the assembled prompt.
    let first = &retried[0];
    let second = retried
        .iter()
        .find(|d| d.dispatched_at > first.dispatched_at)
        .expect("the retried dispatch is the later one");
    let carries_partial = second.messages.iter().any(|m| match m {
        LlmMessage::Assistant { content, .. } => content.contains("partial"),
        _ => false,
    });
    assert!(
        !carries_partial,
        "the retried request must exclude the failed attempt's partial output"
    );
}
