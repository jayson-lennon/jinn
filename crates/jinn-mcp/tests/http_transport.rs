//! HTTP transport integration tests: failure and latency behavior.
//!
//! These verify the load-bearing properties of the managed-HTTP path that are
//! independent of any particular MCP server implementation:
//!
//! - **Bad command**: a nonexistent binary fails to spawn, surfacing the
//!   error rather than hanging.
//! - **Process exits before connect**: a short-lived process (e.g.
//!   `false`) exits immediately; `connect_with_retry` returns `Err` carrying
//!   the captured output, not a hang.
//! - **Slow boot, no false Dead**: a long-running process that never
//!   speaks HTTP keeps the connect loop retrying indefinitely — the bounded
//!   timeout below proves the loop is *still trying* rather than having
//!   flipped to `Dead`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::time::Duration;

// Integration test binaries never run the crate's `#[cfg(test)]` lib hook, so
// install the ring rustls provider here directly: reqwest is built with
// `rustls-no-provider` (see the workspace `Cargo.toml`) and panics with
// "No provider set" at `reqwest::Client` construction otherwise.
// `install_default` errors on the second call; the result is deliberately ignored.
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

use jinn_mcp::client::McpClient;

/// A nonexistent command fails at spawn time with a clear error.
#[rstest::rstest]
#[tokio::test]
async fn connect_http_rejects_nonexistent_command() {
    // Given a program that does not exist.
    // When attempting to spawn the HTTP server.
    let result = McpClient::connect_http(
        "definitely-not-a-real-binary-xyzzy",
        &[],
        "127.0.0.1",
        &[],
        Vec::new(),
    );

    // Then spawn fails fast with an error (no hang).
    assert!(result.is_err(), "nonexistent command should fail to spawn");
}

/// A child that exits immediately causes connect to return `Err` with the
/// captured output, never hanging.
#[rstest::rstest]
#[tokio::test]
async fn connect_with_retry_returns_err_when_child_exits_immediately() {
    // Given a half-open HTTP server whose process exits at once.
    let (program, args) = jinn_testutil::platform::exit_command(1);
    let half = McpClient::connect_http(program, &args, "127.0.0.1", &[], Vec::new())
        .expect("spawn should succeed even though the process exits immediately");

    // When retrying the connect.
    // `false` exits within milliseconds, so a generous bound still completes.
    let result =
        tokio::time::timeout(Duration::from_secs(5), McpClient::connect_with_retry(half)).await;

    // Then the retry loop returns `Err` (child exited), not a timeout/hang.
    assert!(
        result.is_ok(),
        "connect should return Err promptly on child exit, not hang"
    );
    let inner = result.expect("bounded by timeout above");
    assert!(
        inner.is_err(),
        "connect should be an Err when the child exits before HTTP is up"
    );
}

/// A slow-booting process (one that never speaks HTTP) keeps the retry loop
/// trying indefinitely — proving no false `Dead` from a wall-clock timeout.
#[rstest::rstest]
#[tokio::test]
async fn connect_with_retry_keeps_trying_when_server_never_listens() {
    // Given a half-open HTTP server whose process stays alive but never speaks
    // HTTP.
    let (program, args) = jinn_testutil::platform::idle_command(60);
    let half = McpClient::connect_http(program, &args, "127.0.0.1", &[], Vec::new())
        .expect("spawn should succeed");

    // When retrying the connect, bounded by a short timeout.
    let result = tokio::time::timeout(
        Duration::from_millis(800),
        McpClient::connect_with_retry(half),
    )
    .await;

    // Then the connect is still looping (timeout fires) — the process is alive
    // and HTTP is down, so the loop retries forever without flipping to Dead.
    assert!(
        result.is_err(),
        "connect should still be retrying after 800ms against a silent-but-alive server"
    );
}
