//! A reusable stub MCP server for downstream integration tests.
//!
//! Exposed only behind the `server-testkit` feature so production builds never
//! pull in rmcp's `server` implementation. Provides [`spawn_stub_client`]: an
//! in-process `EchoServer` wired to one end of an in-memory duplex pipe, with a
//! real [`McpClient`] connected to the other end — ready to `list_tools` /
//! `call_tool` against, no child process required.
//!
//! The server advertises one tool, `echo`, which returns its `message` argument
//! verbatim as text content. Tests that need to exercise the full client →
//! `tools/list` → `tools/call` lifecycle (e.g. `jinn-domain`'s actor dispatch
//! roundtrip) call [`spawn_stub_client`] to obtain a connected client.

#![allow(
    clippy::missing_docs_in_private_items,
    clippy::missing_panics_doc,
    clippy::expect_used,
    reason = "test-only stub server for downstream integration tests"
)]

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestMethod, CallToolRequestParams, CallToolResponse, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, ServiceExt};
use rmcp::transport::async_rw::AsyncRwTransport;
use serde_json::json;
use tokio::io::DuplexStream;

use crate::client::McpClient;

/// A minimal MCP server that advertises one `echo` tool.
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

    /// Like `echo`, but sleeps `delay_ms` first — lets downstream tests
    /// exercise timeout ceilings without spawning a hung server.
    fn slow_echo_tool() -> Tool {
        Tool::new(
            "slow_echo",
            "Sleeps `delay_ms`, then echoes `message` back as text.",
            Arc::new(
                json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "delay_ms": { "type": "integer" }
                    },
                    "required": ["message", "delay_ms"],
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

    // Trait-required signature is async though this fake never awaits.
    // Trait-required async signature; this fake never awaits.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        async {}.await;
        Ok(ListToolsResult::with_all_items(vec![
            Self::echo_tool(),
            Self::slow_echo_tool(),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "echo" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(no message)");
                let result = rmcp::model::CallToolResult::success(vec![ContentBlock::text(
                    message.to_owned(),
                )]);
                Ok(result.into())
            }
            "slow_echo" => {
                let args = request.arguments.unwrap_or_default();
                let message = args
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(no message)");
                let delay_ms = args
                    .get("delay_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let result = rmcp::model::CallToolResult::success(vec![ContentBlock::text(
                    message.to_owned(),
                )]);
                Ok(result.into())
            }
            _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
        }
    }
}

/// Wires an in-process `EchoServer` to one end of a duplex pipe and returns the
/// other end for the client to connect through.
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

/// Spawns the in-process stub `EchoServer` and connects a real [`McpClient`] to
/// it over an in-memory duplex pipe.
///
/// The returned client is fully connected (handshake complete) and ready to
/// `list_tools` / `call_tool`. No child process is spawned. The server task is
/// detached and dies when the transport closes (on client drop or shutdown).
///
/// # Panics
///
/// Panics if the MCP handshake fails (a wiring error, not a test condition).
#[must_use]
pub async fn spawn_stub_client() -> McpClient {
    let client_io = spawn_stub_server();
    let (client_read, client_write) = tokio::io::split(client_io);
    McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect to stub server")
}

/// Like [`spawn_stub_client`], but also returns a [`ServerKiller`] that aborts
/// the server task on drop, forcing the client's transport to report closed
/// (the signal the liveness-watch task polls).
///
/// Use in tests that need to simulate post-connect server death without
/// killing the owning `McpClient` directly.
///
/// # Panics
///
/// Panics if the MCP handshake fails (a wiring error, not a test condition).
#[must_use]
pub async fn spawn_stub_client_with_killer() -> (McpClient, ServerKiller) {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let join = tokio::spawn(async move {
        let Ok(running) = EchoServer.serve(server_io).await else {
            return;
        };
        let _ = running.waiting().await;
    });
    let (client_read, client_write) = tokio::io::split(client_io);
    let client = McpClient::connect_with_transport(AsyncRwTransport::<
        RoleClient,
        tokio::io::ReadHalf<DuplexStream>,
        tokio::io::WriteHalf<DuplexStream>,
    >::new(client_read, client_write))
    .await
    .expect("client must connect to stub server");
    (client, ServerKiller(join))
}

/// Aborts the paired stub server task on drop, closing the transport from the
/// server side so the client's `is_transport_closed()` flips true.
///
/// Returned by [`spawn_stub_client_with_killer`].
pub struct ServerKiller(pub tokio::task::JoinHandle<()>);

impl Drop for ServerKiller {
    fn drop(&mut self) {
        self.0.abort();
    }
}
