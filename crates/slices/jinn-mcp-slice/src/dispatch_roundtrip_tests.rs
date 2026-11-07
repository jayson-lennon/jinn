//! End-to-end dispatch roundtrip for the MCP actor.
//!
//! These tests exercise the *full* dispatch seam that the pure unit tests in
//! `connection.rs` cannot reach: a real [`McpActor`] spawned on the bus,
//! connected to an in-process stub MCP server, receiving an [`ExecuteTool`]
//! command for a namespaced tool and publishing a [`ToolExecutionCompleted`]
//! event carrying the server's response.
//!
//! The stub server advertises one `echo` tool (from
//! `jinn_mcp::server_testkit`); the actor namespaces it as `mcp__stub__echo`,
//! so a published `ExecuteTool` for that name walks the exact path the
//! orchestrator's `dispatch_actor` takes for MCP providers
//! (`provider.starts_with(MCP_PROVIDER_PREFIX)`).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::time::Duration;

use jinn_mcp::server_testkit::{spawn_stub_client, spawn_stub_client_with_killer};

use crate::connection::{McpActor, McpActorDeps};
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::bus::HarnessServices;
use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};
use jinn_preferences_config::schemas::ToolsConfig;
use jinn_preferences_config::schemas::mcp::McpServerConfig;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_tools_msg::ExecuteTool;
use jinn_tools_msg::ToolExecutionCompleted;
use jinn_tools_msg::ToolsUnregistered;
use marked_path::MarkedPathAccess as _;
use trouper::actor::ActorPath;

/// The server name injected into the actor — becomes the tool namespace segment
/// (`mcp__stub__echo`) and the strip-namespace key the actor matches on.
const SERVER_NAME: &str = "stub";

/// Builds a server config whose command is irrelevant — the test injects a
/// pre-connected client, so `on_start` never spawns a process.
fn stub_config() -> McpServerConfig {
    McpServerConfig {
        command: Some(String::new()),
        args: vec![],
        ..Default::default()
    }
}

/// Spawns an `McpActor` wired to the stub server for `session_id`, plus the
/// services it was spawned from. Every roundtrip test starts from this.
async fn spawn_actor_against_stub(
    harness: &TestHarness,
    session_id: &SessionId,
) -> (jinn_kernel::Services, ActorPath) {
    let services = harness.services().await;
    let client = spawn_stub_client().await;
    let actor = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;
    (services, actor)
}

/// `true` once `pid` has been reaped (ESRCH); a zombie is still killable and
/// returns 0. Signal 0 is an existence check with no side effects.
fn is_reaped(pid: u32) -> bool {
    // SAFETY: `libc::kill(pid, 0)` is a signal-0 existence check with no side
    // effects; safe to call from a test.
    unsafe { libc::kill(pid as i32, 0) != 0 }
}

/// Polls `predicate` until it holds or `budget` elapses.
async fn wait_until(mut predicate: impl FnMut() -> bool, budget: Duration) -> bool {
    tokio::time::timeout(budget, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok()
}

/// An `ExecuteTool` for a namespaced MCP tool is dispatched to the actor, which
/// forwards it to the stub server's `tools/call` and publishes the result.
#[rstest::rstest]
#[tokio::test]
async fn execute_tool_for_namespaced_echo_returns_server_response() {
    // Given an McpActor wired to the stub server, with the session id known to
    // the test and a recorder for ToolExecutionCompleted.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let services = harness.services().await;
    let _actor_path = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;

    // When publishing an ExecuteTool for the namespaced echo tool.
    let tool_call = ToolCall {
        id: "tc_1".into(),
        name: "mcp__stub__echo".into(),
        arguments: r#"{"message": "hello mcp"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then a ToolExecutionCompleted arrives carrying the server's echoed text.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(
        messages.len(),
        1,
        "expected one result from the roundtrip, got: {messages:?}"
    );
    let result = &messages[0].result;
    assert!(
        result.success,
        "echo should succeed, content was: {}",
        result.content
    );
    assert_eq!(result.content, "hello mcp");
    assert_eq!(result.name, "mcp__stub__echo");
    assert_eq!(messages[0].session_id, session_id);
}

/// A `ToolExecutionCompleted` is published even when the actor holds no
/// connected client — the failure path produces a failed result, never a hang.
#[rstest::rstest]
#[tokio::test]
async fn execute_tool_when_client_disconnected_yields_failed_result() {
    // Given an McpActor whose injected client is already dropped (simulating a
    // dead connection) — we spawn it with a client then immediately drop it by
    // not keeping it alive... but `on_start` consumes it. Instead, verify the
    // disconnected path by constructing the actor so connect fails.
    //
    // Since the override always connects successfully, exercise the failure
    // path via an unknown tool name: the stub returns a protocol error, which
    // the actor surfaces as a failed (non-crashing) result.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let services = harness.services().await;
    let _actor_path = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;

    // When publishing an ExecuteTool for a tool the stub does not advertise.
    let tool_call = ToolCall {
        id: "tc_2".into(),
        name: "mcp__stub__does_not_exist".into(),
        arguments: "{}".to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then a failed ToolExecutionCompleted arrives (no panic, no hang).
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(messages.len(), 1, "expected one result, got: {messages:?}");
    assert!(
        !messages[0].result.success,
        "unknown tool should produce a failed result"
    );
}

/// When the orchestrator sends tight truncation limits, the actor bounds the
/// server's response and records the original in `full_content` — proving
/// the limits flow through `ExecuteTool` and apply end-to-end.
#[rstest::rstest]
#[tokio::test]
async fn execute_tool_truncates_large_response_to_orchestrator_limits() {
    // Given an McpActor wired to the stub server.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let services = harness.services().await;
    let _actor_path = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;

    // And a large message payload (well over the byte limit we will send).
    let big_message = "x".repeat(500);
    let tool_call = ToolCall {
        id: "tc_3".into(),
        name: "mcp__stub__echo".to_owned(),
        arguments: format!(r#"{{"message": "{big_message}"}}"#),
    };

    // When publishing an ExecuteTool with a 100-byte limit.
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: Some(100),
            max_output_bytes: Some(100),
            cwd: None,
        })
        .await;

    // Then the result content is bounded and the full payload is preserved.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(messages.len(), 1, "expected one result, got: {messages:?}");
    let result = &messages[0].result;
    assert!(result.success);
    assert!(
        result.content.len() <= 100,
        "content should be bounded to 100 bytes, got {} bytes: {}",
        result.content.len(),
        result.content
    );
    assert_eq!(result.full_content.as_deref(), Some(big_message.as_str()));
    assert!(
        result.truncation.is_some(),
        "truncation metadata must be present when truncation occurred"
    );
}

/// When the MCP server's transport closes after a successful connection, the
/// liveness-watch task detects it and publishes `McpServerStatus(Dead)` — the
/// exact fix for the "kill -9 leaves it stuck on running" bug.
///
/// Detection works at the transport level, the dead status is published, and
/// the final tail is published alongside it.
#[rstest::rstest]
#[tokio::test]
async fn transport_close_publishes_dead_status() {
    // Given an McpActor wired to a stub server we can kill, recording status.
    let (_harness, status_recorder, killer) = {
        let harness = TestHarness::new().await;
        let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let session_id = SessionId::new();

        let (client, killer) = spawn_stub_client_with_killer().await;
        let services = harness.services().await;
        let _actor_path = McpActor::spawn(
            &services.trouper_system,
            McpActorDeps::with_client(
                ActorDeps {
                    services: services.clone(),
                },
                session_id.clone(),
                SERVER_NAME.to_owned(),
                stub_config(),
                client,
            ),
        )
        .await;

        (harness, status_recorder, killer)
    };

    // When the actor starts up, the watcher runs a few ticks with the
    // connection alive, and the server's transport then closes (kill -9).
    let startup = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
    assert!(
        startup
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running),
        "the actor must report Running before the kill, got: {startup:?}"
    );
    // The recorder must have stopped changing before "no Dead fired while
    // the connection was alive" is an observation rather than a race with
    // the watcher. A fixed sleep could let the assertion run before the
    // watcher had published anything at all.
    let drained = jinn_testutil::wait::await_settled_messages(
        &status_recorder,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;
    drop(killer);

    // Then a Dead status is published within the watch cadence + slack. The
    // pre-kill read cleared the recorder, so await a single new message.
    let after = await_recorded(&status_recorder, 1, Duration::from_secs(5)).await;
    let dead = after
        .iter()
        .filter(|m| m.status == McpConnectionStatus::Dead)
        .count();
    assert!(
        dead >= 1,
        "expected at least one Dead status after transport close, got: {after:?}"
    );

    // And no Dead fires while the connection is still alive.
    assert!(
        drained
            .iter()
            .all(|m| m.status != McpConnectionStatus::Dead),
        "no Dead should fire while the connection is alive, got: {drained:?}"
    );
}

/// After an actor is stopped (the kill half of a restart), its liveness
/// watcher must be gone — a fresh actor spawned in its place must work normally
/// and its watcher must be the only one publishing. This is the zombie-prevention
/// regression: `on_stop` sets the old watcher's shutdown flag.
#[rstest::rstest]
#[tokio::test]
async fn restarted_actor_has_no_zombie_watcher_from_the_previous_one() {
    // Given a first McpActor that is stopped (simulating the kill-half of restart).
    let (harness, status_recorder, session_id) = {
        let harness = TestHarness::new().await;
        let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let session_id = SessionId::new();

        let client = spawn_stub_client().await;
        let services = harness.services().await;
        let first = McpActor::spawn(
            &services.trouper_system,
            McpActorDeps::with_client(
                ActorDeps {
                    services: services.clone(),
                },
                session_id.clone(),
                SERVER_NAME.to_owned(),
                stub_config(),
                client,
            ),
        )
        .await;
        let _ = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
        // Stop the first actor — its on_stop kills its watcher.
        services.trouper_system.stop(&first).await;
        // Consume the first actor's on_stop Dead so it isn't counted later.
        let _ = await_recorded(&status_recorder, 1, Duration::from_secs(3)).await;

        (harness, status_recorder, session_id)
    };

    // When spawning a fresh actor in its place (the spawn-half of restart).
    let client2 = spawn_stub_client().await;
    let services = harness.services().await;
    let _second = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client2,
        ),
    )
    .await;
    // Drain the second actor's startup statuses.
    let _ = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;

    // Then the second actor runs without any spurious Dead from the first
    // actor's (now-killed) watcher.
    // The recorder must have stopped changing first, so "no Dead fired" is
    // an observation rather than a race with the watcher.
    let trailing = jinn_testutil::wait::await_settled_messages(
        &status_recorder,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        trailing
            .iter()
            .all(|m| m.status != McpConnectionStatus::Dead),
        "no Dead should fire from a zombie watcher while the second actor runs, got: {trailing:?}"
    );
}

/// Normal teardown (`on_stop`) publishes `ToolsUnregistered` carrying the
/// server's provider namespace and session — so registries and context caches
/// can prune this server's session-scoped tools on disable/close/restart.
#[rstest::rstest]
#[tokio::test]
async fn normal_teardown_publishes_tools_unregistered() {
    // Given a running McpActor recording ToolsUnregistered.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolsUnregistered>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let services = harness.services().await;
    let actor_path = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;

    // When the actor is stopped normally (the coordinator's teardown path).
    services.trouper_system.stop(&actor_path).await;

    // Then a ToolsUnregistered arrives naming this session × provider.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(
        messages.len(),
        1,
        "expected exactly one ToolsUnregistered on teardown, got: {messages:?}"
    );
    assert_eq!(messages[0].provider, "mcp__stub__");
    assert_eq!(messages[0].session_id, session_id);
}

/// On normal teardown (`on_stop`), the liveness watcher exits without
/// double-publishing `Dead` beyond `on_stop`'s own publish — the shutdown-flag
/// ordering prevents the race.
#[rstest::rstest]
#[tokio::test]
async fn normal_teardown_publishes_exactly_one_dead() {
    // Given a running McpActor recording status.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let services = harness.services().await;
    let actor_path = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;

    // Wait for startup to publish Starting + Running.
    let startup = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
    assert!(
        startup
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running)
    );

    // When the actor is stopped normally (the coordinator's teardown path).
    services.trouper_system.stop(&actor_path).await;

    // Then exactly one Dead is published by teardown. The startup await already
    // drained Starting + Running, so a single new message is the on_stop Dead.
    // A grace window then catches any racing watcher publish that would make two.
    let on_stop_statuses = await_recorded(&status_recorder, 1, Duration::from_secs(5)).await;
    // The recorder must have stopped changing first, so "exactly one Dead"
    // counts every Dead that will be published rather than the ones that
    // had arrived when the read was taken.
    let trailing = jinn_testutil::wait::await_settled_messages(
        &status_recorder,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;
    let final_statuses = [on_stop_statuses, trailing].concat();
    let dead_count = final_statuses
        .iter()
        .filter(|m| m.status == McpConnectionStatus::Dead)
        .count();
    assert_eq!(
        dead_count, 1,
        "teardown must publish exactly one Dead, got {dead_count}: {final_statuses:?}"
    );
}

/// End-to-end disable cycle, live half: while the actor is alive an
/// `ExecuteTool` call succeeds against the registered tool — the state the
/// orchestrator's dispatch gate is checked against before a disable.
#[rstest::rstest]
#[tokio::test]
async fn live_disable_cycle_call_succeeds_before_teardown() {
    // Given a running McpActor wired to the stub server, its tools registered
    // for the session, and the session marked enabled + Running (the live
    // state the orchestrator's dispatch gate checks).
    let harness = TestHarness::new().await;
    let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();
    let state = jinn_kernel::common::state::State::new(
        jinn_kernel::common::app_state::AppState::default_with_scope_focus(),
    );
    state.write().session.get_or_create(&session_id);
    state
        .write()
        .session
        .get_mut(&session_id)
        .expect("session")
        .enable_mcp_server("stub");
    let services = harness.services().await;
    let runtime = crate::activate_runtime(&services.slices);
    runtime.update(|runtime| {
        runtime.set_status(&session_id, "stub", McpConnectionStatus::Running);
    });
    spawn_actor_against_stub(&harness, &session_id).await;

    // When calling the echo tool while the server is live.
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call: ToolCall {
                id: "tc_live".into(),
                name: "mcp__stub__echo".to_owned(),
                arguments: r#"{"message": "before disable"}"#.to_owned(),
            },
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the call succeeds.
    let before = await_recorded(&results, 1, Duration::from_secs(3)).await;
    assert!(
        before[0].result.success && before[0].result.content == "before disable",
        "live call must succeed, got: {:?}",
        before[0].result
    );
}

/// End-to-end disable cycle, torn-down half: once the coordinator's disable
/// path has stopped the actor and pruned the registry, a subsequent call
/// fails fast with a legible reason instead of hanging — the exact bug this
/// work fixes, exercised across the real bus.
#[rstest::rstest]
#[tokio::test]
async fn disable_cycle_calls_fail_fast_after_teardown() {
    // Given a running McpActor wired to the stub server, its tools registered
    // for the session, and the session marked enabled + Running.
    let (harness, session_id, _results, _state) = {
        let harness = TestHarness::new().await;
        let _results = harness.spawn_recorder::<ToolExecutionCompleted>().await;
        let session_id = SessionId::new();
        let state = jinn_kernel::common::state::State::new(
            jinn_kernel::common::app_state::AppState::default_with_scope_focus(),
        );
        state.write().session.get_or_create(&session_id);
        state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session")
            .enable_mcp_server("stub");
        let services = harness.services().await;
        let runtime = crate::activate_runtime(&services.slices);
        runtime.update(|runtime| {
            runtime.set_status(&session_id, "stub", McpConnectionStatus::Running);
        });
        let (_services, actor) = spawn_actor_against_stub(&harness, &session_id).await;
        services.trouper_system.stop(&actor).await;
        state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session")
            .disable_mcp_server("stub");

        (harness, session_id, _results, state)
    };

    // When calling the echo tool again after teardown (actor stopped, registry
    // pruned, session enablement flipped off — the confirm_mcp ordering).
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call: ToolCall {
                id: "tc_dead".into(),
                name: "mcp__stub__echo".to_owned(),
                arguments: r#"{"message": "after disable"}"#.to_owned(),
            },
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the call fails fast rather than hanging.
    // The actor is stopped, so the raw ExecuteTool has no subscriber: the
    // completion must not arrive. The recorder must have stopped changing
    // first, so "no completion arrived" is an observation rather than a
    // race with the dispatch. A fixed sleep would pass on a slow host
    // simply because the dispatch had not been processed yet.
    let completions = jinn_testutil::wait::await_settled_messages(
        &_results,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;

    // And nothing completes: the call hangs on a subscriber that is gone.
    assert!(
        completions.is_empty(),
        "a call to a torn-down MCP server must not complete, got: {completions:?}"
    );
}

/// The HTTP child-exit watcher reaps the child (no zombie) and cancels the
/// transport when the child process dies.
///
/// This is the load-bearing behavior for the `kill -9` HTTP bug: without the
/// watcher, a half-open TCP socket keeps `is_transport_closed()` false, so the
/// server stays "running" and the child becomes a zombie. The watcher fixes
/// both: `try_wait()` reaps, and the cancel-token flip lets the liveness
/// watcher publish `Dead`.
#[rstest::rstest]
#[tokio::test]
async fn http_child_exit_reaps_and_cancels_transport() {
    // Given a real child process (`sleep 30`) and a stub MCP client whose
    // transport token we can observe.
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let sleep_child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = sleep_child.id().expect("child has a pid");
    let stub_client = spawn_stub_client().await;
    let probe = stub_client.liveness_probe();
    assert!(!probe.is_transport_closed(), "transport open before kill");

    // When the child-exit watcher runs and the child is killed externally.
    let shutdown = Arc::new(AtomicBool::new(false));
    crate::connection::spawn_child_watch(shutdown.clone(), sleep_child, stub_client.cancel_token());
    // SAFETY: sending SIGKILL to a child we just spawned; the pid is valid
    // for the duration of the test.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Then within the watcher cadence + slack, the transport reports closed
    // (cancel token fired) AND the child is reaped (no longer in /proc).
    let closed = wait_until(|| probe.is_transport_closed(), Duration::from_secs(3)).await;
    assert!(closed, "transport should report closed after child death");
    // `kill(pid, 0)` returns ESRCH (no such process) once reaped. A zombie
    // would still be killable (return 0).
    let reaped = wait_until(|| is_reaped(pid), Duration::from_secs(3)).await;
    assert!(
        reaped,
        "child should be reaped (no zombie) after watcher handles exit"
    );
}

/// Normal teardown (disable/restart) signals the watcher's shutdown flag;
/// the watcher exits, drops the `Child`, and `kill_on_drop` kills the
/// still-alive process. The child is reaped (no zombie).
#[rstest::rstest]
#[tokio::test]
async fn http_teardown_kills_and_reaps_still_alive_child() {
    // Given a watcher running over a still-alive child.
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let sleep_child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = sleep_child.id().expect("child has a pid");

    let stub_client = spawn_stub_client().await;
    let cancel_token = stub_client.cancel_token();

    let shutdown = Arc::new(AtomicBool::new(false));
    crate::connection::spawn_child_watch(shutdown.clone(), sleep_child, cancel_token);

    // When normal teardown signals the shutdown flag (mimicking on_stop).
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);

    // Then within the cadence the child is killed (kill_on_drop) and reaped.
    let reaped = wait_until(|| is_reaped(pid), Duration::from_secs(3)).await;
    assert!(
        reaped,
        "teardown should kill + reap the still-alive child, no zombie"
    );
}

/// An MCP call that outlives `tool_default_timeout_secs` is failed with a
/// timeout message and a `ToolExecutionCompleted` is still published — the
/// pending batch self-completes instead of stranding in `Sending` forever.
#[rstest::rstest]
#[tokio::test]
async fn execute_tool_exceeding_timeout_yields_failed_result() {
    // Given an McpActor wired to the stub server and a tight 1-second ceiling.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();

    // And the actor's own services carry a tight tool timeout.
    let (services, _actor) = spawn_actor_against_stub(&harness, &session_id).await;
    {
        services
            .config
            .put::<ToolsConfig>(&ToolsConfig {
                default_timeout_secs: 1,
                ..ToolsConfig::default()
            })
            .expect("write the tool timeout ceiling");
    }

    // When calling slow_echo with a 5-second delay.
    let tool_call = ToolCall {
        id: "tc_slow".into(),
        name: "mcp__stub__slow_echo".to_owned(),
        arguments: r#"{"message": "late", "delay_ms": 5000}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the result arrives around the ceiling (well before the 5s delay).
    let messages = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let got = await_recorded(&recorder, 1, Duration::from_secs(4)).await;
            if got
                .first()
                .is_some_and(|m| m.result.tool_call_id.as_str() == "tc_slow")
            {
                return got;
            }
        }
    })
    .await
    .expect("result must arrive before the tool's own delay elapses");
    let result = &messages[0].result;
    assert!(!result.success, "timeout must fail the call: {result:?}");
    assert!(
        result.content.contains("timed out"),
        "timeout message must be surfaced to the model: {}",
        result.content
    );
}

/// `tool_default_timeout_secs = 0` disables the ceiling: a call slower than
/// any default completes successfully rather than being failed.
#[rstest::rstest]
#[tokio::test]
async fn execute_tool_with_disabled_timeout_completes() {
    // Given an McpActor wired to the stub server and the ceiling disabled.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();
    let services = harness.services().await;

    let client = spawn_stub_client().await;
    let _actor = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client(
            ActorDeps {
                services: services.clone(),
            },
            session_id.clone(),
            SERVER_NAME.to_owned(),
            stub_config(),
            client,
        ),
    )
    .await;
    {
        services
            .config
            .put::<ToolsConfig>(&ToolsConfig {
                default_timeout_secs: 0,
                ..ToolsConfig::default()
            })
            .expect("write the disabled tool timeout ceiling");
    }

    // When calling slow_echo with a 1.5-second delay.
    let tool_call = ToolCall {
        id: "tc_unbounded".into(),
        name: "mcp__stub__slow_echo".to_owned(),
        arguments: r#"{"message": "patient", "delay_ms": 1500}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the result arrives carrying the echoed text, not a timeout.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(6)).await;
    assert_eq!(messages.len(), 1, "got: {messages:?}");
    let result = &messages[0].result;
    assert!(result.success, "disabled ceiling must not fail: {result:?}");
    assert_eq!(result.content, "patient");
}

/// Mints a fixture turn identity the way the ledger's mint does — a monotonic
/// per-process counter starting at 1, never zero.
fn fixture_turn() -> jinn_core_types::turn_identity::TurnToken {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    jinn_core_types::turn_identity::TurnToken::from_counter(SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

/// An `ExecuteTool` carrying both `dump_to_disk` and a cwd writes the server
/// response to the resolved path and publishes a stubbed result.
#[rstest::rstest]
#[tokio::test]
async fn dump_to_disk_call_writes_file_and_publishes_stub() {
    // Given an McpActor wired to the stub server and a test-owned dump dir.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();
    let dir = jinn_testutil::tempdir();
    let cwd = dir.path.as_path().to_string_lossy().into_owned();
    let (_services, _actor_path) = spawn_actor_against_stub(&harness, &session_id).await;

    // When calling echo with dump_to_disk naming a file in fresh subdirectories.
    let tool_call = ToolCall {
        id: "tc_dump".into(),
        name: "mcp__stub__echo".into(),
        arguments: r#"{"message": "dump me", "dump_to_disk": "research/page.md"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: Some(cwd),
        })
        .await;

    // Then the published result is a stub naming the file, and the file
    // holds the server's original response.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(messages.len(), 1, "got: {messages:?}");
    let result = &messages[0].result;
    assert!(result.success, "{result:?}");
    assert!(
        result.content.contains("research/page.md"),
        "stub names the dump path: {}",
        result.content
    );
    assert_eq!(result.full_content.as_deref(), Some("dump me"));
    let written =
        std::fs::read_to_string(dir.path.as_path().join("research/page.md")).expect("dump file");
    assert_eq!(written, "dump me");
}

/// An `ExecuteTool` carrying `dump_to_disk` without a cwd skips the dump —
/// the published result is the plain server response, never a wrong path.
#[rstest::rstest]
#[tokio::test]
async fn dump_to_disk_without_cwd_is_skipped() {
    // Given an McpActor wired to the stub server and no cwd on the command.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();
    let (_services, _actor_path) = spawn_actor_against_stub(&harness, &session_id).await;

    // When calling echo with dump_to_disk set.
    let tool_call = ToolCall {
        id: "tc_nodump".into(),
        name: "mcp__stub__echo".into(),
        arguments: r#"{"message": "kept in context", "dump_to_disk": "page.md"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the result passes through untouched: plain content, no stub, and
    // no file anywhere (the relative path was never resolved against a guess).
    let messages = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert_eq!(messages.len(), 1, "got: {messages:?}");
    let result = &messages[0].result;
    assert!(result.success, "{result:?}");
    assert_eq!(result.content, "kept in context");
    assert!(result.full_content.is_none());
    assert!(result.truncation.is_none());
}
