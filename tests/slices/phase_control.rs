//! RED tests for the phase-control unification.
//!
//! Six scenarios the unification exists to fix, written against the
//! composed system BEFORE the fix, per the task contract. Each test
//! drives the real fabric — real slice activation, real queue actor,
//! real session actor, real watchdogs — through `launch_for_test`, and
//! asserts one observable cancellation behavior:
//!
//! - [`cancel_then_racing_dispatch_leaves_phase_idle`] (R1): the phase
//!   must not re-enter `Streaming` when a queued dispatch races a
//!   cancel. Today `queue_actor::dispatch_prepared` calls
//!   `begin_streaming()` with no termination check.
//! - [`cancel_then_racing_dispatch_publishes_no_provider_request`] (R2):
//!   that racing dispatch must publish no `SendToLlmProvider` — refused
//!   at mint, not latched downstream.
//! - [`cancel_then_racing_dispatch_arms_no_stall_timer`] (R3): no
//!   provider request means no stall arm; the watchdog must never fire.
//! - [`user_message_after_cancel_dispatches_exactly_one_turn`] (R4):
//!   after the race, the user's next message must start exactly one
//!   turn — the wedged `Streaming` phase today queues it forever.
//! - [`stream_token_after_cancel_does_not_reach_streaming`] (R6): a
//!   late token for a cancelled session must not arm the phase back to
//!   `Streaming` (today's defensive `begin_streaming()`).
//!
//! The sixth scenario (R5, the busy counter reading as "working") is
//! sidebar-owned and lives in the sidebar's own test module
//! (`crates/slices/jinn-sidebar/src/sections/sessions_tests.rs`), where
//! the state adapter it tests lives.
//!
//! All six are expected RED before the unification lands and GREEN
//! after. Each is demonstrated failing against the pre-change tree and
//! paired with a mutation that breaks it again (recorded in the task's
//! final report).

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::time::Duration;

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntry, SessionId};
use jinn_inference_msg::{CancelTurn, SendToLlmProvider, StreamToken};
use jinn_kernel::AppCore;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_llm_support::token_estimator::TiktokenCounter;
use jinn_provider::FakeLlmServiceFactory;
use jinn_session_msg::TurnOutcome;
use jinn_slices::PhaseView;
use jinn_testutil::bus_harness::Recorder;
use jinn_tui::TuiApp;

use crate::common::launch_for_test;

/// How long any single wait may run before the test gives up and lets
/// the assertion report what it saw.
const WAIT: Duration = Duration::from_secs(10);

/// A composed app over the real fabric: queue actor, session actor,
/// inference actor, watchdogs, and a scripted LLM factory. The stall
/// watchdog window is configured in seconds; 10 keeps it silent inside
/// a test's wait horizon, 1 makes it trip fast (R3).
///
/// Also spawns the kernel session actor over the same `State` and
/// trouper system the harness wires (the production deps shape from
/// `actor_wiring.rs`, with the fake-session store the harness carries).
async fn composed_app(
    factory: impl jinn_provider::LlmServiceFactory + 'static,
    stall_timeout_secs: u64,
) -> (TuiApp, SessionId) {
    let (app, session_id, _recorder) =
        composed_app_with_recorder(factory, stall_timeout_secs).await;
    (app, session_id)
}

/// [`composed_app`], plus a typed `SendToLlmProvider` recorder tapped
/// into the fabric before the slices activate.
async fn composed_app_with_recorder(
    factory: impl jinn_provider::LlmServiceFactory + 'static,
    stall_timeout_secs: u64,
) -> (TuiApp, SessionId, Recorder<SendToLlmProvider>) {
    let (app, session_id, recorder, ()) =
        composed_app_with_recorders(factory, stall_timeout_secs, ()).await;
    (app, session_id, recorder)
}

/// [`composed_app_with_recorder`], plus extra typed recorders named by the
/// caller's tuple type. Each recorder is tapped into the fabric before the
/// slices activate, so no publish can outrun its tap.
async fn composed_app_with_recorders<M>(
    factory: impl jinn_provider::LlmServiceFactory + 'static,
    stall_timeout_secs: u64,
    extra: M,
) -> (TuiApp, SessionId, Recorder<SendToLlmProvider>, M::Recorders)
where
    M: ExtraRecorders,
{
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_preferences_config::StallWatchdogConfig;
    use jinn_testutil::bus_harness::TestHarness;

    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
    let extras = extra.spawn(&harness).await;

    let services = harness.services().await;
    services.llm_service.swap(std::sync::Arc::new(factory));

    let state = jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus());
    services
        .config
        .put::<StallWatchdogConfig>(&StallWatchdogConfig {
            timeout_secs: stall_timeout_secs,
            max_restarts: 3,
        })
        .expect("write the stall watchdog section");

    let core = AppCore {
        state: state.clone(),
        bridge: services.bridge.clone(),
    };

    // The context-assembly service answers the queue actor's assemble ask
    // (must exist before any ask).
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
    (app, session_id, recorder, extras)
}

/// The extra typed recorders a test asks [`composed_app_with_recorders`]
/// to tap: the unit type taps none; a tuple taps one per element.
trait ExtraRecorders {
    /// The recorders this marker spawns, in declaration order.
    type Recorders: Send + 'static;

    /// Spawns one recorder per element onto the harness.
    async fn spawn(&self, harness: &jinn_testutil::bus_harness::TestHarness) -> Self::Recorders;
}

impl ExtraRecorders for () {
    type Recorders = ();

    async fn spawn(&self, _harness: &jinn_testutil::bus_harness::TestHarness) {}
}

/// The marker naming one `TurnCompleted` recorder: a distinct type so a
/// caller's tuple reads as a list of facts to tap, not bare generics.
struct TurnCompletedTap;

impl<A: ExtraRecorders, B: ExtraRecorders> ExtraRecorders for (A, B) {
    type Recorders = (A::Recorders, B::Recorders);

    async fn spawn(&self, harness: &jinn_testutil::bus_harness::TestHarness) -> Self::Recorders {
        (self.0.spawn(harness).await, self.1.spawn(harness).await)
    }
}

impl ExtraRecorders for TurnCompletedTap {
    type Recorders = jinn_testutil::bus_harness::Recorder<jinn_session_msg::TurnCompleted>;

    async fn spawn(&self, harness: &jinn_testutil::bus_harness::TestHarness) -> Self::Recorders {
        harness
            .spawn_recorder::<jinn_session_msg::TurnCompleted>()
            .await
    }
}

/// A factory whose streams produce one token and end normally.
#[expect(dead_code, reason = "reserved for the bounded hand pass")]
fn finishing_factory() -> FakeLlmServiceFactory {
    FakeLlmServiceFactory::new(vec!["done".to_owned()])
}

/// A factory whose streams produce one token and then hang forever —
/// no completion. The flapped phase a racing dispatch produces therefore
/// stays flapped: nothing settles it inside the test's window.
///
/// (The stall watchdog must be configured with a window longer than the
/// test's wait so its own trip cannot confound the observation — except
/// R3, which asserts the trip never happens at all.)
fn hung_factory() -> jinn_provider::HungStreamFactory {
    jinn_provider::HungStreamFactory
}

/// Seeds the phase the way the queue actor leaves it after a real
/// dispatch: `Streaming` with a live stream stamp.
fn seed_streaming(app: &TuiApp, session_id: &SessionId) {
    let dispatched_at = jiff::Timestamp::now();
    // The ledger turn is what a cancel settles: mint it the way the
    // dispatch path does, so the cancel below has a live turn to drain.
    let _minted = app
        .services
        .turn_ledger
        .mint(
            session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        )
        .expect("seed mint must be admitted");
    app.core.state.with_session(|view| {
        let session = view.session.map().get_or_create(session_id);
        session.begin_streaming();
        session.arm_stream(dispatched_at);
        session
            .append_stream_token("warm", dispatched_at)
            .expect("warm token registers the streaming generation");
    });
    // The stream stays in flight in the ledger: the cancel's synthesized
    // completion carries the live (turn, stream) pair and retires it,
    // exactly as a mid-stream cancel does in production.
}

/// True when the named session's phase is `Streaming`.
fn is_streaming(app: &TuiApp, session_id: &SessionId) -> bool {
    phase_of(app, session_id) == Some(PhaseView::Streaming)
}

/// The named session's phase, if it exists.
fn phase_of(app: &TuiApp, session_id: &SessionId) -> Option<PhaseView> {
    app.core
        .state
        .read()
        .session
        .get(session_id)
        .map(|s| s.phase())
}

/// Polls until the session's phase equals `wanted` or the wait expires.
///
/// Async with `tokio::time::sleep`: the composed actors run on this
/// test's own runtime, and a blocking sleep would starve them.
async fn wait_until_phase(app: &TuiApp, session_id: &SessionId, wanted: PhaseView) -> bool {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if phase_of(app, session_id) == Some(wanted) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Polls the recorder until at least `min_count` requests arrive.
async fn wait_for_recorder(
    recorder: &Recorder<SendToLlmProvider>,
    min_count: usize,
) -> Vec<SendToLlmProvider> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut collected: Vec<SendToLlmProvider> = Vec::new();
    loop {
        collected.extend(recorder.drain());
        if collected.len() >= min_count {
            return collected;
        }
        if tokio::time::Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Publishes a cancel for the named session on the composed fabric.
async fn publish_cancel(app: &TuiApp, session_id: &SessionId) {
    app.services
        .bus
        .publish(CancelTurn {
            session_id: session_id.clone(),
            drain_buffers_to_input: false,
        })
        .await;
}

/// Publishes a racing dispatch for the named session.
async fn publish_racing_dispatch(app: &TuiApp, session_id: &SessionId) {
    app.services
        .bus
        .publish(jinn_turn_dispatch_msg::DispatchTurn {
            session_id: session_id.clone(),
        })
        .await;
}

// ---------------------------------------------------------------------------
// R1 — the cancelled turn's phase must not re-enter Streaming
// ---------------------------------------------------------------------------

/// R1: a `DispatchTurn` that races a cancel must not put the session
/// back into `Streaming`.
///
/// Today: the cancel settles the session to `Idle`, the racing
/// `dispatch_prepared` then calls `begin_streaming()` with no
/// termination check, and the phase flaps back to `Streaming` with a
/// live in-flight guard on a dead turn.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn cancel_then_racing_dispatch_leaves_phase_idle() {
    // Given a composed app mid-turn (phase `Streaming`, a live stream
    // stamp) and a racing dispatch published after the cancel. The
    // stream factory hangs, so if the racing dispatch is admitted its
    // flapped `Streaming` persists for the whole observation window.
    let (app, session_id) = composed_app(hung_factory(), 30).await;
    seed_streaming(&app, &session_id);
    assert!(is_streaming(&app, &session_id), "seed: phase is Streaming");

    // When the cancel lands first and settles...
    publish_cancel(&app, &session_id).await;
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");

    // ...and the racing dispatch arrives after the settle — the
    // "resume queued before the cancel" shape arriving late.
    publish_racing_dispatch(&app, &session_id).await;
    // The phase is the observable here, so wait for it to stop changing.
    // A fixed sleep would pass on a slow host simply because the racing
    // dispatch had not been processed yet — the opposite of what this test
    // claims.
    jinn_testutil::wait::await_value_settled(
        Duration::from_millis(200),
        Duration::from_secs(10),
        || phase_of(&app, &session_id),
    )
    .await;

    // Then the phase never flapped back to Streaming.
    assert!(
        !is_streaming(&app, &session_id),
        "a dispatch racing a cancel must not re-enter Streaming"
    );
}

// ---------------------------------------------------------------------------
// R2 — the racing dispatch publishes no provider request
// ---------------------------------------------------------------------------

/// R2: the same race must publish no `SendToLlmProvider`.
///
/// Today: the racing `dispatch_prepared` publishes the request and the
/// downstream latch/tombstone pair swallows its effects; after the
/// unification the refusal happens at mint and nothing is published.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn cancel_then_racing_dispatch_publishes_no_provider_request() {
    // Given the same seeded mid-turn session and racing pair, with a
    // typed recorder tapped into the fabric. The factory hangs so an
    // admitted racing dispatch cannot self-heal the phase either.
    let (app, session_id, recorder) = composed_app_with_recorder(hung_factory(), 30).await;
    seed_streaming(&app, &session_id);

    // When the cancel lands and settles, then the racing dispatch.
    publish_cancel(&app, &session_id).await;
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;

    // Then no provider request is published for the racing dispatch.
    // The wait gives the queue actor every chance to publish — and the
    // recorder must have stopped changing first, so "no request" means
    // "no request will come" rather than "no request has arrived yet".
    jinn_testutil::wait::await_settled(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(10),
    )
    .await;
    let racing_requests: Vec<SendToLlmProvider> = recorder.drain();
    assert!(
        racing_requests.is_empty(),
        "a dispatch racing a cancel must publish no SendToLlmProvider; got {}",
        racing_requests.len()
    );
}

// ---------------------------------------------------------------------------
// R3 — the racing dispatch arms no stall timer
// ---------------------------------------------------------------------------

/// R3: with a 1-second stall window, a racing dispatch after a cancel
/// must never produce a stall-retry marker — no provider request, no
/// arm, no trip.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn cancel_then_racing_dispatch_arms_no_stall_timer() {
    // Given a composed app whose stall window is one second and whose
    // streams hang (so an admitted racing dispatch arms the watchdog
    // against real silence).
    let (app, session_id) = composed_app(hung_factory(), 1).await;
    seed_streaming(&app, &session_id);

    // When the cancel lands and settles, then the racing dispatch, and
    // the stall window is given several chances to elapse.
    publish_cancel(&app, &session_id).await;
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;

    // Then no stall-retry marker ever lands in the history.
    // The stall window is one second, so the wait must outlast it: the
    // history must have stopped changing for longer than the watchdog
    // would take to fire.
    jinn_testutil::wait::await_value_settled(
        Duration::from_millis(500),
        Duration::from_secs(10),
        || {
            app.core
                .state
                .read()
                .session
                .get(&session_id)
                .map(|s| s.history().len())
                .unwrap_or_default()
        },
    )
    .await;
    let stalled = app
        .core
        .state
        .read()
        .session
        .get(&session_id)
        .is_some_and(|s| {
            s.history()
                .iter()
                .any(|e| e.kind_str() == "system" && e.text().contains("LLM stream stalled"))
        });
    assert!(
        !stalled,
        "a dispatch racing a cancel must arm no stall timer"
    );
}

// ---------------------------------------------------------------------------
// R4 — the user's next message after the race starts exactly one turn
// ---------------------------------------------------------------------------

/// R4: after a cancel and a racing dispatch, the user's next message
/// must dispatch exactly one turn and land the session back in `Idle`
/// when it finishes.
///
/// Today: the wedged `Streaming` phase queues the user's message
/// instead of dispatching it, and the turn never starts.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn user_message_after_cancel_dispatches_exactly_one_turn() {
    // Given the seeded mid-turn session, the cancel/race pair, and a
    // recorder drained of the pre-cancel traffic. The factory hangs, so
    // today's wedged `Streaming` phase (the admitted racing dispatch)
    // never settles and the user's message queues forever.
    let (app, session_id, recorder) = composed_app_with_recorder(hung_factory(), 30).await;
    seed_streaming(&app, &session_id);
    publish_cancel(&app, &session_id).await;
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;
    // The recorder must have stopped changing before it is drained, so
    // the drain captures every request the race produced rather than
    // whatever had arrived at the moment of the drain.
    jinn_testutil::wait::await_settled(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(10),
    )
    .await;
    recorder.drain();

    // When the user sends a message.
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("after the race"),
        })
        .await;

    // Then exactly one provider request goes out for the user's
    // message. (The stream it starts hangs by construction; the count
    // is the behavior under test, not the completion.)
    let arrived = wait_for_recorder(&recorder, 1).await;
    assert_eq!(
        arrived.len(),
        1,
        "the user's next message must dispatch exactly one turn; got {}",
        arrived.len()
    );
    // No second turn may start behind the user's message: the recorder
    // must have stopped changing first, so "no second turn" means "no
    // second turn will come" rather than "none has arrived yet".
    jinn_testutil::wait::await_settled(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(10),
    )
    .await;
    let total = arrived.len() + recorder.drain().len();
    assert_eq!(
        total, 1,
        "no second turn may start behind the user's message"
    );
}

// ---------------------------------------------------------------------------
// R6 — no entry point re-enters Streaming without a live generation
// ---------------------------------------------------------------------------

/// R6: a `StreamToken` arriving while the session is mid-dispatch
/// (`Sending`) must not arm the phase to `Streaming` when the turn has
/// been cancelled — no live generation may back the phase.
///
/// Today: `on_stream_token`'s defensive `begin_streaming()` arms the
/// phase for any token seen in `Sending`, cancelled or not. The cancel
/// settles through `Sending`, the late token arrives, and the session
/// is `Streaming` with no stream.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn stream_token_after_cancel_does_not_reach_streaming() {
    // Given a session in `Sending` — the pre-dispatch phase a racing
    // dispatch leaves behind, with no live generation stamped yet.
    let (app, session_id) = composed_app(hung_factory(), 30).await;
    let seed_stamp = jiff::Timestamp::now();
    app.core.state.with_session(|view| {
        let session = view.session.map().get_or_create(&session_id);
        session.begin_sending();
    });
    assert_eq!(phase_of(&app, &session_id), Some(PhaseView::Working));
    // (The stale token arrives during this assert's window.)

    // When a stale token arrives while the session is still `Sending` —
    // the delivery from a generation whose dispatch was refused (no
    // cancel needed: the phase was never settled away from `Sending`).
    app.services
        .bus
        .publish(StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "late".to_owned(),
            is_thinking: false,
            dispatched_at: seed_stamp,
        })
        .await;

    // Then the phase must not be `Streaming`: a token with no live
    // generation behind it may not arm the phase.
    // The phase must have stopped changing first, so "not Streaming" is
    // an observation rather than a race with the actor.
    jinn_testutil::wait::await_value_settled(
        Duration::from_millis(200),
        Duration::from_secs(10),
        || phase_of(&app, &session_id),
    )
    .await;
    assert!(
        !is_streaming(&app, &session_id),
        "a token with no live generation must not arm the phase to Streaming"
    );
}

// ---------------------------------------------------------------------------
// R5 lives in the sidebar's own test module:
// crates/slices/jinn-sidebar/src/sections/sessions_tests.rs
// :: busy_session_with_idle_phase_reports_idle_in_sidebar  (demonstrated RED)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// R7 — ESC twice settles the phase to Idle (the user-facing cancel)
// ---------------------------------------------------------------------------

/// The user's cancel gesture, end to end: mid-turn, press ESC twice —
/// once to raise the confirmation, once to confirm — and the session's
/// phase settles to `Idle`. This is the whole observable of the cancel;
/// every internal mechanism exists to make this true.
///
/// Driven through the real intent handler (`route_intent`) the way the
/// live app receives keys, over the composed fabric, so nothing between
/// the keypress and the phase actor can be assumed.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn esc_esc_mid_turn_settles_the_phase_idle() {
    // Given a composed app mid-turn: the queue actor dispatched the
    // user's message for real, the inference actor started a stream
    // (one token, then hung — the turn is genuinely in flight, not
    // seeded), and the session is `Streaming`.
    let (mut app, session_id) = composed_app(hung_factory(), 30).await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("start a turn"),
        })
        .await;
    let streaming = wait_until_phase(&app, &session_id, PhaseView::Streaming).await;
    assert!(streaming, "setup: the dispatched turn reached Streaming");

    // When the user presses ESC twice: raise the cancel prompt, then
    // confirm it.
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);
    assert!(
        app.core.state.read().frontend.cancel_stream_prompt,
        "the first ESC arms the cancel prompt"
    );
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);

    // Then the phase settles to `Idle` — the spinner stops, the session
    // takes new input, and nothing downstream of the cancel keeps it
    // busy.
    // Then the phase settles to `Idle` — the spinner stops, the session
    // takes new input, and nothing downstream of the cancel keeps it
    // busy.
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(
        settled,
        "ESC ESC must settle the phase to Idle; it is {:?}",
        phase_of(&app, &session_id)
    );

    // And the session takes a new turn: the cancelled one is gone, not
    // wedged — a fresh dispatch reaches Streaming again.
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("again"),
        })
        .await;
    let restreaming = wait_until_phase(&app, &session_id, PhaseView::Streaming).await;
    assert!(
        restreaming,
        "a turn after the cancel must dispatch cleanly; phase is {:?}",
        phase_of(&app, &session_id)
    );
}

// ---------------------------------------------------------------------------
// ESC-ESC buffer dump — the confirmed cancel hands the buffered work back
// ---------------------------------------------------------------------------

/// The full ESC-ESC dump, end to end: mid-turn with a steering fragment and
/// a queued message buffered, press ESC twice — raise the confirmation,
/// confirm — and the input box holds both texts (steering first, joined by
/// the drain helper's separator), exactly one `TurnCompleted(Canceled)` is
/// published, the phase settles `Idle`, and no provider request fires for
/// the drained content: the buffers are empty when the queue actor's idle
/// transition reads them, so there is nothing to re-dispatch.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn esc_esc_mid_turn_dumps_steering_and_queue_into_the_input() {
    // Given a composed app mid-turn (a real dispatched turn hung on the
    // provider), with one steering fragment submitted behind it and one
    // message queued, plus a `TurnCompleted` tap. Provider-request absence
    // is asserted through the buffer-empty reads: an empty steering buffer
    // and empty queue give the idle transition nothing to dispatch, which
    // is the mechanism the no-request fact rides.
    let (mut app, session_id, _recorder, turns) =
        composed_app_with_recorders(hung_factory(), 30, TurnCompletedTap).await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("start a turn"),
        })
        .await;
    let streaming = wait_until_phase(&app, &session_id, PhaseView::Streaming).await;
    assert!(streaming, "setup: the dispatched turn reached Streaming");
    app.services
        .bus
        .publish(jinn_chat_input_msg::SubmitSteeringMessage {
            session_id: session_id.clone(),
            text: "steer it".to_owned(),
        })
        .await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("queued work"),
        })
        .await;

    // When the user presses ESC twice: raise the cancel prompt, then
    // confirm it.
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);
    assert!(
        app.core.state.read().frontend.cancel_stream_prompt,
        "the first ESC arms the cancel prompt"
    );
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(
        settled,
        "ESC ESC must settle the phase to Idle; it is {:?}",
        phase_of(&app, &session_id)
    );

    // Then both buffered texts are the input box's draft, steering first.
    // Polled rather than read once: the dump runs in the session actor's
    // settle, which the phase event trails.
    let deadline = tokio::time::Instant::now() + WAIT;
    let draft = loop {
        let text = app
            .core
            .state
            .read()
            .session
            .get(&session_id)
            .map_or(String::new(), |s| {
                s.with_input(|i| i.text().to_owned(), String::new)
            });
        if text.contains("steer it") && text.contains("queued work") {
            break text;
        }
        if tokio::time::Instant::now() >= deadline {
            break text;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        draft, "steer it\n\n---\n\nqueued work",
        "the confirmed cancel must dump the steering buffer and the queue \
         into the input box, steering first"
    );

    // And the buffers are empty — the dump drained them, so the idle
    // transition has nothing to re-dispatch, and no provider request can
    // fire for the drained content.
    let (queue_len, steering_len) = app
        .core
        .state
        .read()
        .session
        .get(&session_id)
        .map_or((usize::MAX, usize::MAX), |s| {
            (s.queue_len(), s.steering_buffer().len())
        });
    assert_eq!(queue_len, 0, "the queue was dumped, not kept");
    assert_eq!(steering_len, 0, "the steering buffer was dumped, not kept");

    // And exactly one cancelled-turn report was published, after the
    // recorder has stopped changing — "exactly one" means "one will have
    // come", not "one so far".
    jinn_testutil::wait::await_settled(&turns, Duration::from_millis(300), Duration::from_secs(10))
        .await;
    let reports = turns.drain();
    assert_eq!(
        reports.len(),
        1,
        "the cancelled turn reports exactly once; got {reports:?}"
    );
    assert_eq!(reports[0].outcome, TurnOutcome::Canceled);
}

/// A busy session (a hung stream) cancelled via `CancelTurn` settles to
/// `Idle` exactly once: one `TurnCompleted(Canceled)` is published — the
/// settle runs on the work-set-empty edge, and a drain that armed twice
/// (double `CancelTurn`) must not settle or report a second time.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn cancel_reports_canceled_exactly_once_and_settles() {
    // Given a composed app mid-turn with a hung stream (the turn is
    // genuinely in flight) and a `TurnCompleted` recorder.
    let (app, session_id, _recorder, turns) =
        composed_app_with_recorders(hung_factory(), 30, TurnCompletedTap).await;
    seed_streaming(&app, &session_id);

    // When the user cancels — twice, the double-press shape the UI can
    // produce while the first cancel is still draining.
    publish_cancel(&app, &session_id).await;
    publish_cancel(&app, &session_id).await;
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");

    // Then exactly one cancelled-turn report was published, and the
    // session takes a new turn afterwards (the ledger entry is gone, not
    // wedged open by the drain).
    jinn_testutil::wait::await_settled(&turns, Duration::from_millis(300), Duration::from_secs(10))
        .await;
    let reports = turns.drain();
    assert_eq!(
        reports.len(),
        1,
        "a cancelled turn must report Canceled exactly once; got {reports:?}"
    );
    assert_eq!(reports[0].outcome, TurnOutcome::Canceled);

    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("after the cancel"),
        })
        .await;
    let restreaming = wait_until_phase(&app, &session_id, PhaseView::Streaming).await;
    assert!(
        restreaming,
        "the session must accept a new turn after the drain; phase is {:?}",
        phase_of(&app, &session_id)
    );
}

/// A message enqueued while an ESC-ESC cancel drain is still in flight is
/// dumped with the rest — it was meant to ride the cancelled turn, and the
/// confirmed cancel hands buffered work back instead of running it. The
/// raced enqueue lands in the input box alongside whatever was already
/// buffered, and no provider request fires for it.
///
/// A message enqueued **after** the settle has its own coverage in
/// [`user_message_after_cancel_dispatches_exactly_one_turn`]: it starts
/// exactly one turn, whatever the ordering.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn enqueued_while_the_esc_esc_drain_is_in_flight_is_dumped_not_dispatched() {
    // Given a composed app mid-turn — a real dispatched turn hung on the
    // provider — so the ESC-ESC gesture has a live turn to cancel. The
    // factory hangs, so the drain has a real stream abort to ride and a
    // real settle to race against.
    let (mut app, session_id, recorder, _turns) =
        composed_app_with_recorders(hung_factory(), 30, TurnCompletedTap).await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("start a turn"),
        })
        .await;
    let streaming = wait_until_phase(&app, &session_id, PhaseView::Streaming).await;
    assert!(streaming, "setup: the dispatched turn reached Streaming");

    // When the ESC-ESC cancel lands and the user's next message is enqueued
    // right behind it — the publishes race, so the enqueue may land
    // mid-drain or after the settle; the dump contract must not care which.
    // Driven through the real intent handler: the confirmed branch is the
    // only publisher of a drain-flagged cancel.
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);
    app.route_intent(jinn_kernel::KernelIntent::NormalEscape);
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("raced into the drain"),
        })
        .await;

    // Then the raced message is the input box's draft — dumped, not
    // dispatched. But the session must be `Idle` first: a draft produced by
    // the dump only proves the drain ran, and the settle is what proves the
    // race window closed. The recorder's pre-cancel request (the turn's own
    // dispatch) is drained first, so only post-cancel traffic counts.
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;
    assert!(settled, "the cancel must settle the session to Idle");
    recorder.drain();
    let deadline = tokio::time::Instant::now() + WAIT;
    let draft = loop {
        let text = app
            .core
            .state
            .read()
            .session
            .get(&session_id)
            .map_or(String::new(), |s| {
                s.with_input(|i| i.text().to_owned(), String::new)
            });
        if text.contains("raced into the drain") {
            break text;
        }
        if tokio::time::Instant::now() >= deadline {
            break text;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        draft.contains("raced into the drain"),
        "an enqueue racing the ESC-ESC drain must be dumped into the input \
         box, not dispatched; input is {draft:?}"
    );

    // And no provider request fired for it: the recorder must have stopped
    // changing first, so "no request" means "no request will come".
    jinn_testutil::wait::await_settled(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(10),
    )
    .await;
    let total = recorder.drain().len();
    assert_eq!(
        total, 0,
        "the raced enqueue must not dispatch; got {total} request(s)"
    );
}

/// The unflagged shape of the same race: a message enqueued behind a
/// **non-ESC** cancel dispatches exactly once, whatever the interleaving —
/// the unflagged cancel keeps the queue for the idle transition, and a
/// message landing after the settle dispatches directly. The one forbidden
/// outcome is zero or two dispatches.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn message_enqueued_during_the_drain_dispatches_exactly_once() {
    // Given a composed app mid-turn with a hung stream. The factory hangs,
    // so the drain has a real stream abort to ride and a real settle to
    // race against.
    let (app, session_id, recorder, _turns) =
        composed_app_with_recorders(hung_factory(), 30, TurnCompletedTap).await;

    // When the cancel lands (unflagged — the bot/watchdog/cascade shape)
    // and the user's next message is enqueued right behind it.
    publish_cancel(&app, &session_id).await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("queued mid-drain"),
        })
        .await;

    // Then exactly one provider request goes out for the user's message.
    // One request is also the drain's convergence proof: the idle
    // transition that fires it only happens after the settle, so a drain
    // that never settled produces zero.
    let arrived = wait_for_recorder(&recorder, 1).await;
    jinn_testutil::wait::await_settled(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(10),
    )
    .await;
    let total = arrived.len() + recorder.drain().len();
    assert_eq!(
        total, 1,
        "the drain-time enqueue must produce exactly one dispatch, whatever the ordering"
    );
}

// ---------------------------------------------------------------------------
// Provider-error settlement — the emission-fix's end-to-end proof
// ---------------------------------------------------------------------------

/// A factory whose stream errors immediately at creation — the provider
/// down / auth-rejected shape a real backend produces. Exists here because
/// the inference crate's equivalent is test-private.
#[derive(Debug)]
struct ErroringStreamFactory;

impl jinn_provider::LlmServiceFactory for ErroringStreamFactory {
    fn create(
        &self,
    ) -> Result<
        Box<dyn jinn_provider::LlmService>,
        error_stack::Report<jinn_provider::LlmServiceError>,
    > {
        Ok(Box::new(ErroringStreamService))
    }
    fn name(&self) -> &'static str {
        "ErroringStream"
    }
}

#[derive(Debug)]
struct ErroringStreamService;

#[async_trait::async_trait]
impl jinn_provider::LlmService for ErroringStreamService {
    fn name(&self) -> &'static str {
        "ErroringStream"
    }
    async fn chat_stream(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<jinn_provider::LlmMessage>,
    ) -> Result<jinn_provider::ChatStream, error_stack::Report<jinn_provider::LlmServiceError>>
    {
        Err(error_stack::Report::new(
            jinn_provider::LlmServiceError::Provider,
        ))
    }
    async fn chat_stream_with_tools(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<jinn_provider::LlmMessage>,
        _tools: Vec<jinn_core_types::ToolDefinition>,
    ) -> Result<jinn_provider::ToolStream, error_stack::Report<jinn_provider::LlmServiceError>>
    {
        Err(error_stack::Report::new(
            jinn_provider::LlmServiceError::Provider,
        ))
    }
}

/// A provider error mid-turn settles the session: the inference actor's
/// `StreamCompleted(Error)` carries the identity its dispatch minted, the
/// session actor's ledger admission accepts it, and the turn ends — phase
/// `Streaming → Idle`, exactly one `TurnCompleted(Error)`, the ledger entry
/// swept. Before the emission fix, the error event rode an unset identity,
/// admission refused it, no settle ran, and the session stayed `Streaming`
/// with a live spinner forever.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn provider_error_mid_turn_settles_the_session_idle() {
    // Given a composed app whose provider errors on every stream, driven
    // through the real dispatch path: the user's message dispatches a turn,
    // the inference actor's stream creation fails, `emit_stream_error` runs.
    let (app, session_id, _recorder, (phases, turns)) = composed_app_with_recorders(
        ErroringStreamFactory,
        30,
        (PhaseChangedTap, TurnCompletedTap),
    )
    .await;
    app.services
        .bus
        .publish(EnqueueUserMessage {
            session_id: session_id.clone(),
            entry: ChatEntry::user("provoke a provider error"),
        })
        .await;

    // When the error path runs to completion — poll rather than assume,
    // the actors are async.
    let settled = wait_until_phase(&app, &session_id, PhaseView::Idle).await;

    // Then the session is `Idle`: the spinner stops, the session takes
    // new input.
    assert!(
        settled,
        "a provider error must settle the session to Idle; phase is {:?}",
        phase_of(&app, &session_id)
    );

    // And the phase projection recorded the turn's full arc — the error
    // settled through the ledger, not a phase shortcut. The tap actor
    // trails the publish, so poll for the event instead of draining once.
    let deadline = tokio::time::Instant::now() + WAIT;
    let phase_events = loop {
        let drained = phases.drain();
        if drained
            .iter()
            .any(|e| e.old_phase == PhaseView::Streaming && e.new_phase == PhaseView::Idle)
        {
            break drained;
        }
        if tokio::time::Instant::now() >= deadline {
            break drained;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        phase_events
            .iter()
            .any(|e| e.old_phase == PhaseView::Streaming && e.new_phase == PhaseView::Idle),
        "expected SessionPhaseChanged(Streaming → Idle); got {phase_events:?}"
    );

    // And exactly one `TurnCompleted(Error)` reported the turn — the
    // once-ness the ledger's settle-once guarantees.
    jinn_testutil::wait::await_settled(&turns, Duration::from_millis(300), Duration::from_secs(10))
        .await;
    let reports = turns.drain();
    assert_eq!(
        reports.len(),
        1,
        "an errored turn reports exactly once; got {reports:?}"
    );
    assert_eq!(reports[0].outcome, TurnOutcome::Error);

    // And the ledger entry is swept: the session takes a new turn.
    let grant = app.services.turn_ledger.mint(
        &session_id,
        jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
    );
    assert!(
        grant.is_some(),
        "a settled turn's entry must be swept; a fresh mint must be admitted"
    );
}

/// The marker naming one `SessionPhaseChanged` recorder.
struct PhaseChangedTap;

impl ExtraRecorders for PhaseChangedTap {
    type Recorders = jinn_testutil::bus_harness::Recorder<jinn_session_msg::SessionPhaseChanged>;

    async fn spawn(&self, harness: &jinn_testutil::bus_harness::TestHarness) -> Self::Recorders {
        harness
            .spawn_recorder::<jinn_session_msg::SessionPhaseChanged>()
            .await
    }
}
