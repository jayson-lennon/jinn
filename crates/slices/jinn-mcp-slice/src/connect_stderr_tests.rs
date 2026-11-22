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

use crate::connection::acquire_client;
use jinn_kernel::Services;
use jinn_preferences_config::schemas::mcp::McpServerConfig;
use rstest::rstest;

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_that_dies_at_startup_leaves_its_stderr_in_the_connect_failure() {
    // Given a stdio server whose command writes one stderr line and exits.
    let services = Services::new_fake().await;
    // The fixture binary is written per-run into the test's own tmpdir —
    // a hardcoded /tmp path evaporates with tmp reaper sweeps and leaves
    // the test reporting a spawn failure (empty tail) instead of the
    // stderr path it exists to exercise.
    let fixture =
        std::env::temp_dir().join(format!("jinn-mcp-stderr-fixture-{}", std::process::id()));
    std::fs::write(
        &fixture,
        "#!/bin/sh\necho 'boom: this server dies on startup' >&2\nexit 1\n",
    )
    .expect("write stderr fixture");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stderr fixture");
    }
    let server = McpServerConfig {
        singleton: true,
        command: Some(fixture.to_string_lossy().into_owned()),
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
