//! Actor-level tests for the `restart_mcp_server` tool (ask pattern).
//!
//! The tool `ask`s the coordinator directly (request/reply). These tests
//! exercise:
//!   - the coordinator's `restart_one` outcome (real actor, unrunnable command
//!     → `ConnectFailed`; unknown server → `UnknownServer`),
//!   - the tool's `execute()` failure paths (no coordinator ref; unknown
//!     server routed through the real ask).
//!
//! A success-path test (`restart_one` → `Ok`) requires a runnable MCP server
//! subprocess, which does not exist in this crate; the success path is
//! structurally identical to the existing dispatch-roundtrip tests that use
//! the in-process stub. See `mcp_actor/dispatch_roundtrip_tests.rs` for that
//! coverage.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::PathBuf;

use crate::coordinator::{McpCoordinatorActor, McpCoordinatorActorDeps};
use jinn_domain::common::actor_deps::ActorDeps;
use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::bus::test_harness::TestHarness;
use jinn_domain::common::root_supervisor::RootSupervisor;
use jinn_domain::common::state::State;
use jinn_domain::feat::mcp::McpServerConfig;
use jinn_domain::feat::preferences_actor::UserPreferences;
use jinn_domain::feat::tools_actor::restart_mcp::execute;
use jinn_domain::feat::tools_actor::tool_types::{ToolCall, ToolContext};
use jinn_domain::protocol::SessionId;
use jinn_slices::{RestartError, RestartMcpServer};
use kameo::actor::Spawn;

/// A configured MCP server whose command will never spawn successfully, so the
/// spawned `McpActor` fails to connect and goes Dead.
fn unrunnable_server() -> McpServerConfig {
    McpServerConfig {
        command: Some("/this/command/does/not/exist".to_owned()),
        args: vec![],
        ..Default::default()
    }
}

/// Spawns a real coordinator seeded with the given configured servers.
async fn spawn_coordinator(
    harness: &TestHarness,
    servers: &[(&str, McpServerConfig)],
) -> (
    kameo::actor::ActorRef<McpCoordinatorActor>,
    jinn_domain::Services,
    jinn_domain::common::state::State,
) {
    let services = harness.services().await;
    services
        .user_preferences_storage
        .save(&UserPreferences {
            mcp_server: servers
                .iter()
                .map(|(name, config)| ((*name).to_owned(), config.clone()))
                .collect(),
            ..UserPreferences::default()
        })
        .expect("seed prefs");
    let root = RootSupervisor::spawn_root().await;
    let state = State::new(AppState::default());
    let actor = McpCoordinatorActor::spawn(McpCoordinatorActorDeps {
        deps: ActorDeps {
            services: services.clone(),
        },
        root,
        state: state.clone(),
        cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
    });
    actor.wait_for_startup().await;
    (actor, services, state)
}

/// Builds a tool call targeting the given server.
fn call(server: &str) -> ToolCall {
    ToolCall {
        id: "tc_1".to_owned(),
        name: "restart_mcp_server".to_owned(),
        arguments: format!("{{\"server\": \"{server}\"}}"),
    }
}

/// Builds a ToolContext wired to the given coordinator ref + state seeded with
/// `excalimate`.
fn ctx_with_coordinator(
    coordinator: kameo::actor::ActorRef<McpCoordinatorActor>,
    session_id: SessionId,
) -> ToolContext {
    let config = McpServerConfig {
        command: Some(String::new()),
        args: vec![],
        ..Default::default()
    };
    let mut app = AppState::default();
    app.frontend.preferences = UserPreferences {
        mcp_server: [("excalimate".to_owned(), config)].into_iter().collect(),
        ..Default::default()
    };
    let state = State::new(app);

    ToolContext {
        cwd: PathBuf::from("/tmp"),
        command_policy:
            jinn_domain::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
        timeout: None,
        state: Some(state),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: None,
        mcp_coordinator: Some(crate::mcp_coordinator_handle(coordinator)),
        interactive_term: None,
        task_spawns: None,
        session_store: None,
    }
}

// ---------------------------------------------------------------------------
// Coordinator-level: restart_one outcomes (the real ask)
// ---------------------------------------------------------------------------

/// An unrunnable command → the new actor fails to connect → `ConnectFailed`.
#[rstest::rstest]
#[tokio::test]
async fn restart_one_returns_connect_failed_for_unrunnable_command() {
    // Given a coordinator with an unrunnable server enabled for a session.
    let harness = TestHarness::new().await;
    let (coordinator, _services, _state) =
        spawn_coordinator(&harness, &[("unrunnable", unrunnable_server())]).await;
    let session_id = SessionId::new();

    // When asking the coordinator to restart that server.
    let reply = coordinator
        .ask(RestartMcpServer {
            session_id,
            server: "unrunnable".to_owned(),
        })
        .await;

    // Then the reply is a domain-level ConnectFailed (wrapped in SendError).
    assert!(
        matches!(
            reply,
            Err(kameo::error::SendError::HandlerError(
                RestartError::ConnectFailed
            ))
        ),
        "unrunnable command should yield ConnectFailed; got: {reply:?}"
    );
}

/// A server that isn't in the config → `UnknownServer`.
#[rstest::rstest]
#[tokio::test]
async fn restart_one_returns_unknown_server_for_unconfigured_server() {
    // Given a coordinator with one configured server.
    let harness = TestHarness::new().await;
    let (coordinator, _services, _state) =
        spawn_coordinator(&harness, &[("unrunnable", unrunnable_server())]).await;
    let session_id = SessionId::new();

    // When asking to restart a different (unconfigured) server.
    let reply = coordinator
        .ask(RestartMcpServer {
            session_id,
            server: "ghost".to_owned(),
        })
        .await;

    // Then the reply is a domain-level UnknownServer.
    assert!(
        matches!(
            reply,
            Err(kameo::error::SendError::HandlerError(
                RestartError::UnknownServer
            ))
        ),
        "unconfigured server should yield UnknownServer; got: {reply:?}"
    );
}

// ---------------------------------------------------------------------------
// Tool-level: execute() failure paths
// ---------------------------------------------------------------------------

/// No coordinator ref (e.g. test seed without one) → immediate failure.
#[rstest::rstest]
#[tokio::test]
async fn execute_fails_when_coordinator_ref_is_none() {
    // Given a context with no coordinator ref.
    let session_id = SessionId::new();
    let ctx = ToolContext {
        cwd: PathBuf::from("/tmp"),
        command_policy:
            jinn_domain::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
        timeout: None,
        state: Some(State::new(AppState::default())),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: None,
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: None,
        session_store: None,
    };

    // When executing.
    let result = execute(call("excalimate"), ctx).await;

    // Then the tool fails fast mentioning the coordinator.
    assert!(!result.success, "missing coordinator should fail");
    assert!(
        result.content.contains("coordinator"),
        "error should mention the coordinator; got: {}",
        result.content
    );
}

/// An unknown server routed through the real ask → failure naming the server.
#[rstest::rstest]
#[tokio::test]
async fn execute_returns_failure_for_unknown_server_via_ask() {
    // Given a coordinator with `excalimate` configured and a context wired to it.
    let harness = TestHarness::new().await;
    let (coordinator, _services, _state) =
        spawn_coordinator(&harness, &[("unrunnable", unrunnable_server())]).await;
    let ctx = ctx_with_coordinator(coordinator, SessionId::new());

    // When executing with an unconfigured server name.
    let result = execute(call("ghost"), ctx).await;

    // Then the tool fails, naming the unknown server.
    assert!(!result.success, "unknown server should fail");
    assert!(
        result.content.contains("ghost"),
        "error should name the unknown server; got: {}",
        result.content
    );
}

/// A namespaced tool name routes to the real ask (and fails UnknownServer if
/// the server isn't configured) — proves namespace-stripping reaches the ask.
#[rstest::rstest]
#[tokio::test]
async fn execute_strips_namespace_and_routes_to_ask() {
    // Given a coordinator with no `stub` server configured.
    let harness = TestHarness::new().await;
    let (coordinator, _services, _state) = spawn_coordinator(&harness, &[]).await;
    let ctx = ctx_with_coordinator(coordinator, SessionId::new());

    // When executing with a namespaced tool name.
    let result = execute(
        ToolCall {
            id: "tc_1".to_owned(),
            name: "restart_mcp_server".to_owned(),
            arguments: "{\"server\": \"mcp__stub__echo\"}".to_owned(),
        },
        ctx,
    )
    .await;

    // Then the tool fails with UnknownServer for `stub` (namespace stripped).
    assert!(!result.success, "should reach the ask and fail");
    assert!(
        result.content.contains("stub"),
        "error should name the stripped server; got: {}",
        result.content
    );
}
