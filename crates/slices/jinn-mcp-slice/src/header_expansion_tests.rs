//! Header-expansion integration tests for the MCP actor connect path.
//!
//! Proves the observable contract at the actor boundary:
//!
//! - A `RemoteHttp` server whose headers reference an unresolvable `${VAR}`
//!   lands in the failed state (`Dead` published) instead of connecting —
//!   legible failure, no half-authenticated attempts.
//! - The `Stdio` arm ignores configured `headers` entirely: a stdio server
//!   with a bogus header var still connects (via injected client, which
//!   bypasses transport; the expansion function itself is covered in
//!   `feat::mcp::tests` — here we prove `connect_for_transport`'s stdio arm
//!   never calls it).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::panic,
    reason = "test assertions"
)]

use std::collections::BTreeMap;
use std::time::Duration;

use kameo::actor::Spawn;

use crate::connection::{McpActor, McpActorDeps};
use jinn_domain::common::bus::test_harness::{TestHarness, await_recorded};
use jinn_domain::feat::mcp::{McpServerConfig, TransportKind};
use jinn_domain::protocol::SessionId;
use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};

/// A RemoteHttp server whose header references an unknown variable publishes
/// Dead (never Running) — the failure is surfaced through the standard
/// lifecycle plumbing with no new status machinery.
#[rstest::rstest]
#[tokio::test]
async fn remote_http_server_with_unresolved_header_variable_lands_dead() {
    // Given an McpActor for a RemoteHttp server whose Authorization header
    // references a variable that was never seeded into the key store.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_owned(),
        "Bearer ${JINN_TEST_NEVER_SET_VAR}".to_owned(),
    );
    let server = McpServerConfig {
        transport: TransportKind::RemoteHttp,
        url: Some("http://127.0.0.1:1/mcp".to_owned()),
        headers,
        ..Default::default()
    };

    // When spawning the actor (no injected client — real connect path).
    let services = harness.services().await;
    let actor = McpActor::spawn(McpActorDeps::new(
        jinn_domain::common::actor_deps::ActorDeps {
            services: services.clone(),
        },
        session_id,
        "gated-remote".to_owned(),
        server,
    ));
    actor.wait_for_startup().await;

    // Then startup completes in the Dead state.
    let statuses = await_recorded(&status_recorder, 1, Duration::from_secs(3)).await;
    assert!(
        statuses
            .iter()
            .any(|m| m.status == McpConnectionStatus::Dead),
        "expected a Dead status for unresolved header var, got: {statuses:?}"
    );
    // And Running was never published.
    assert!(
        !statuses
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running),
        "actor must not reach Running when headers cannot expand, got: {statuses:?}"
    );
}

/// A RemoteHttp server whose headers all resolve from the key store proceeds
/// past expansion into the normal (retrying) connect path — its status is
/// Starting only, never the immediate Dead that a failed expansion produces
/// against an equally unreachable URL.
///
/// Note: `on_start` blocks inside the no-timeout retry loop by design, so this
/// test never awaits `wait_for_startup()`; it samples published statuses on a
/// fixed schedule instead.
#[rstest::rstest]
#[tokio::test]
async fn remote_http_server_with_resolvable_headers_enters_retry_loop() {
    // Given an McpActor for a RemoteHttp server whose header variable IS
    // seeded into the key store, pointed at an unreachable port.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    const VAR: &str = "JINN_TEST_MCP_ACTOR_HEADER_VAR";
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_owned(), format!("Bearer ${{{VAR}}}"));

    let server = McpServerConfig {
        transport: TransportKind::RemoteHttp,
        url: Some("http://127.0.0.1:1/mcp".to_owned()),
        headers,
        ..Default::default()
    };

    // Seed exactly what EnvInitActor would seed at startup.
    let services = harness.services().await;
    services
        .api_keys
        .insert(VAR.to_owned(), "seeded".to_owned());

    // When spawning the actor on the unreachable URL. `wait_for_startup` is
    // intentionally NOT awaited: on_start parks in the retry loop forever by
    // design. Spawning alone is enough for Starting to publish.
    let _actor = McpActor::spawn(McpActorDeps::new(
        jinn_domain::common::actor_deps::ActorDeps {
            services: services.clone(),
        },
        session_id,
        "ok-remote".to_owned(),
        server,
    ));

    // Then within a window where an expansion failure would long since have
    // published Dead, resolvable headers show only Starting — no Dead.
    let statuses = await_recorded(&status_recorder, 1, Duration::from_secs(3)).await;
    assert!(
        statuses
            .iter()
            .any(|m| m.status == McpConnectionStatus::Starting),
        "expected at least a Starting status, got: {statuses:?}"
    );
    // And Dead never fires within that same window (the loop is retrying).
    let settled = await_recorded(&status_recorder, 0, Duration::from_millis(700)).await;
    assert!(
        settled
            .iter()
            .all(|m| m.status != McpConnectionStatus::Dead),
        "resolvable headers must not produce Dead while retrying, got: {settled:?}"
    );
}

/// A `Stdio` server's configured `headers` are ignored entirely: the failure
/// reason for an unspawnable binary is the *spawn* failure — never any
/// header-expansion error naming an unseeded variable.
#[rstest::rstest]
#[tokio::test]
async fn stdio_arm_ignores_configured_headers_entirely() {
    // Given services WITHOUT the variable that the config's headers reference,
    // and a stdio command that cannot exist.
    let services = jinn_domain::Services::new_fake().await;
    let mut headers = std::collections::BTreeMap::new();
    headers.insert(
        "Authorization".to_owned(),
        "Bearer ${JINN_TEST_STDIO_NEVER_SET}".to_owned(),
    );
    let server = McpServerConfig {
        transport: TransportKind::Stdio,
        command: Some("definitely-not-a-real-binary-xyzzy".to_owned()),
        args: vec![],
        headers,
        ..Default::default()
    };

    // When connecting through the transport dispatcher.
    let result = crate::connection::connect_for_transport(&services, &server).await;

    // Then the attempt fails because the binary cannot spawn.
    let Err(err) = result else {
        panic!("nonexistent stdio command must fail")
    };
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("failed to spawn"),
        "failure should be the spawn failure, got: {rendered}"
    );
    // And NOT because headers were consulted — no variable language anywhere.
    assert!(
        !rendered.contains("JINN_TEST_STDIO_NEVER_SET"),
        "stdio arm must not expand or validate headers, got: {rendered}"
    );
}

/// A Stdio server whose headers reference an unresolvable variable still
/// reaches `Running` when a pre-connected stub client is injected — proof the
/// stdio lifecycle never consults headers at all (the injected client skips
/// `connect_for_transport`; pairing it with bogus headers isolates any leak of
/// header handling into the stdio path).
#[rstest::rstest]
#[tokio::test]
async fn stdio_server_with_bogus_header_variable_still_connects() {
    // Given an McpActor for a Stdio server whose headers reference an
    // unresolvable variable — with a pre-connected stub client injected so
    // `on_start` takes the same path but never spawns a child process.
    let harness = TestHarness::new().await;
    let status_recorder = harness.spawn_recorder::<McpServerStatus>().await;
    let session_id = SessionId::new();

    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_owned(),
        "Bearer ${JINN_TEST_NEVER_SET_VAR}".to_owned(),
    );
    let server = McpServerConfig {
        transport: TransportKind::Stdio,
        command: Some(String::new()),
        headers,
        ..Default::default()
    };

    // When spawning the actor with an injected stub connection.
    let services = harness.services().await;
    let client = jinn_mcp::server_testkit::spawn_stub_client().await;
    let actor = McpActor::spawn(McpActorDeps::with_client(
        jinn_domain::common::actor_deps::ActorDeps { services },
        session_id.clone(),
        "stdio-ignores-headers".to_owned(),
        server,
        client,
    ));
    actor.wait_for_startup().await;

    // Then startup reaches Running — headers were never consulted.
    let statuses = await_recorded(&status_recorder, 1, Duration::from_secs(3)).await;
    assert!(
        statuses
            .iter()
            .any(|m| m.status == McpConnectionStatus::Running),
        "stdio server should reach Running regardless of headers, got: {statuses:?}"
    );
}
