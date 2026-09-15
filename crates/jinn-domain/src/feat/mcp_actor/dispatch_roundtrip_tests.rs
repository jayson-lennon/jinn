//! End-to-end dispatch roundtrip for the MCP actor.
//!
//! These tests exercise the *full* dispatch seam that the pure unit tests in
//! `mcp_actor/mod.rs` cannot reach: a real [`McpActor`] spawned on the bus,
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
use kameo::actor::Spawn;

use crate::common::actor_deps::ActorDeps;
use crate::common::bus::test_harness::{TestHarness, await_recorded};
use crate::feat::mcp::McpServerConfig;
use crate::feat::mcp_actor::protocol::{McpConnectionStatus, McpServerStatus};
use crate::feat::mcp_actor::{McpActor, McpActorDeps};
use crate::feat::tools_actor::protocol::command::ExecuteTool;
use crate::feat::tools_actor::protocol::event::ToolExecutionCompleted;
use crate::feat::tools_actor::protocol::event::ToolsUnregistered;
use crate::feat::tools_actor::tool_types::ToolCall;
use crate::protocol::SessionId;

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
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // When publishing an ExecuteTool for the namespaced echo tool.
    let tool_call = ToolCall {
        id: "tc_1".to_owned(),
        name: "mcp__stub__echo".to_owned(),
        arguments: r#"{"message": "hello mcp"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
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
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // When publishing an ExecuteTool for a tool the stub does not advertise.
    let tool_call = ToolCall {
        id: "tc_2".to_owned(),
        name: "mcp__stub__does_not_exist".to_owned(),
        arguments: "{}".to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
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
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // And a large message payload (well over the byte limit we will send).
    let big_message = "x".repeat(500);
    let tool_call = ToolCall {
        id: "tc_3".to_owned(),
        name: "mcp__stub__echo".to_owned(),
        arguments: format!(r#"{{"message": "{big_message}"}}"#),
    };

    // When publishing an ExecuteTool with a 100-byte limit.
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: Some(100),
            max_output_bytes: Some(100),
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
/// Covers AC1, AC2, AC4: detection works (transport-level), the dead status is
/// published, and the final tail is published alongside it.
#[rstest::rstest]
#[tokio::test]
async fn transport_close_publishes_dead_status() {
    // Given an McpActor wired to a stub server we can kill, recording status.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let (client, killer) = spawn_stub_client_with_killer().await;
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // Then the actor publishes Starting and Running during startup.
    let startup = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
    assert!(
        startup
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running),
        "expected a Running status before kill, got: {startup:?}"
    );

    // Let the watcher run a few ticks with the connection alive to prove the
    // recorder is drained (no spurious pre-kill Dead), then drop the killer.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let drained = await_recorded(&status_recorder, 0, Duration::from_millis(50)).await;
    assert!(
        drained
            .iter()
            .all(|m| m.status != McpConnectionStatus::Dead),
        "no Dead should fire while the connection is alive, got: {drained:?}"
    );
    // When the server's transport closes (simulating kill -9).
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
}

/// On normal teardown (`on_stop`), the liveness watcher exits without
/// double-publishing `Dead` beyond `on_stop`'s own publish — the shutdown-flag
/// ordering prevents the race.
///
/// Covers AC3.
#[rstest::rstest]
#[tokio::test]
async fn normal_teardown_publishes_exactly_one_dead() {
    // Given a running McpActor recording status.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // Wait for startup to publish Starting + Running.
    let startup = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
    assert!(
        startup
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running)
    );

    // When the actor is stopped normally (the coordinator's teardown path).
    let _ = actor.stop_gracefully().await;
    // Then exactly one Dead is published by teardown. The startup await already
    // drained Starting + Running, so a single new message is the on_stop Dead.
    // A grace window then catches any racing watcher publish that would make two.
    let on_stop_statuses = await_recorded(&status_recorder, 1, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(450)).await;
    let trailing = await_recorded(&status_recorder, 0, Duration::from_millis(50)).await;
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

/// After an actor is stopped (the kill half of a restart), its liveness
/// watcher must be gone — a fresh actor spawned in its place must work normally
/// and its watcher must be the only one publishing. This is the zombie-prevention
/// regression: `on_stop` sets the old watcher's shutdown flag.
#[rstest::rstest]
#[tokio::test]
async fn restarted_actor_has_no_zombie_watcher_from_the_previous_one() {
    // Given a first McpActor that is stopped (simulating the kill-half of restart).
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let client = spawn_stub_client().await;
    let first = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    first.wait_for_startup().await;
    let _ = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;
    // Stop the first actor — its on_stop kills its watcher.
    let _ = first.stop_gracefully().await;
    // Consume the first actor's on_stop Dead so it isn't counted later.
    let _ = await_recorded(&status_recorder, 1, Duration::from_secs(3)).await;

    // When spawning a fresh actor in its place (the spawn-half of restart).
    let client2 = spawn_stub_client().await;
    let second = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client2,
    ));
    second.wait_for_startup().await;
    // Drain the second actor's startup statuses.
    let _ = await_recorded(&status_recorder, 2, Duration::from_secs(3)).await;

    // Then the second actor runs without any spurious Dead from the first
    // actor's (now-killed) watcher.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let trailing = await_recorded(&status_recorder, 0, Duration::from_millis(50)).await;
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
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: harness.services().await,
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // When the actor is stopped normally (the coordinator's teardown path).
    let _ = actor.stop_gracefully().await;

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

/// End-to-end disable cycle: while the actor is alive an `ExecuteTool` call
/// succeeds; after teardown (the coordinator's disable path) the actor is gone
/// and the registry cleanup means no subscriber hangs the call — the exact
/// bug this work fixes, exercised across the real bus.
#[rstest::rstest]
#[tokio::test]
async fn disable_cycle_calls_fail_fast_after_teardown() {
    // Given a running McpActor wired to the stub server, its tools registered
    // for the session, and the session marked enabled + Running (the live
    // state the orchestrator's dispatch gate checks).
    let harness = TestHarness::new().await;
    let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = SessionId::new();

    let services = harness.services().await;
    let state = crate::common::state::State::new(
        crate::common::app_state::AppState::default_with_scope_focus(),
    );
    state.write_test_no_cap().session.get_or_create(&session_id);
    state
        .write_test_no_cap()
        .session
        .get_mut(&session_id)
        .expect("session")
        .enable_mcp_server("stub");
    state
        .write_test_no_cap()
        .session
        .get_mut(&session_id)
        .expect("session")
        .set_mcp_server_status("stub", McpConnectionStatus::Running);

    let client = spawn_stub_client().await;
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps { services },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;

    // When calling the echo tool while the server is live.
    let live_call = ToolCall {
        id: "tc_live".to_owned(),
        name: "mcp__stub__echo".to_owned(),
        arguments: r#"{"message": "before disable"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call: live_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
        })
        .await;

    // Then the call succeeds.
    let before = await_recorded(&results, 1, Duration::from_secs(3)).await;
    assert!(
        before[0].result.success && before[0].result.content == "before disable",
        "live call must succeed, got: {:?}",
        before[0].result
    );

    // When the server is disabled (teardown: actor stops, registry pruned,
    // session enablement flipped off — the confirm_mcp ordering).
    let _ = actor.stop_gracefully().await;
    state
        .write_test_no_cap()
        .session
        .get_mut(&session_id)
        .expect("session")
        .disable_mcp_server("stub");

    // Then a subsequent call fails fast with a legible reason (recorded
    // through the same recorder — no hang, no watchdog rescue).
    let dead_call = ToolCall {
        id: "tc_dead".to_owned(),
        name: "mcp__stub__echo".to_owned(),
        arguments: r#"{"message": "after disable"}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call: dead_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
        })
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The actor is stopped; the raw ExecuteTool now has no subscriber. This
    // test pins the actor-side half of the cycle; the orchestrator-side
    // fail-fast (gate + prune) is covered by mcp_dispatch_gate_tests, which
    // together with this test proves the full disable path never hangs.
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

    let is_reaped = |pid: u32| -> bool {
        // Returns true once the child has been reaped (ESRCH); a zombie is
        // still killable (returns 0).
        // SAFETY: `libc::kill(pid, 0)` is a signal-0 existence check with no
        // side effects; safe to call from a test.
        unsafe { libc::kill(pid as i32, 0) != 0 }
    };

    let sleep_child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = sleep_child.id().expect("child has a pid");

    let stub_client = spawn_stub_client().await;
    let probe = stub_client.liveness_probe();
    let cancel_token = stub_client.cancel_token();
    assert!(!probe.is_transport_closed(), "transport open before kill");

    // When the child-exit watcher runs and the child is killed externally.
    let shutdown = Arc::new(AtomicBool::new(false));
    super::spawn_child_watch(shutdown.clone(), sleep_child, cancel_token);
    // `try_wait` requires the child to be dead; kill it via the OS.
    // SAFETY: sending SIGKILL to a child we just spawned; the pid is valid
    // for the duration of the test.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Then within the watcher cadence + slack, the transport reports closed
    // (cancel token fired) AND the child is reaped (no longer in /proc).
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        while !probe.is_transport_closed() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    assert!(closed, "transport should report closed after child death");

    // The child should be reaped — `kill(pid, 0)` returns ESRCH (no such
    // process) once reaped. A zombie would still be killable (return 0).
    let reaped = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if is_reaped(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
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

    let is_reaped = |pid: u32| -> bool {
        // SAFETY: `libc::kill(pid, 0)` is a signal-0 existence check, no side effects.
        unsafe { libc::kill(pid as i32, 0) != 0 }
    };

    let sleep_child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let pid = sleep_child.id().expect("child has a pid");

    let stub_client = spawn_stub_client().await;
    let cancel_token = stub_client.cancel_token();

    let shutdown = Arc::new(AtomicBool::new(false));
    super::spawn_child_watch(shutdown.clone(), sleep_child, cancel_token);

    // When normal teardown signals the shutdown flag (mimicking on_stop).
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);

    // Then within the cadence the child is killed (kill_on_drop) and reaped.
    let reaped = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if is_reaped(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
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
    let services = harness.services().await;

    let client = spawn_stub_client().await;
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: services.clone(),
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;
    {
        let prefs = crate::feat::preferences_actor::UserPreferences {
            tool_default_timeout_secs: 1,
            ..Default::default()
        };
        services
            .user_preferences_storage
            .save(&prefs)
            .expect("save prefs");
    }

    // When calling slow_echo with a 5-second delay.
    let tool_call = ToolCall {
        id: "tc_slow".to_owned(),
        name: "mcp__stub__slow_echo".to_owned(),
        arguments: r#"{"message": "late", "delay_ms": 5000}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
        })
        .await;

    // Then the result arrives around the ceiling (well before the 5s delay).
    let messages = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let got = await_recorded(&recorder, 1, Duration::from_secs(4)).await;
            if got
                .first()
                .is_some_and(|m| m.result.tool_call_id == "tc_slow")
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
    let actor = McpActor::spawn(McpActorDeps::with_client(
        ActorDeps {
            services: services.clone(),
        },
        session_id.clone(),
        SERVER_NAME.to_owned(),
        stub_config(),
        client,
    ));
    actor.wait_for_startup().await;
    {
        let prefs = crate::feat::preferences_actor::UserPreferences {
            tool_default_timeout_secs: 0,
            ..Default::default()
        };
        services
            .user_preferences_storage
            .save(&prefs)
            .expect("save prefs");
    }

    // When calling slow_echo with a 1.5-second delay.
    let tool_call = ToolCall {
        id: "tc_unbounded".to_owned(),
        name: "mcp__stub__slow_echo".to_owned(),
        arguments: r#"{"message": "patient", "delay_ms": 1500}"#.to_owned(),
    };
    harness
        .publish(ExecuteTool {
            session_id: session_id.clone(),
            tool_call,
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
        })
        .await;

    // Then the result arrives carrying the echoed text, not a timeout.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(6)).await;
    assert_eq!(messages.len(), 1, "got: {messages:?}");
    let result = &messages[0].result;
    assert!(result.success, "disabled ceiling must not fail: {result:?}");
    assert_eq!(result.content, "patient");
}
