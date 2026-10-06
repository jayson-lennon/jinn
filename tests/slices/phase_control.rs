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
use jinn_session_msg::PhaseKind;
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
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_preferences_config::StallWatchdogConfig;
    use jinn_testutil::bus_harness::TestHarness;

    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;

    let services = harness.services().await;
    services.llm_service.swap(std::sync::Arc::new(factory));

    let state = jinn_kernel::State::new(jinn_kernel::AppState::default());
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
    (app, session_id, recorder)
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
    app.core.state.with_session(|view| {
        let session = view.session.map().get_or_create(session_id);
        session.begin_streaming();
        session
            .append_stream_token("warm", dispatched_at)
            .expect("warm token registers the streaming generation");
    });
}

/// True when the named session's phase is `Streaming`.
fn is_streaming(app: &TuiApp, session_id: &SessionId) -> bool {
    phase_of(app, session_id) == Some(PhaseKind::Streaming)
}

/// The named session's phase, if it exists.
fn phase_of(app: &TuiApp, session_id: &SessionId) -> Option<PhaseKind> {
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
async fn wait_until_phase(app: &TuiApp, session_id: &SessionId, wanted: PhaseKind) -> bool {
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
    let settled = wait_until_phase(&app, &session_id, PhaseKind::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");

    // ...and the racing dispatch arrives after the settle — the
    // "resume queued before the cancel" shape arriving late.
    publish_racing_dispatch(&app, &session_id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
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
    let settled = wait_until_phase(&app, &session_id, PhaseKind::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;

    // Then no provider request is published for the racing dispatch.
    // The grace window gives the queue actor every chance to publish.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
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
    let settled = wait_until_phase(&app, &session_id, PhaseKind::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;

    // Then no stall-retry marker ever lands in the history.
    tokio::time::sleep(Duration::from_secs(4)).await;
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
    let settled = wait_until_phase(&app, &session_id, PhaseKind::Idle).await;
    assert!(settled, "cancel must settle the session to Idle");
    publish_racing_dispatch(&app, &session_id).await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
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
    tokio::time::sleep(Duration::from_millis(500)).await;
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
    assert_eq!(phase_of(&app, &session_id), Some(PhaseKind::Sending));
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
    tokio::time::sleep(Duration::from_millis(500)).await;
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
