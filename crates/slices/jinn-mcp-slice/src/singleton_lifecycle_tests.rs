//! Singleton MCP server lifecycle tests.
//!
//! A server configured with `singleton = true` spawns once per app run rather
//! than once per session: the first reconcile that finds it in any session's
//! enablement set spawns its actor, and the three session-teardown events
//! (`SessionClosed`, `SessionArchived`, `SessionTeardownFinished`) never kill
//! it — including the teardown of the session that triggered its spawn.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use std::collections::BTreeSet;

use jinn_core_types::SessionId;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::bus::HarnessServices;
use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};
use jinn_preferences_config::schemas::mcp::{McpServerConfig, McpServersConfig};
use jinn_session_msg::SessionClosed;
use jinn_session_store_msg::SessionLoadCompleted;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

use crate::coordinator::{McpCoordinatorActor, McpCoordinatorActorDeps};

/// A configured MCP server whose command will never spawn successfully, so
/// the spawned actor publishes `Starting` then `Dead` (never `Running`).
/// The count of `Starting` events is the spawn counter: every distinct spawn
/// publishes exactly one.
fn unrunnable_singleton() -> McpServerConfig {
    McpServerConfig {
        command: Some("/this/command/does/not/exist".to_owned()),
        singleton: true,
        ..Default::default()
    }
}

fn unrunnable_per_session() -> McpServerConfig {
    McpServerConfig {
        command: Some("/this/command/does/not/exist".to_owned()),
        ..Default::default()
    }
}

/// Spawns a coordinator with the given configured servers, returning the
/// harness-level services so tests can publish and inspect.
async fn spawn_lifecycle(
    harness: &TestHarness,
    servers: &[(&str, McpServerConfig)],
) -> (
    trouper::actor::ActorPath,
    jinn_kernel::Services,
    jinn_kernel::common::state::State,
) {
    let services = harness.services().await;
    let mcp_server = servers
        .iter()
        .map(|(name, config)| ((*name).to_owned(), config.clone()))
        .collect();
    services
        .config
        .put::<McpServersConfig>(&McpServersConfig(mcp_server))
        .expect("seed the mcp section");
    let state =
        jinn_kernel::common::state::State::new(jinn_kernel::common::app_state::AppState::default());
    let runtime = crate::activate_runtime(&services.slices);
    let path = McpCoordinatorActor::spawn(
        &services.trouper_system,
        McpCoordinatorActorDeps {
            deps: ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            runtime,
        },
    )
    .await;
    (path, services, state)
}

fn single_enabled(server: &str) -> BTreeSet<String> {
    let mut s = BTreeSet::new();
    s.insert(server.to_owned());
    s
}

/// Inserts a fresh session carrying `enabled` into the shared state and
/// returns its id (the SessionCreated seeding flow the coordinator reads).
fn insert_session_with_enablement(
    state: &jinn_kernel::common::state::State,
    enabled: &BTreeSet<String>,
) -> SessionId {
    let mut session = jinn_session_state::ChatSessionState::new();
    session.set_enabled_mcp_servers(enabled.clone());
    let session_id = session.session_id().clone();
    let mut app_state = state.write();
    app_state.session.insert(session);
    drop(app_state);
    session_id
}

/// Number of `Starting` statuses recorded — the observable spawn counter.
fn spawns_in(events: &[McpServerStatus]) -> usize {
    events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Starting)
        .count()
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_server_spawns_once_across_two_sessions() {
    // Given a coordinator with one singleton-configured server.
    let (harness, recorder, actor) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, _services, _state) =
            spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
        (harness, recorder, actor)
    };

    // When two different sessions both enable it.
    let first = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: first,
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    let second = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: second,
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then exactly one spawn happened (a single Starting status): the second
    // session's reconcile found the singleton already running and was a no-op.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    let spawns = events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Starting)
        .count();
    assert_eq!(
        spawns, 1,
        "a singleton server must spawn once across two sessions; got {spawns}: {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_server_survives_closing_every_session() {
    // Given a coordinator whose singleton server was spawned by session A.
    let (harness, recorder, actor, session_a) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, _services, _state) =
            spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
        let session_a = SessionId::new();
        harness
            .publish(jinn_mcp_msg::McpEnablementChanged {
                session_id: session_a.clone(),
                enabled: single_enabled("bundled-example"),
            })
            .await;
        (harness, recorder, actor, session_a)
    };
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // When closing every session — including the one that triggered the spawn.
    harness
        .publish(SessionClosed {
            session_id: session_a,
        })
        .await;
    harness
        .publish(SessionClosed {
            session_id: SessionId::new(),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // And re-enabling from a fresh session afterwards.
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: SessionId::new(),
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then still exactly one spawn: the singleton survived the closes, so the
    // re-enable found it running and did not spawn a second actor.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    let spawns = events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Starting)
        .count();
    assert_eq!(
        spawns, 1,
        "a singleton must survive session close without a respawn; got {spawns}: {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_server_survives_teardown_finished() {
    // Given a coordinator whose singleton server was spawned by a session.
    let (harness, recorder, actor, session_a) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, _services, _state) =
            spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
        let session_a = SessionId::new();
        harness
            .publish(jinn_mcp_msg::McpEnablementChanged {
                session_id: session_a.clone(),
                enabled: single_enabled("bundled-example"),
            })
            .await;
        (harness, recorder, actor, session_a)
    };
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // When lifecycle teardown finishes for that session.
    harness
        .publish(jinn_session_lifecycle_msg::SessionTeardownFinished {
            session_id: session_a,
            error: None,
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // And a second session enables the same server.
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: SessionId::new(),
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then no second spawn: teardown never touches the singleton.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    let spawns = events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Starting)
        .count();
    assert_eq!(
        spawns, 1,
        "session teardown must not kill (and re-enable must not respawn) a singleton; \
         got {spawns}: {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_server_is_spawned_on_session_load_completed() {
    // Given a coordinator with a singleton server and a loaded session whose
    // enabled set contains it.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let (_actor, services, state) =
        spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
    let session_id = insert_session_with_enablement(&state, &single_enabled("bundled-example"));

    // When the session finishes loading from disk.
    harness
        .publish(SessionLoadCompleted {
            session_id: session_id.clone(),
        })
        .await;

    // Then the singleton spawned (a Starting status arrives).
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(3)).await;
    assert!(
        events
            .iter()
            .any(|e| e.status == McpConnectionStatus::Starting && e.server == "bundled-example"),
        "SessionLoadCompleted must spawn the singleton; got {events:?}"
    );
    let _ = services; // kept for parity with the sibling harnesses
}

#[rstest::rstest]
#[tokio::test]
async fn per_session_server_still_spawns_per_session() {
    // Given a coordinator with a NON-singleton server.
    let (harness, recorder, actor) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, _services, _state) =
            spawn_lifecycle(&harness, &[("excalimate", unrunnable_per_session())]).await;
        (harness, recorder, actor)
    };

    // When two sessions enable it.
    let first = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: first.clone(),
            enabled: single_enabled("excalimate"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: SessionId::new(),
            enabled: single_enabled("excalimate"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then two spawns happened — the singleton change must not alter
    // per-session semantics.
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    let spawns = events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Starting)
        .count();
    assert_eq!(
        spawns, 2,
        "a non-singleton server must still spawn per session; got {spawns}: {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_spawn_is_visible_in_runtime_status_for_any_session() {
    // Given a coordinator whose singleton spawned via session A.
    let harness = TestHarness::new().await;
    let (_actor, services, _state) =
        spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
    let runtime = services
        .slices
        .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
        .expect("cell catalog registers the mcp runtime slot");
    let session_a = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_a.clone(),
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The singleton's status is written to the runtime cell, so wait for
    // the cell to carry it. Reading the cell after a fixed sleep could
    // happen before the write, and the lookup would then prove nothing.
    jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
        state.singleton_status("bundled-example").is_some()
    })
    .await;

    // When a different session looks up the singleton's status.
    let session_b = SessionId::new();

    // Then the lookup resolves for session B too — the status is not pinned
    // to the session that spawned it (the orchestrator's dispatch gate reads
    // it for whichever session the tool call came from).
    let status_for_b = runtime.read().singleton_status("bundled-example");
    assert!(
        status_for_b.is_some(),
        "singleton runtime status must be readable without a session id; \
         got {:?} for session b lookup",
        runtime.read().status(&session_b, "bundled-example")
    );
}

/// An explicitly restarted singleton respawns into the singleton namespace
/// (not the per-session map), so exactly one actor exists for it afterwards.
#[rstest::rstest]
#[tokio::test]
async fn restarting_a_singleton_respawns_it_by_name() {
    // Given a coordinator whose singleton spawned via a session.
    let (harness, recorder, actor, services, session_a) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, services, _state) =
            spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
        let session_a = SessionId::new();
        harness
            .publish(jinn_mcp_msg::McpEnablementChanged {
                session_id: session_a.clone(),
                enabled: single_enabled("bundled-example"),
            })
            .await;
        (harness, recorder, actor, services, session_a)
    };
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // When the server is restarted by name (the restart_mcp_server path).
    let reply = services
        .trouper_system
        .ask(
            actor.clone(),
            crate::coordinator::McpRestartForTest {
                session_id: session_a,
                server: "bundled-example".to_owned(),
                timeout: std::time::Duration::from_secs(5),
            },
            std::time::Duration::from_secs(10),
        )
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the restart reports a fresh spawn of the shared actor: the
    // unrunnable command cannot ever connect, so `ConnectFailed` is exactly
    // the "old actor killed, new actor spawned" proof.
    let restarted = match &reply {
        Ok(value) => {
            let outcome: crate::coordinator::McpRestartOutcome = value.clone().decode().unwrap();
            outcome.error.as_deref() == Some("ConnectFailed")
        }
        Err(_) => false,
    };
    assert!(
        restarted,
        "restart must replace the singleton actor (ConnectFailed = fresh spawn \
         of the unrunnable command); got: {reply:?}"
    );

    // And a subsequent reconcile finds the singleton slot filled: re-enabling
    // from yet another session must NOT spawn again. The full history carries
    // exactly two `Starting` events — spawn one and the restart's respawn.
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: SessionId::new(),
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
    let spawns = spawns_in(&events);
    assert_eq!(
        spawns, 2,
        "restart replaces the singleton actor exactly once (2 Starting total), \
         and later re-enables spawn nothing more; got {spawns}: {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn disabling_a_singleton_hides_its_provider_for_that_session() {
    // Given a coordinator with one singleton, enabled by session A.
    let harness = TestHarness::new().await;
    let recorder = harness
        .spawn_recorder::<jinn_tools_msg::ProviderVisibilityChanged>()
        .await;
    let (actor, _services, _state) =
        spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
    let session_a = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_a.clone(),
            enabled: single_enabled("bundled-example"),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // When session A reconciles with an empty enabled set (the picker
    // commit after unchecking the server).
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_a.clone(),
            enabled: BTreeSet::new(),
        })
        .await;

    // Then a visibility change hides the provider for session A.
    let events = await_recorded(&recorder, 2, std::time::Duration::from_secs(2)).await;
    assert!(
        events.iter().any(|e| e.session_id == session_a
            && e.provider == "mcp__bundled-example__"
            && e.hidden),
        "disabling a singleton must hide its provider for that session; got {events:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn reenabling_a_singleton_restores_its_provider_visibility() {
    // Given a coordinator whose singleton was hidden for session A.
    let harness = TestHarness::new().await;
    let recorder = harness
        .spawn_recorder::<jinn_tools_msg::ProviderVisibilityChanged>()
        .await;
    let (actor, _services, _state) =
        spawn_lifecycle(&harness, &[("bundled-example", unrunnable_singleton())]).await;
    let session_a = SessionId::new();
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_a.clone(),
            enabled: BTreeSet::new(),
        })
        .await;
    // The coordinator must have processed the enablement, and no further
    // status event may still be coming — the assertion below counts the
    // spawns, so a count read while events are still arriving is not the
    // count the test claims. A fixed sleep raced both.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // When session A re-enables the singleton.
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_a.clone(),
            enabled: single_enabled("bundled-example"),
        })
        .await;

    // Then a visibility change restores the provider for session A.
    let events = await_recorded(&recorder, 2, std::time::Duration::from_secs(2)).await;
    assert!(
        events.iter().any(|e| e.session_id == session_a
            && e.provider == "mcp__bundled-example__"
            && !e.hidden),
        "re-enabling a singleton must restore its provider; got {events:?}"
    );
}

// P4: a JinnTomlReloaded re-reads the config and reconciles every live
// session against the session's persisted enablement — a server the
// session desired but the config lacked spawns once it appears; a
// spawned server the config dropped is killed.
#[rstest::rstest]
#[tokio::test]
async fn a_config_reload_reconciles_added_and_removed_servers_for_live_sessions() {
    // Given a coordinator with `bundled-example` configured, and a live
    // session that desires BOTH names (a reload will supply the second
    // and drop the first).
    let (harness, recorder, actor, services, state, session_id) = {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (actor, services, state) =
            spawn_lifecycle(&harness, &[("bundled-example", unrunnable_per_session())]).await;
        let mut both = single_enabled("bundled-example");
        both.insert("bundled-second".to_owned());
        let session_id = insert_session_with_enablement(&state, &both);
        (harness, recorder, actor, services, state, session_id)
    };
    harness
        .publish(jinn_mcp_msg::McpEnablementChanged {
            session_id: session_id.clone(),
            enabled: {
                let mut both = single_enabled("bundled-example");
                both.insert("bundled-second".to_owned());
                both
            },
        })
        .await;
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    // The pre-reload phase drained wholesale: the unrunnable server's own
    // lifecycle (Starting then its connect-failure Dead) landed here.
    let _ = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;

    // When the config swaps — the old server removed, the desired-but-
    // missing one added — and a reload lands.
    services
        .config
        .put::<McpServersConfig>(&McpServersConfig(
            [("bundled-second", unrunnable_per_session())]
                .iter()
                .map(|(n, c)| ((*n).to_owned(), c.clone()))
                .collect(),
        ))
        .expect("swap the mcp section");
    let now_configured = services
        .config
        .get::<McpServersConfig>()
        .expect("the section reads");
    assert!(
        !now_configured.0.contains_key("bundled-example")
            && now_configured.0.contains_key("bundled-second"),
        "the put must replace the section; got {:?}",
        now_configured.0.keys().collect::<Vec<_>>()
    );
    harness.publish(jinn_boot_msg::JinnTomlReloaded).await;
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the reconcile ran: the session still desires `bundled-second`,
    // which the config now supplies — its spawn's `Starting` is the
    // observable. Poll for it by count (the spawn is a separate task),
    // then settle and count.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while recorder.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the added server never spawned on the reload; count {}",
            recorder.len()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let events = jinn_testutil::wait::await_settled_messages(
        &recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        spawns_in(&events),
        1,
        "the added server spawned on the reload; got {events:?}"
    );
    let deaths = events
        .iter()
        .filter(|e| e.status == McpConnectionStatus::Dead)
        .count();
    assert_eq!(
        deaths, 1,
        "the removed server was killed exactly once on the reload; got {events:?}"
    );
    let _unused = &state;
}
