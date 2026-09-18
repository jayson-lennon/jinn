//! MCP client connection to a single server.
//!
//! This module is transport-level: [`McpClient`] owns one `rmcp` client
//! connection to one MCP server. It knows nothing about the actor
//! system or `AppState`. The `jinn-mcp-slice` drives it.
//!
//! # Lifecycle
//!
//! Three transports are supported, selected by the server's `TransportKind`:
//!
//! - **stdio** ([`McpClient::connect`]): spawns the server as a child process
//!   and handshakes over stdin/stdout. `kill_on_drop` ensures a dropped
//!   connection always terminates the child.
//! - **HTTP, managed** ([`McpClient::connect_http`] + [`McpClient::connect_with_retry`]):
//!   spawns the server with a jinn-allocated port (injected via `<port>`/`<ip>`
//!   tokens), then polls the HTTP endpoint on a backoff until the handshake
//!   succeeds or the child exits. No wall-clock timeout — a slow-booting server
//!   stays connecting for as long as it needs. Both stdout and stderr are drained
//!   into the log buffer (stdout is available here, unlike stdio mode).
//! - **HTTP, remote** ([`McpClient::connect_remote`]): connects to an externally
//!   managed URL with no child process.
//!
//! Both HTTP transports accept resolved `(name, value)` header pairs that are
//! applied as default headers on every request. jinn-domain expands `${VAR}`
//! tokens before calling in; this module never reads the environment.
//!
//! [`McpClient::shutdown`] closes the transport and waits (with a timeout) for
//! the child to exit if one was spawned.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use error_stack::{Report, ResultExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use tokio::io::AsyncBufReadExt;
use wherror::Error;

use crate::transport::{expand_tokens, parse_host, pick_free_port};

/// A server process failed to start or communicate.
#[derive(Debug, Error)]
#[error(debug)]
pub struct McpClientError;

/// The command line used to spawn an MCP server.
///
/// A thin owned value so callers (the actor system) can carry it cheaply
/// without importing `rmcp`/`tokio` types.
#[derive(Debug, Clone)]
pub struct ServerCommand {
    /// The executable to run (e.g. `"npx"`).
    pub program: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
}

/// A live connection to one MCP server.
///
/// Owns the `rmcp` [`RunningService`], which in turn owns the child process.
/// Dropping this value cancels the transport and (via `kill_on_drop` on the
/// child) terminates the server process asynchronously; prefer
/// [`McpClient::shutdown`] for bounded, deterministic cleanup.
///
/// The child's **stderr** is piped (never inherited) and drained by a
/// background task into a bounded [`McpStderrBuffer`], so server diagnostics
/// never corrupt jinn's terminal.
pub struct McpClient {
    service: RunningService<RoleClient, ()>,
    /// Shared stderr ring buffer, written by the drain task.
    stderr_buffer: Arc<Mutex<McpStderrBuffer>>,
    /// Owned child process for HTTP-mode servers (stdio mode leaves this
    /// `None` because rmcp's `TokioChildProcess` owns the child).
    child: Option<tokio::process::Child>,
}

impl McpClient {
    /// Returns `Some(child)` once, for the HTTP child-exit watcher to own.
    /// Only ever `Some` for HTTP-mode connections; stdio (rmcp owns the child)
    /// and remote (no child) always return `None`. After the first call it
    /// returns `None`.
    pub fn take_child(&mut self) -> Option<tokio::process::Child> {
        self.child.take()
    }

    /// A cheap, `Send + 'static` token that cancels the transport's service
    /// loop when `.cancel()` is called.
    ///
    /// Used by the HTTP child-exit watcher: after reaping the child, it cancels
    /// the transport so `is_transport_closed()` flips true (closing `tx`),
    /// which the existing liveness watcher observes to publish `Dead`.
    /// No `&mut self` access to `McpClient` is needed in the watcher — the token
    /// is cloned from the `RunningService`.
    pub fn cancel_token(&self) -> rmcp::service::RunningServiceCancellationToken {
        self.service.cancellation_token()
    }
}

/// A cheap, `Send + 'static` handle for polling whether a connection is
/// still alive, without holding the full [`McpClient`].
///
/// Cloned from the connection's `Peer` (all `Arc`-backed fields).
/// [`LivenessProbe::is_transport_closed`] returns `true` when the underlying
/// transport has closed (server process died, HTTP connection dropped, etc.)
/// — uniform across stdio, HTTP, and RemoteHttp transports.
///
/// Owned by the `McpActor` liveness-watch task so it can detect post-connect
/// death without borrowing the client.
#[derive(Clone)]
pub struct LivenessProbe(rmcp::Peer<RoleClient>);

impl LivenessProbe {
    /// Returns `true` when the underlying transport has closed.
    #[must_use]
    pub fn is_transport_closed(&self) -> bool {
        self.0.is_transport_closed()
    }
}

/// An HTTP-mode MCP server process spawned by jinn but not yet connected.
///
/// [`McpClient::connect_http`] produces this; [`McpClient::connect_with_retry`]
/// consumes it once the HTTP endpoint is reachable. The buffer holds the
/// combined stdout+stderr captured since spawn.
pub struct HalfOpenHttp {
    /// The known URL (`http://<bind_addr>:<port>/mcp`) to connect to.
    pub url: String,
    /// Resolved HTTP headers (name/value pairs) sent on every request,
    /// including the MCP handshake. Already expanded by the caller.
    pub headers: Vec<(String, String)>,
    /// The spawned child process; `kill_on_drop` terminates it if dropped
    /// before connection completes.
    pub child: tokio::process::Child,
    /// Shared log buffer (stdout + stderr), kept after connection so the
    /// inspector's live log pane keeps working.
    pub buffer: Arc<Mutex<McpStderrBuffer>>,
}

/// Builds a `reqwest::Client` whose default headers carry `headers`, so every
/// request — handshake and subsequent JSON-RPC traffic alike — includes them.
///
/// An empty slice yields a plain client (no header behavior change).
///
/// # Errors
///
/// Returns an error attaching the offending header **name** if any name or
/// value is not valid per the HTTP spec; resolved values are never included
/// in error context.
fn http_client_with(
    headers: &[(String, String)],
) -> Result<reqwest::Client, Report<McpClientError>> {
    if headers.is_empty() {
        return Ok(reqwest::Client::default());
    }
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .change_context(McpClientError)
            .attach(format!("invalid MCP header name: {name}"))?;
        let value = HeaderValue::from_str(value)
            .change_context(McpClientError)
            .attach(format!("invalid value for MCP header: {name}"))?;
        map.append(name, value);
    }
    reqwest::Client::builder()
        .default_headers(map)
        .build()
        .change_context(McpClientError)
        .attach("failed to build HTTP client with configured MCP headers")
}

impl McpClient {
    /// Spawns the server process and completes the MCP initialize handshake.
    ///
    /// The child's stderr is piped and drained into an internal ring buffer
    /// (see [`McpClient::stderr_tail`]); it never reaches jinn's terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be spawned or the handshake fails.
    pub async fn connect(cmd: &ServerCommand) -> Result<Self, Report<McpClientError>> {
        let mut std_cmd = std::process::Command::new(&cmd.program);
        std_cmd.args(&cmd.args);
        // Terminal isolation: setsid on Unix / CREATE_NO_WINDOW on Windows —
        // the server process must never write over jinn's terminal.
        jinn_common::process_isolation::isolate(&mut std_cmd);
        let mut command = tokio::process::Command::from(std_cmd);
        command.kill_on_drop(true);

        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .change_context(McpClientError)
            .attach("failed to spawn MCP server process")?;

        let stderr_buffer = Arc::new(Mutex::new(McpStderrBuffer::new()));
        if let Some(stderr) = stderr {
            spawn_line_drain(stderr, stderr_buffer.clone());
        }

        let service =
            ().serve(transport)
                .await
                .change_context(McpClientError)
                .attach("MCP initialize handshake failed")?;

        Ok(Self {
            service,
            stderr_buffer,
            child: None,
        })
    }

    /// Connects over an arbitrary transport instead of spawning a child process.
    ///
    /// Used by integration tests that drive an in-process stub MCP server
    /// over an in-memory duplex pipe. Gated behind the `testkit` feature so it
    /// can never appear in production code paths. There is no child process, so
    /// the stderr buffer is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if the MCP initialize handshake fails over the transport.
    #[cfg(feature = "testkit")]
    pub async fn connect_with_transport<T, E, A>(
        transport: T,
    ) -> Result<Self, Report<McpClientError>>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service =
            ().serve(transport)
                .await
                .change_context(McpClientError)
                .attach("MCP initialize handshake failed")?;
        Ok(Self {
            service,
            stderr_buffer: Arc::new(Mutex::new(McpStderrBuffer::new())),
            child: None,
        })
    }

    /// Spawns an HTTP-mode MCP server as a child process without connecting.
    ///
    /// Parses the bind address from the host portion of `url_template`,
    /// allocates a free port (bind-and-release), expands `<ip>`/`<port>` tokens
    /// in the supplied args **and** in the url template, spawns the server with
    /// `kill_on_drop`, and drains **both stdout and stderr** into one shared log
    /// buffer. Returns the [`HalfOpenHttp`] handle; the caller completes the
    /// connection in [`Self::connect_with_retry`] once the HTTP endpoint is
    /// reachable.
    ///
    /// jinn owns the port allocation and never parses server output for the
    /// bind address — the URL is known the instant the port is allocated.
    ///
    /// # Errors
    ///
    /// Returns an error if the port can't be allocated or the child fails
    /// to spawn.
    ///
    /// # Panics
    ///
    /// Panics if `url_template` somehow yields no URL after token expansion
    /// (impossible for a single-element input).
    #[expect(
        clippy::expect_used,
        reason = "invariant: url_template expands to exactly one URL"
    )]
    pub fn connect_http(
        program: &str,
        args: &[String],
        url_template: &str,
        headers: Vec<(String, String)>,
    ) -> Result<HalfOpenHttp, Report<McpClientError>> {
        let bind_addr = parse_host(url_template);
        let port = pick_free_port(&bind_addr)
            .change_context(McpClientError)
            .attach("failed to allocate a free port for the HTTP MCP server")?;
        let expanded = expand_tokens(args, &bind_addr, port);
        let url = expand_tokens(&[url_template.to_owned()], &bind_addr, port)
            .into_iter()
            .next()
            .expect("url_template is a single-element vec");

        let mut std_cmd = std::process::Command::new(program);
        std_cmd
            .args(&expanded)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Terminal isolation, same as the stdio transport above.
        jinn_common::process_isolation::isolate(&mut std_cmd);
        let mut command = tokio::process::Command::from(std_cmd);
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .change_context(McpClientError)
            .attach("failed to spawn HTTP MCP server process")?;

        let buffer = Arc::new(Mutex::new(McpStderrBuffer::new()));
        if let Some(stdout) = child.stdout.take() {
            spawn_line_drain(stdout, buffer.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_drain(stderr, buffer.clone());
        }

        tracing::info!(%url, "spawned HTTP MCP server");
        Ok(HalfOpenHttp {
            url,
            headers,
            child,
            buffer,
        })
    }

    /// Polls an HTTP endpoint until the MCP initialize handshake succeeds,
    /// with **no wall-clock timeout**.
    ///
    /// Loops on a backoff (~50ms growing to a ~1s cap) until either the
    /// handshake completes (`Ok`) or the child process exits (captured output
    /// is retained in the buffer, surfaced as the error context). A slow-booting
    /// server stays `Starting` for as long as it needs.
    ///
    /// # Errors
    ///
    /// Returns an error if the child process exits before the endpoint is
    /// reachable. Connection-refused while the server boots is retried
    /// indefinitely, so this only returns `Err` on process exit or an
    /// unrecoverable handshake failure.
    pub async fn connect_with_retry(half: HalfOpenHttp) -> Result<Self, Report<McpClientError>> {
        let HalfOpenHttp {
            url,
            headers,
            mut child,
            buffer,
        } = half;
        // Build the HTTP client (with resolved default headers) once, before
        // the retry loop — every attempt reuses it.
        let http = http_client_with(&headers)?;
        let mut backoff = Duration::from_millis(50);
        let cap = Duration::from_secs(1);

        loop {
            // If the child has already exited, surface the captured output.
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let captured = buffer
                        .lock()
                        .map(|b| b.tail().to_owned())
                        .unwrap_or_default();
                    return Err(Report::new(McpClientError)
                        .attach("HTTP MCP server process exited before connecting")
                        .attach(captured));
                }
                Ok(None) => {} // still running — proceed to attempt connect
                Err(e) => {
                    return Err(Report::new(McpClientError)
                        .attach("failed to poll HTTP MCP server process")
                        .attach(format!("{e:?}")));
                }
            }

            // Attempt the handshake. A refused connection is expected while the
            // server boots; retry after backoff. Each attempt is bounded so a
            // hung handshake (port open but server unresponsive) still lets the
            // child-exit check re-run — without this, a server that accepts the
            // TCP connection then hangs mid-handshake blocks forever.
            match Self::attempt_http_handshake(&http, &url).await {
                Ok(service) => {
                    tracing::info!(%url, "connected to HTTP MCP server");
                    return Ok(Self {
                        service,
                        stderr_buffer: buffer,
                        child: Some(child),
                    });
                }
                Err(reason) => {
                    tracing::debug!(reason, %url, "HTTP MCP connect attempt failed; retrying");
                }
            }

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(cap);
        }
    }

    /// One bounded attempt at the MCP HTTP handshake.
    ///
    /// Returns `Ok(service)` on success, or `Err(reason)` (a tracing field
    /// value string) if the handshake errored or timed out — both are
    /// retryable. The `client` carries any configured default headers and is
    /// reused across attempts by the caller.
    async fn attempt_http_handshake(
        client: &reqwest::Client,
        url: &str,
    ) -> Result<RunningService<RoleClient, ()>, &'static str> {
        let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
        let transport = StreamableHttpClientTransport::with_client(client.clone(), config);
        match tokio::time::timeout(Duration::from_secs(3), ().serve(transport)).await {
            Ok(Ok(service)) => Ok(service),
            Ok(Err(_e)) => Err("handshake error"),
            Err(_elapsed) => Err("attempt timed out"),
        }
    }

    /// Connects to a remote, externally-managed HTTP MCP server.
    ///
    /// No child process is spawned or owned; jinn only connects to `url`. The
    /// handshake is retried on a backoff (no wall-clock timeout), matching the
    /// managed-HTTP behavior, but with **no child-exit check** — there is no
    /// child to observe, so connect retries forever until the endpoint is up.
    ///
    /// # Errors
    ///
    /// Returns an error if the header set is invalid or the underlying
    /// transport construction fails.
    pub async fn connect_remote(
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<Self, Report<McpClientError>> {
        // Build the HTTP client (with resolved default headers) once, before
        // the retry loop — every attempt reuses it.
        let http = http_client_with(&headers)?;
        let mut backoff = Duration::from_millis(50);
        let cap = Duration::from_secs(1);
        loop {
            match Self::attempt_http_handshake(&http, url).await {
                Ok(service) => {
                    tracing::info!(url, "connected to remote HTTP MCP server");
                    let stderr_buffer = Arc::new(Mutex::new(McpStderrBuffer::new()));
                    return Ok(Self {
                        service,
                        stderr_buffer,
                        child: None,
                    });
                }
                Err(reason) => {
                    tracing::debug!(
                        reason,
                        url,
                        "remote HTTP MCP connect attempt failed; retrying"
                    );
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(cap);
        }
    }

    /// Lists the tools the server exposes.
    ///
    /// # Errors
    ///
    /// Returns an error if the `tools/list` request fails.
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, Report<McpClientError>> {
        self.service
            .list_tools(Option::default())
            .await
            .map(|result| result.tools)
            .change_context(McpClientError)
            .attach("tools/list request failed")
    }

    /// Calls a tool by name with optional JSON object arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if the `tools/call` request fails at the transport or
    /// protocol level. A tool-reported error (`is_error`) is returned as a
    /// successful [`CallToolResult`], not an [`Err`].
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<CallToolResult, Report<McpClientError>> {
        let mut params = CallToolRequestParams::new(name.to_owned());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }
        self.service
            .call_tool(params)
            .await
            .change_context(McpClientError)
            .attach("tools/call request failed")
    }

    /// Returns a shared handle to the underlying stderr ring buffer.
    ///
    /// Allows a background task (e.g. the live-inspector debounce) to read the
    /// tail without cloning the full client connection.
    #[must_use]
    pub fn stderr_buffer(&self) -> Arc<Mutex<McpStderrBuffer>> {
        self.stderr_buffer.clone()
    }

    /// Returns a snapshot of the captured stderr tail (newest content).
    ///
    /// May be empty if the server hasn't written anything to stderr.
    #[must_use]
    pub fn stderr_tail(&self) -> String {
        self.stderr_buffer
            .lock()
            .map(|buf| buf.tail().to_owned())
            .unwrap_or_default()
    }

    /// Returns `true` when the underlying transport has closed (the server
    /// process died, the HTTP connection dropped, etc.).
    ///
    /// Uniform across stdio, HTTP, and RemoteHttp transports — it reads
    /// rmcp's transport-level close signal, not a process handle. Used by the
    /// `McpActor` liveness watcher to detect post-connect death.
    #[must_use]
    pub fn is_transport_closed(&self) -> bool {
        self.service.is_transport_closed()
    }

    /// Returns a cheap, `Send + 'static` handle for polling connection liveness
    /// without holding the full [`McpClient`].
    ///
    /// Cloned from the connection's `Peer`; see [`LivenessProbe`]. Used by the
    /// `McpActor` liveness-watch task.
    #[must_use]
    pub fn liveness_probe(&self) -> LivenessProbe {
        LivenessProbe(self.service.peer().clone())
    }

    /// Gracefully closes the connection and waits (bounded) for the service
    /// loop to terminate.
    ///
    /// Does **not** kill the HTTP child — for HTTP-mode servers the child is
    /// moved into the child-exit watcher via [`take_child`] at connect time,
    /// and `kill_on_drop` on that handle terminates the process when the
    /// watcher exits. stdio and remote transports have no jinn-owned child.
    pub async fn shutdown(&mut self) {
        if let Err(e) = self
            .service
            .close_with_timeout(Duration::from_secs(5))
            .await
        {
            tracing::warn!(error = ?e, "MCP client shutdown join error");
        }
    }
}

// ─── stderr ring buffer ────────────────────────────────────────────────

/// Maximum bytes of child stderr to retain (the most recent tail).
const MCP_STDERR_MAX_BYTES: usize = 16 * 1024;

/// Bounded ring buffer for captured child-process stderr.
///
/// Keeps the most recent lines (by byte budget). Older content is dropped
/// when the budget is exceeded, mirroring the `bash` tool's streaming
/// truncation: newest content survives, memory is bounded.
#[derive(Debug, Clone)]
pub struct McpStderrBuffer {
    content: String,
    max_bytes: usize,
}

impl Default for McpStderrBuffer {
    fn default() -> Self {
        Self::new()
    }
}
impl McpStderrBuffer {
    /// Creates a buffer with the default 16&nbsp;KB tail budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budget(MCP_STDERR_MAX_BYTES)
    }

    /// Creates a buffer with a custom byte budget.
    #[must_use]
    pub fn with_budget(max_bytes: usize) -> Self {
        Self {
            content: String::new(),
            max_bytes,
        }
    }

    /// Appends one line and trims to the byte budget (keeping the tail).
    pub fn append_line(&mut self, line: &str) {
        if !self.content.is_empty() {
            self.content.push('\n');
        }
        self.content.push_str(line);
        self.trim_to_budget();
    }

    /// Drops leading content until the buffer fits within `max_bytes`,
    /// advancing to the next UTF-8 char boundary so multibyte characters are
    /// never split.
    fn trim_to_budget(&mut self) {
        if self.content.len() <= self.max_bytes {
            return;
        }
        let cut = self.content.len().saturating_sub(self.max_bytes);
        let mut start = cut;
        while !self.content.is_char_boundary(start) {
            start += 1;
        }
        self.content.drain(0..start);
    }

    /// Returns the captured tail (may be empty).
    #[must_use]
    pub fn tail(&self) -> &str {
        &self.content
    }
}

/// Spawns a background task that drains a child stream line-by-line into the
/// shared buffer. Works for both `ChildStderr` and `ChildStdout` (HTTP mode
/// interleaves both into one log buffer). The task exits naturally when the
/// child process dies and the pipe closes (EOF), so no explicit cancellation
/// is needed — `kill_on_drop` guarantees the child terminates.
fn spawn_line_drain<R>(stream: R, buffer: Arc<Mutex<McpStderrBuffer>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut buf) = buffer.lock() {
                buf.append_line(&line);
            }
        }
        // EOF or error: child died or pipe closed — drain exits.
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn stderr_buffer_starts_empty() {
        // Given a fresh buffer.
        let buf = McpStderrBuffer::default();

        // Then its tail is empty.
        assert!(buf.tail().is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn stderr_buffer_appends_and_joins_lines() {
        // Given a fresh buffer.
        let mut buf = McpStderrBuffer::default();

        // When appending two lines.
        buf.append_line("first");
        buf.append_line("second");

        // Then both are retained, newline-joined.
        assert_eq!(buf.tail(), "first\nsecond");
    }

    #[rstest::rstest]
    #[test]
    fn stderr_buffer_drops_oldest_when_over_byte_budget() {
        // Given a buffer with a tiny 10-byte budget.
        let mut buf = McpStderrBuffer::with_budget(10);

        // When appending lines that overflow the budget.
        buf.append_line("AAAAA"); // 5 bytes, fits
        buf.append_line("BBBBB"); // 5 + 1 newline + 5 = 11, trims oldest

        // Then only the most recent content survives and the budget holds.
        assert!(buf.tail().len() <= 11);
        assert!(buf.tail().ends_with("BBBBB"));
        assert!(!buf.tail().starts_with("AAAAA"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn connect_http_spawns_and_captures_combined_output() {
        // Given a no-op server script that prints to both stdout and stderr
        // and stays alive (sleeps), with a <port> token in args.
        // Use `sh -c` to print to both streams; the <port> token is unused
        // by the script but must expand without error.
        let args = vec![
            "-c".to_owned(),
            "echo stdout-line; echo stderr-line >&2; sleep 1".to_owned(),
            "<port>".to_owned(), // token presence proves expansion path
        ];

        let mut half =
            McpClient::connect_http("sh", &args, "http://127.0.0.1:<port>/mcp", Vec::new())
                .expect("spawn");

        // Then the URL is a localhost URL on a real port.
        assert!(half.url.starts_with("http://127.0.0.1:"));
        assert!(half.url.ends_with("/mcp"));

        // And after a brief wait, the buffer captures both streams.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let tail = half
            .buffer
            .lock()
            .map(|b| b.tail().to_owned())
            .unwrap_or_default();
        assert!(tail.contains("stdout-line"), "stdout captured: {tail}");
        assert!(tail.contains("stderr-line"), "stderr captured: {tail}");

        // Cleanup: kill the child.
        let _ = half.child.kill().await;
    }

    #[rstest::rstest]
    #[test]
    fn http_client_with_empty_headers_is_plain_client() {
        // Given no resolved headers.
        // When building the HTTP client.
        let client = http_client_with(&[]).expect("client builds");

        // Then a usable default client comes back (no panic, no error).
        let _ = client;
    }

    #[rstest::rstest]
    #[test]
    fn invalid_header_name_fails_attaching_name_not_value() {
        // Given a header pair with a name that is not a valid HTTP token.
        let headers = vec![("Bad Header\nName".to_owned(), "whatever".to_owned())];

        // When building the HTTP client.
        let result = http_client_with(&headers);

        // Then it fails, and the report names the header — not its value.
        let err = result.expect_err("invalid name must fail");
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("Bad Header"),
            "name in report: {rendered}"
        );
        assert!(
            !rendered.contains("whatever"),
            "value must not appear: {rendered}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn invalid_header_value_fails_attaching_name_not_value() {
        // Given a header pair whose value contains an illegal control char.
        let headers = vec![("X-Ok".to_owned(), "bad\u{0}value".to_owned())];

        // When building the HTTP client.
        let result = http_client_with(&headers);

        // Then it fails naming the header only.
        // (HeaderName's Display is canonicalized to lowercase.)
        let err = result.expect_err("invalid value must fail");
        let rendered = format!("{err:?}");
        let lowered = rendered.to_ascii_lowercase();
        assert!(
            lowered.contains("x-ok"),
            "header name in report: {rendered}"
        );
        assert!(
            !rendered.contains('\u{0}'),
            "raw value must not appear: {rendered}"
        );
    }

    /// Default headers built by `http_client_with` are sent on real requests.
    #[rstest::rstest]
    #[tokio::test]
    async fn default_headers_are_sent_on_the_wire() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Given a TCP listener acting as a raw HTTP capture server.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // And an HTTP client carrying one Authorization default header.
        let client = http_client_with(&[(
            "Authorization".to_owned(),
            "Bearer captured-value".to_owned(),
        )])
        .expect("client builds");

        // When issuing a request to the capture server.
        let request_handle =
            tokio::spawn(async move { client.get(format!("http://{addr}/probe")).send().await });

        // And capturing exactly one connection's request head, bounded by a
        // short wall-clock guard and a loop (TCP fragments — a single `read`
        // is not guaranteed to see the whole head).
        let captured = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut sock, _) = listener.accept().await?;
            let mut head = String::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                if let Some(chunk) = buf.get(..n) {
                    head.push_str(&String::from_utf8_lossy(chunk));
                }
                if head.contains("\r\n\r\n") {
                    break;
                }
            }
            // Respond so the pending client send completes instead of
            // deadlocking the test against itself.
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await;
            drop(sock);
            Ok::<String, std::io::Error>(head)
        })
        .await;

        // Then the configured header appears on the wire. The captured head is
        // included in every failure message for diagnosis (no subscriber wired
        // in unit tests, so tracing output would be invisible here).
        let head = captured
            .expect("capture must finish within 2s")
            .expect("read");
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer captured-value"),
            "expected header on wire, got: {head}"
        );

        // The sender completes once the canned response arrives.
        let _ = request_handle.await;
    }
}
