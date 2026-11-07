//! Dispatch roundtrip for the *singleton* connection actor.
//!
//! The singleton variant of the per-session actor: it registers its tools
//! globally (`RegisterTools { session_id: None }`), answers `ExecuteTool`
//! from any session (not just its spawning session), and — because the
//! registration is global — does not publish `ToolsUnregistered` on stop.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::time::Duration;

use jinn_mcp::server_testkit::spawn_stub_client;

use crate::connection::{McpActor, McpActorDeps};
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::bus::HarnessServices;
use jinn_preferences_config::schemas::mcp::McpServerConfig;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_tools_msg::ExecuteTool;
use jinn_tools_msg::RegisterTools;
use jinn_tools_msg::ToolExecutionCompleted;
use jinn_tools_msg::ToolsUnregistered;

/// The server name — becomes the tool namespace (`mcp__singleton__echo`).
const SERVER_NAME: &str = "singleton";

fn singleton_config() -> McpServerConfig {
    McpServerConfig {
        command: Some(String::new()),
        singleton: true,
        ..Default::default()
    }
}

/// Spawns an McpActor wired to the stub server in singleton mode.
async fn spawn_singleton_actor(
    harness: &TestHarness,
    owning_session: &SessionId,
) -> (jinn_kernel::Services, trouper::actor::ActorPath) {
    let services = harness.services().await;
    let client = spawn_stub_client().await;
    let actor = McpActor::spawn(
        &services.trouper_system,
        McpActorDeps::with_client_as_singleton(
            ActorDeps {
                services: services.clone(),
            },
            owning_session.clone(),
            SERVER_NAME.to_owned(),
            singleton_config(),
            client,
        ),
    )
    .await;
    (services, actor)
}

fn echo_call() -> ToolCall {
    ToolCall {
        id: "tc_1".into(),
        name: "mcp__singleton__echo".to_owned(),
        arguments: r#"{"message":"hi"}"#.to_owned(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_actor_registers_tools_globally() {
    // Given a singleton-mode actor spawned by session A, with a recorder for
    // RegisterTools.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<RegisterTools>().await;
    let (_services, _path) = spawn_singleton_actor(&harness, &SessionId::new()).await;

    // When the actor comes up (allow the startup path to publish).
    let registrations = await_recorded(&recorder, 1, Duration::from_secs(3)).await;

    // Then the registration carries NO session id — it is global.
    assert!(
        !registrations.is_empty(),
        "singleton actor must register its tools"
    );
    assert!(
        registrations.iter().all(|r| r.session_id.is_none()),
        "singleton tools must register globally (session_id: None), got: {registrations:?}"
    );
    // And the definitions use the shared namespace.
    assert!(
        registrations.iter().any(|r| r
            .definitions
            .iter()
            .any(|d| d.name == "mcp__singleton__echo")),
        "expected the namespaced echo tool, got: {registrations:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_actor_serves_a_session_that_never_spawned_it() {
    // Given a singleton actor spawned by session A.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let (_services, _path) = spawn_singleton_actor(&harness, &SessionId::new()).await;
    // The actor must have subscribed to ExecuteTool before the dispatch
    // below, or the call would find no subscriber and the test would
    // prove nothing. A fixed sleep could let the dispatch happen first.
    jinn_testutil::wait::await_inbox_drained(harness.system(), &_path, Duration::from_secs(10))
        .await;

    // When session B — a different session — dispatches an ExecuteTool.
    harness
        .publish(ExecuteTool {
            turn: fixture_turn(),
            session_id: SessionId::new(),
            tool_call: echo_call(),
            dispatched_at: jiff::Timestamp::now(),
            max_output_lines: None,
            max_output_bytes: None,
            cwd: None,
        })
        .await;

    // Then the actor answers it (the session filter must not drop the call).
    let events = await_recorded(&recorder, 1, Duration::from_secs(3)).await;
    assert!(
        !events.is_empty(),
        "a singleton actor must serve any enabled session, not only its spawner"
    );
    assert!(
        events[0].result.success,
        "expected a successful echo, got: {:?}",
        events[0].result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn singleton_actor_stop_does_not_publish_tools_unregistered() {
    // Given a singleton actor spawned by session A.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ToolsUnregistered>().await;
    let (services, path) = spawn_singleton_actor(&harness, &SessionId::new()).await;
    // The actor must have subscribed before it is stopped, or the stop
    // would find nothing to tear down and the test would prove nothing.
    jinn_testutil::wait::await_inbox_drained(harness.system(), &path, Duration::from_secs(10))
        .await;

    // When the actor stops (a restart does exactly this).
    services.trouper_system.stop(&path).await;

    // Then no ToolsUnregistered event is published: the registration was
    // global, and a restart re-registers over the same provider key —
    // publishing the event would strand the global registrations.
    // The recorder must have stopped changing before "nothing was
    // published" is an observation rather than a race with a publish that
    // is still in flight.
    let events = jinn_testutil::wait::await_settled_messages(
        &recorder,
        Duration::from_millis(300),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        events.is_empty(),
        "singleton teardown must not publish ToolsUnregistered; got {events:?}"
    );
}

/// Mints a fixture turn identity the way the ledger's mint does — a monotonic
/// per-process counter starting at 1, never zero.
fn fixture_turn() -> jinn_core_types::turn_identity::TurnToken {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    jinn_core_types::turn_identity::TurnToken::from_counter(SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}
