//! Connect-time stderr surfacing: a child that dies at startup must leave
//! its captured stderr in the failure the actor publishes, so the MCP
//! picker can show why it died.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::os::unix::fs::PermissionsExt as _;

use marked_path::MarkedPathAccess as _;

use crate::connection::acquire_client;
use jinn_kernel::Services;
use jinn_preferences_config::schemas::mcp::McpServerConfig;
use rstest::rstest;

/// The dying-child fixture: an executable script that writes one stderr
/// line and exits. Built in a test-owned temp dir so the test never depends
/// on a hand-planted file in the shared `/tmp`.
fn dying_child_server() -> (jinn_testutil::JinnTempDir, String) {
    let dir = jinn_testutil::tempdir_with_prefix("jinn-test-dying-child-");
    let script = dir.path.as_path().join("panic-server.sh");
    std::fs::write(
        &script,
        "#!/bin/bash\necho \"boom: this server dies on startup\" >&2\nexit 42\n",
    )
    .expect("fixture script write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("fixture script chmod");
    let command = script.to_string_lossy().into_owned();
    (dir, command)
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_that_dies_at_startup_leaves_its_stderr_in_the_connect_failure() {
    // Given a stdio server whose command writes one stderr line and exits.
    let services = Services::new_fake().await;
    let (_fixture_dir, command) = dying_child_server();
    let server = McpServerConfig {
        singleton: true,
        command: Some(command),
        args: vec![],
        ..Default::default()
    };

    // When acquiring a client (connect + tools/list).
    let result = acquire_client(&services, "probe", &server, None, None).await;

    // Then the connect failure carries the child's stderr tail.
    let Err((client, tail)) = result else {
        panic!("the child exited at startup; connect must fail");
    };
    assert!(client.is_none(), "nothing to shut down at connect time");
    assert!(
        tail.contains("boom: this server dies on startup"),
        "stderr tail must carry the child's output, got: {tail:?}"
    );
}
