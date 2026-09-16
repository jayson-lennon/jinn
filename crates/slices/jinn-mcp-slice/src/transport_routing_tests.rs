//! Transport-routing tests for `connect_for_transport`.
//!
//! The routing `match` is trivial; what matters is the *observable behavior*
//! each arm produces:
//!
//! - `Stdio` spawns a child (covered by existing stdio integration tests).
//! - `LocalHttp` spawns a child with a jinn-allocated port then polls (covered by
//!   the HTTP integration test in `jinn-mcp`).
//! - `RemoteHttp` connects to a URL with no child; when the URL is unreachable
//!   it loops on a backoff rather than erroring on the first refusal — the
//!   load-bearing no-timeout property.
//!
//! This module proves the `RemoteHttp` retry-without-timeout behavior directly,
//! since it requires no stub process: an unreachable localhost port exercises
//! the same code path as a slow-booting server.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    reason = "test assertions"
)]

use std::time::Duration;

use crate::connection::connect_for_transport;
use jinn_domain::feat::mcp::TransportKind;

/// `RemoteHttp` to an unreachable URL keeps retrying instead of failing fast.
///
/// This is the no-wall-clock-timeout guarantee (AC5/AC7): a slow or down
/// endpoint never produces an early `Err`. We verify by racing the connect
/// against a short timeout — if the connect returned `Err` immediately, the
/// timeout arm would not fire.
#[rstest::rstest]
#[tokio::test]
async fn remote_http_to_unreachable_url_loops_instead_of_failing() {
    // Given a RemoteHttp config pointing at a port nothing is listening on.
    // Use a port in the dynamic range that's very likely free.
    let config = jinn_domain::feat::mcp::McpServerConfig {
        command: None,
        args: vec![],
        transport: TransportKind::RemoteHttp,
        url: Some("http://127.0.0.1:1/mcp".to_owned()),
        headers: std::collections::BTreeMap::default(),
        auto_enable: false,
    };

    // When attempting to connect, bounded by a short timeout.
    let services = jinn_domain::Services::new_fake().await;
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        connect_for_transport(&services, &config),
    )
    .await;

    // Then the connect is still looping (timeout fires), proving it did not
    // fail fast on the first refused connection.
    assert!(
        result.is_err(),
        "RemoteHttp connect should still be retrying after 500ms, not return"
    );
}
