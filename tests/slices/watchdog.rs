//! End-to-end crossing test for the watchdog slice: a composed app whose
//! stall watchdog is configured with a 1-second window; publishing
//! `SendToLlmProvider` arms the actor, silence past the window trips it,
//! and the visible retry marker lands in the session's history through
//! the real fabric (watchdog → `PushChatEntry` → session actor fold).
//!
//! The harness (`launch_for_test`) activates every slice but does NOT
//! spawn the kernel's `SessionPersistenceActor` — the fold that consumes
//! `PushChatEntry` / `RetryStalledSession`. This file needs the full
//! crossing, so its composed helper spawns that actor itself (the
//! production deps shape from `actor_wiring.rs`, with the fake-session
//! store the harness already carries) before the slices activate.
//!
//! The inference actor is armed with a hung stream factory: the turn
//! dispatches cleanly, produces one token, then goes silent forever —
//! no `StreamCompleted` — which is exactly the silence the watchdog
//! exists to catch. (The harness default factory is empty: factory
//! resolution would fail and emit `StreamCompleted(Error)`, disarming
//! the watchdog per the end-reason policy.)

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_inference_msg::SendToLlmProvider;
use jinn_kernel::AppCore;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_llm_support::token_estimator::TiktokenCounter;
use jinn_preferences_config::StallWatchdogConfig;
use jinn_tui::TuiApp;

use crate::common::launch_for_test;

/// A composed app whose `[watchdog.stall]` window is one second, so a
/// silent stream trips within the test timeout. The section is written
/// into the services' configuration layer **before** `launch_for_test`
/// activates the slices — activation reads the knobs from the layer
/// once.
///
/// Also spawns the kernel session actor over the SAME `State` and the
/// SAME trouper system the harness wires, so the watchdog's marker entry
/// and retry command have their real consumer.
async fn composed_app_with_fast_stall_watchdog() -> (TuiApp, SessionId) {
    let services = jinn_kernel::Services::new_fake().await;
    // Arm the inference actor with a hung stream (see module docs): the
    // dispatch resolves, streams one token, and never completes — the
    // watchdog's window elapses with the turn genuinely in flight.
    let hung_factory = jinn_provider::HungStreamFactory::new();
    services.llm_service.swap(std::sync::Arc::new(hung_factory));

    let state = jinn_kernel::State::new(jinn_kernel::AppState::default());
    // The watchdog reads its knobs from the layer at activation, so the
    // section is seeded before the slices come up.
    services
        .config
        .put::<StallWatchdogConfig>(&StallWatchdogConfig {
            timeout_secs: 1,
            max_restarts: 3,
        })
        .expect("write the stall watchdog section");
    let core = AppCore {
        state: state.clone(),
        bridge: services.bridge.clone(),
    };

    // The context-assembly service answers the queue actor's assemble ask
    // on the stall-retry re-dispatch path (must exist before any ask).
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
            handoff_prompt: String::new(),
        },
    );

    let app = launch_for_test(core, services).await;
    let session_id = app.core.state.read().session.active_session_id().clone();
    (app, session_id)
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn silent_stream_trips_the_stall_watchdog_and_the_marker_lands_in_history() {
    // Given a composed app with a 1-second stall window and a dispatch
    // for its active session (the watchdog arms on receipt).
    let (app, session_id) = composed_app_with_fast_stall_watchdog().await;
    let dispatched_at = jiff::Timestamp::now();
    let dispatch: SendToLlmProvider = serde_json::from_value(serde_json::json!({
        "session_id": session_id.to_string(),
        "messages": [],
        "dispatched_at": dispatched_at.to_string(),
    }))
    .expect("minimal SendToLlmProvider deserializes");

    {
        // Seed the phase machine: the session sits in Streaming with a
        // partial assistant entry. (The queue actor drives this in
        // production; this test publishes the dispatch directly, so it
        // seeds the phase itself. The in-flight-stream guard needs no
        // seeding — the session actor's dispatch receipt arms it from
        // the real `dispatched_at`.)
        app.core.state.with_session(|view| {
            let session = view.session.map().get_or_create(&session_id);
            session.begin_streaming();
            session
                .append_stream_token("warm", dispatched_at)
                .expect("warm token registers the streaming generation");
        });
    }

    // When the dispatch is published on the fabric — the same
    // `BusService::publish` broadcast the queue actor performs in
    // production, so every `.handles` declarant receives a copy
    // (watchdog, session-actor guard, inference actor). The stream
    // hangs after one token and never completes.
    app.services.bus.publish(dispatch).await;

    // Then, within the (1s) window plus tick latency, the watchdog trips
    // and the marker lands: watchdog → `RetryStalledSession` → session
    // actor's guard accepts → its fold discards the partial entry and
    // pushes the marker via its own `PushChatEntry` handling. Polled
    // against the session's history (the user-visible truth).
    assert!(
        wait_for_stall_marker(&app, &session_id).await,
        "stall retry marker must land in the session history through the real fabric"
    );
}

/// Polls the session's history for the stall-retry marker, giving up after
/// twenty seconds and panicking with the diagnostics that explain why.
async fn wait_for_stall_marker(app: &TuiApp, session_id: &SessionId) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let tripped = app
            .core
            .state
            .read()
            .session
            .get(session_id)
            .is_some_and(|s| {
                s.history().iter().any(|e| {
                    e.kind_str() == "system"
                        && e.text()
                            .contains("LLM stream stalled, retrying (attempt 1 of")
                })
            });
        if tripped {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            let history_kinds = app
                .core
                .state
                .read()
                .session
                .get(session_id)
                .map_or_default(|s| {
                    s.history()
                        .iter()
                        .map(|e| e.kind_str().to_owned())
                        .collect::<Vec<_>>()
                });
            let dead = app.services.trouper_system.dead_letter_count().await;
            panic!(
                "marker never landed; diagnostics: dead letters = {dead}, \
                 history kinds now = {history_kinds:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
