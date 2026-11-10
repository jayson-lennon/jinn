//! End-to-end integration test: spawn an in-process stub MCP server over an
//! in-memory duplex pipe and exercise the full client lifecycle through
//! `McpClient` — connect → list_tools → call_tool → shutdown.
//!
//! This validates the real `McpClient` against a real (if minimal) MCP server
//! implementation, exercising the JSON-RPC handshake, tool advertisement, and
//! tool dispatch paths without depending on an external process.
//!
//! The stub server advertises one tool, `echo`, which returns its `message`
//! argument verbatim as text content.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::map_err_ignore,
    clippy::missing_docs_in_private_items,
    reason = "test-only stub fixtures and harness"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

use jinn_mcp::client::{McpClient, ServerCommand};
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestMethod, CallToolRequestParams, CallToolResponse, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, ServiceExt};
use rmcp::transport::async_rw::AsyncRwTransport;
use serde_json::{Value, json};
use tokio::io::DuplexStream;

/// A minimal MCP server that advertises one `echo` tool.
///
/// All trait methods have defaults except `get_info`, `list_tools`, and
/// `call_tool`, which we override.
struct EchoServer;

impl EchoServer {
    fn echo_tool() -> Tool {
        Tool::new(
            "echo",
            "Echoes the `message` argument back as text.",
            Arc::new(
                json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    },
                    "required": ["message"],
                })
                .as_object()
                .expect("valid object")
                .clone(),
            ),
        )
    }
}

impl ServerHandler for EchoServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::default())
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "rmcp service contract requires async fn; the body never awaits"
    )]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![Self::echo_tool()]))
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "rmcp service contract requires async fn; the body never awaits"
    )]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name.as_ref() != "echo" {
            return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
        }
        let message = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("(no message)");
        let result =
            rmcp::model::CallToolResult::success(vec![ContentBlock::text(message.to_owned())]);
        Ok(result.into())
    }
}

/// Wires an in-process `EchoServer` to one end of a duplex pipe and returns
/// the other end for the client to connect through.
fn spawn_stub_server() -> DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    // `serve` returns once the handshake completes; `waiting()` keeps the
    // server handling requests until the transport closes. Detach the task.
    tokio::spawn(async move {
        let Ok(running) = EchoServer.serve(server_io).await else {
            return;
        };
        let _ = running.waiting().await;
    });
    client_io
}

#[rstest::rstest]
#[tokio::test]
async fn connect_lists_tools_advertising_echo() {
    // Given a stub MCP server reachable over an in-memory duplex pipe.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect to stub server");

    // When listing the server's tools.
    let tools = client.list_tools().await.expect("tools/list must succeed");

    // Then `tools/list` advertises exactly the `echo` tool.
    let tool_name = tools
        .first()
        .map(|t| t.name.as_ref().to_owned())
        .unwrap_or_default();
    assert_eq!(tools.len(), 1, "stub advertises exactly one tool");
    assert_eq!(tool_name, "echo");

    // And the client shuts down without error (no panic, no hang).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn call_tool_echo_returns_the_message_as_text() {
    // Given a client connected to the stub MCP server.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect to stub server");

    // When calling `echo` with a message argument.
    let result = client
        .call_tool(
            "echo",
            Some(
                json!({ "message": "hello mcp" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await
        .expect("tools/call must succeed");

    // Then the result echoes the message back as text.
    assert!(
        !result.is_error.unwrap_or(false),
        "echo should succeed, not report an error"
    );
    let text = result
        .content
        .into_iter()
        .find_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text),
            _ => None,
        })
        .unwrap_or_default();
    assert_eq!(text, "hello mcp");

    // And the client shuts down without error (no panic, no hang).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn calling_an_unknown_tool_returns_a_protocol_error() {
    // Given a connected client with the echo server.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect");

    // When calling a tool the server does not advertise.
    let result = client.call_tool("does_not_exist", None).await;

    // Then the client surfaces a protocol-level error (Err, not a success).
    assert!(
        result.is_err(),
        "unknown tool must produce a client error, not a silent success"
    );
}

#[rstest::rstest]
#[test]
fn server_command_carries_program_and_args() {
    // Given an excalimate-style stdio command.
    let cmd = ServerCommand {
        program: "npx".to_owned(),
        args: vec!["@excalimate/mcp-server".to_owned(), "--stdio".to_owned()],
        env: Vec::new(),
    };

    // When reading the command back (sanity check for the owned value the
    // actor carries).
    // Then the fields round-trip.
    assert_eq!(cmd.program, "npx");
    assert_eq!(cmd.args, vec!["@excalimate/mcp-server", "--stdio"]);
}

#[rstest::rstest]
#[tokio::test]
async fn is_transport_closed_returns_false_on_a_live_client() {
    // Given a stub MCP server reachable over an in-memory duplex pipe.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect");

    // When polling the transport-level liveness signal on a live connection.
    let closed = client.is_transport_closed();

    // Then it reports the connection as open.
    assert!(
        !closed,
        "a freshly connected client's transport must not be closed"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn is_transport_closed_returns_true_after_shutdown() {
    // Given a connected client.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect");

    // When shutting the connection down.
    client.shutdown().await;

    // Then the transport-level liveness signal reports it as closed.
    assert!(
        client.is_transport_closed(),
        "after shutdown the transport must report closed"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn liveness_probe_reflects_transport_state_independently_of_client() {
    // Given a connected client and a standalone liveness probe cloned from it.
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect");
    let probe = client.liveness_probe();

    // When polling the standalone probe.
    // Then the probe reports the connection open while the client is alive.
    assert!(!probe.is_transport_closed());

    // And after the client shuts down, the standalone probe also reports closed,
    // proving the probe polls shared transport state without holding the client.
    client.shutdown().await;
    assert!(probe.is_transport_closed());
}

#[rstest::rstest]
#[tokio::test]
async fn killer_drops_and_flips_transport_closed() {
    // Given a connected client with a killer handle, transport open.
    let (client, killer) = jinn_mcp::server_testkit::spawn_stub_client_with_killer().await;
    assert!(!client.is_transport_closed());

    // When the killer is dropped (server task aborted).
    drop(killer);

    // Then within a short window the client's transport reports closed.
    let mut closed = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if client.is_transport_closed() {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "dropping the killer must flip is_transport_closed()"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn liveness_probe_flips_when_killer_drops_across_task_boundary() {
    // Given a connected client + killer, mirroring how McpActor owns the client
    // and spawns a watcher task holding a cloned probe.
    let (client, killer) = jinn_mcp::server_testkit::spawn_stub_client_with_killer().await;
    let probe = client.liveness_probe();

    // A watcher task (like McpActor's) polls the probe.
    let probe_for_task = probe.clone();
    let closed_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = closed_flag.clone();
    let watch = tokio::spawn(async move {
        // The poll runs until the close is seen; the outer timeout bounds the
        // test, but the loop itself carries its own deadline so a probe that
        // never flips cannot spin forever if the timeout is removed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if probe_for_task.is_transport_closed() {
                flag_clone.store(true, Ordering::SeqCst);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the probe never reported the transport closed"
            );
        }
    });

    // When the killer is dropped (simulating kill -9).
    drop(killer);

    // Then the watcher task detects the close within a short window.
    let _ = tokio::time::timeout(Duration::from_secs(3), watch).await;
    assert!(
        closed_flag.load(Ordering::SeqCst),
        "probe polled from a spawned task must detect the transport close"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn killer_variant_stays_alive_until_dropped() {
    // Given a connected client from the killer variant.
    let (client, killer) = jinn_mcp::server_testkit::spawn_stub_client_with_killer().await;

    // When holding the killer and polling the client over a short window.
    // Then it stays alive (transport open) for a while without dropping the killer.
    //
    // The window is a real duration, so the clock has to move for the check
    // to mean anything — a signal cannot substitute here. The loop asserts
    // the transport is still open on every tick, so a close at any point in
    // the window fails the test rather than being missed.
    let window = Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + window;
    let mut ticks = 0;
    while tokio::time::Instant::now() < deadline {
        ticks += 1;
        assert!(
            !client.is_transport_closed(),
            "client closed unexpectedly at tick {ticks} before killer dropped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // And can still call a tool.
    drop(killer);
}
