//! MCP client actor — owns one connection to one MCP server for one session.
//!
//! One [`McpActor`] is spawned per (session × enabled server). It connects to
//! the server over stdio (via [`McpClient`]), lists the server's tools at
//! startup, registers them under the `mcp__<server>__<tool>` namespace as
//! session-scoped tools, and answers [`ExecuteTool`] calls by forwarding them
//! to the server's `tools/call` and publishing the result as
//! [`ToolExecutionCompleted`].
//!
//! # Lifecycle
//!
//! - [`Actor::on_start`]: subscribe to [`ExecuteTool`]; connect the client and
//!   list tools. On success, register the tools. On failure, log and let the
//!   actor stop — the [`McpCoordinatorActor`] reports status; a later
//!   enable/disable cycle can respawn.
//! - [`Actor::on_stop`]: shut the client down so the child process terminates.
//!
//! Each tool call is dispatched to a standalone task so the mailbox stays free
//! for concurrent requests.

// End-to-end dispatch roundtrip tests live in a separate module so the
// stub-server fixtures (gated behind jinn-mcp's `server-testkit` feature)
// stay isolated from the pure unit tests above.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jinn_mcp::{
    CallToolResult, ContentBlock, JsonObject, McpClient, McpClientError, ServerCommand,
    tool_mapping::{map_tool, provider_name, strip_namespace},
};
use kameo::actor::ActorRef;
use kameo::prelude::{Context, Message};
use parking_lot::Mutex;

use error_stack::{Report, ResultExt as _};
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_domain::common::actor_deps::{ActorDeps, BusPublish};
use jinn_domain::feat::mcp::{McpServerConfig, TransportKind};
use jinn_domain::protocol::SessionId;
use jinn_mcp_msg::{McpConnectionStatus, McpServerLog, McpServerStatus};
use jinn_tools_msg::truncation::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use jinn_tools_msg::{ExecuteTool, RegisterTools};
use jinn_tools_msg::{ToolExecutionCompleted, ToolsUnregistered};

use jinn_domain::Services;

/// Debounce interval for live stderr republishing while the actor is Running.
const STDERR_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

pub struct McpActor {
    deps: ActorDeps,
    /// The session this actor serves.
    session_id: SessionId,
    /// The server's name — the `[mcp_server.<name>]` table key.
    name: String,
    /// The live MCP client connection, established during `on_start`.
    client: Option<McpClient>,
    /// Cancellation flag for the stderr-debounce republish task. Set in
    /// `on_stop` so the task exits promptly when the actor tears down.
    stderr_task_shutdown: Arc<AtomicBool>,
    /// Cancellation flag for the liveness-watch task. Set in `on_stop`
    /// *before* `stderr_task_shutdown` so the watcher exits before this
    /// teardown's own `Dead` publish (no spurious double-publish).
    liveness_task_shutdown: Arc<AtomicBool>,
    /// Cancellation flag for the HTTP child-exit watcher (`None` for
    /// stdio/remote transports, which have no jinn-owned child). Set in
    /// `on_stop` before `client.shutdown()` so the watcher exits and drops
    /// the `Child` (`kill_on_drop` terminates a still-alive process).
    child_task_shutdown: Option<Arc<AtomicBool>>,
}

/// Dependencies for [`McpActor`].
///
/// Implements [`Clone`] so the actor can be spawned under kameo's
/// supervision tree. The optional injected client lives behind a shared slot
/// (`Arc<Mutex<Option<McpClient>>>`) so cloning the deps clones the *handle* to
/// the slot, not the client itself; `on_start` drains the slot once.
#[derive(Clone)]
pub struct McpActorDeps {
    /// Common actor dependencies (services + bus).
    deps: ActorDeps,
    /// The session this actor serves.
    session_id: SessionId,
    /// The server's name — the `[mcp_server.<name>]` table key. Carried
    /// separately because `McpServerConfig` no longer has a `name` field.
    name: String,
    /// The configured server to connect to.
    server: McpServerConfig,
    /// Optional pre-connected client, used only by integration tests.
    ///
    /// Production is always `None` (the actor spawns the server from `server`).
    /// A test injects a client connected to an in-process stub so the full
    /// subscribe → list → register → dispatch path runs without a child.
    client_override: Arc<Mutex<Option<McpClient>>>,
}

impl McpActorDeps {
    /// Production constructor: spawns the server process at `on_start`.
    #[must_use]
    pub fn new(
        deps: ActorDeps,
        session_id: SessionId,
        name: String,
        server: McpServerConfig,
    ) -> Self {
        Self {
            deps,
            session_id,
            name,
            server,
            client_override: Arc::new(Mutex::new(None)),
        }
    }

    /// Test-only constructor: inject a pre-connected [`McpClient`] so `on_start`
    /// skips spawning a child process.
    #[cfg(test)]
    #[must_use]
    pub fn with_client(
        deps: ActorDeps,
        session_id: SessionId,
        name: String,
        server: McpServerConfig,
        client: McpClient,
    ) -> Self {
        Self {
            deps,
            session_id,
            name,
            server,
            client_override: Arc::new(Mutex::new(Some(client))),
        }
    }
}

/// Builds the [`ServerCommand`] for an [`McpServerConfig`].
///
/// # Panics
///
/// Panics if `command` is `None` — only valid for `RemoteHttp` servers,
/// which never reach this function.
#[expect(
    clippy::expect_used,
    reason = "invariant: documented, only called for Stdio/LocalHttp"
)]
fn server_command(config: &McpServerConfig) -> ServerCommand {
    ServerCommand {
        program: config
            .command
            .clone()
            .expect("command required for Stdio/LocalHttp servers"),
        args: config.args.clone(),
    }
}

/// Connects a fresh [`McpClient`] according to the server's configured transport.
///
/// - [`TransportKind::Stdio`]: spawn the child and handshake over stdin/stdout.
///   Configured `headers` are ignored entirely.
/// - [`TransportKind::LocalHttp`]: spawn the child (managed port via `<port>`
///   token, bind address parsed from `url`), then poll the HTTP endpoint on a
///   backoff until the handshake succeeds or the child exits (no wall-clock timeout).
/// - [`TransportKind::RemoteHttp`]: connect to the configured `url` with no child.
///
/// For both HTTP transports, configured header values are expanded from
/// [`ApiKeysService`](jinn_domain::common::services::api_keys_service::ApiKeysService)
/// (`${VAR}` tokens resolved against variables seeded at startup) *before* any
/// connect attempt; an unresolvable variable aborts the connect, which the
/// caller surfaces as a dead server.
#[expect(
    clippy::expect_used,
    reason = "invariants: url/command documented per transport kind"
)]
pub(crate) async fn connect_for_transport(
    services: &Services,
    server: &McpServerConfig,
) -> Result<McpClient, Report<McpClientError>> {
    match server.transport {
        TransportKind::Stdio => McpClient::connect(&server_command(server)).await,
        TransportKind::LocalHttp => {
            let url = server
                .url
                .as_ref()
                .expect("url required for LocalHttp servers");
            let headers = expand_server_headers(services, server)?;
            let half = McpClient::connect_http(
                server
                    .command
                    .as_deref()
                    .expect("command required for LocalHttp servers"),
                &server.args,
                url,
                headers,
            )?;
            McpClient::connect_with_retry(half).await
        }
        TransportKind::RemoteHttp => {
            let url = server
                .url
                .as_ref()
                .expect("url required for RemoteHttp servers");
            let headers = expand_server_headers(services, server)?;
            McpClient::connect_remote(url, headers).await
        }
    }
}

/// Expands `${VAR}` tokens in one server's configured headers against the
/// startup-seeded key store.
///
/// Header **names** pass through literally; only values expand. Failure means
/// some referenced variable is missing/empty — propagated as-is so the error
/// names the variable and nothing else.
///
/// # Errors
///
/// Returns [`HeaderExpandError::UnresolvedVariable`] (via [`McpClientError`]
/// context) when expansion fails.
fn expand_server_headers(
    services: &Services,
    server: &McpServerConfig,
) -> Result<Vec<(String, String)>, Report<McpClientError>> {
    let resolve = |name: &str| services.api_keys.get(name);
    jinn_domain::feat::mcp::expand_mcp_headers(&server.headers, &resolve)
        .change_context(McpClientError)
}

/// Acquires a connected [`McpClient`] and the server's tool definitions.
///
/// Production path: spawn the server process (`server.command`/`args`) and
/// list its tools. Test path: use the injected `client_override` and list its
/// tools — no child process is spawned.
///
/// On failure returns `Err(Some(client))` where `client` is half-open (
/// connected but list failed) and must be shut down by the caller; returns
/// `Err(None)` when the failure was at connect time (nothing to shut down).
/// On success returns the client and its mapped tool definitions together.
async fn acquire_client(
    services: &Services,
    name: &str,
    server: &McpServerConfig,
    client_override: Option<McpClient>,
) -> Result<(McpClient, Vec<ToolDefinition>), Option<McpClient>> {
    let client = match client_override {
        Some(injected) => injected,
        None => match connect_for_transport(services, server).await {
            Ok(client) => client,
            Err(report) => {
                tracing::warn!(
                    server = %name,
                    error = %report,
                    "MCP actor: failed to connect to server"
                );
                // Connect failed before any client existed.
                return Err(None);
            }
        },
    };

    match client.list_tools().await {
        Ok(tools) => {
            let definitions = tools
                .iter()
                .map(|tool| map_tool(name, tool))
                .collect::<Vec<ToolDefinition>>();
            Ok((client, definitions))
        }
        Err(report) => {
            tracing::warn!(
                server = %name,
                error = %report,
                "MCP actor: failed to list tools"
            );
            // Half-open: connected but list failed. Return the client so the
            // caller can shut it down.
            Err(Some(client))
        }
    }
}

/// Converts an rmcp `CallToolResult` into a single text string for jinn's
/// `ToolResult::content`.
///
/// Text content blocks are concatenated (separated by newlines); non-text
/// blocks (images, audio, resources) are summarized as placeholders so the LLM
/// knows they existed even though they can't be rendered as text.
fn format_result_content(result: &CallToolResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &result.content {
        match block {
            ContentBlock::Text(text) => parts.push(text.text.clone()),
            ContentBlock::Image(_) => parts.push("[image content]".to_owned()),
            ContentBlock::Audio(_) => parts.push("[audio content]".to_owned()),
            ContentBlock::Resource(_) => parts.push("[resource content]".to_owned()),
            ContentBlock::ResourceLink(_) => {
                parts.push("[resource link]".to_owned());
            }
            _ => parts.push("[unsupported content]".to_owned()),
        }
    }
    parts.join("\n")
}

impl kameo::Actor for McpActor {
    type Args = McpActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let McpActorDeps {
            deps,
            session_id,
            name,
            server,
            client_override,
        } = args;

        deps.subscribe(actor_ref.recipient::<ExecuteTool>()).await;

        publish_status(&deps, &session_id, &name, McpConnectionStatus::Starting).await;

        // Drain any test-injected client from the shared slot. Production is
        // always `None` here, so the actor spawns the server process. Tests
        // inject a pre-connected client so no child is spawned.
        let injected_client = client_override.lock().take();

        // Obtain a connected client + its tool definitions. Failure (connect or
        // list) is non-fatal to the process: the actor runs idle, the lifecycle
        // actor / dashboard surfaces the dead status, and a later enable/disable
        // cycle can respawn.
        let (mut client, definitions) =
            match acquire_client(&deps.services, &name, &server, injected_client).await {
                Ok(ready) => ready,
                Err(half_open) => {
                    if let Some(mut half_open) = half_open {
                        half_open.shutdown().await;
                    }
                    publish_status(&deps, &session_id, &name, McpConnectionStatus::Dead).await;
                    return Ok(Self {
                        deps,
                        session_id,
                        name,
                        client: None,
                        stderr_task_shutdown: Arc::new(AtomicBool::new(false)),
                        liveness_task_shutdown: Arc::new(AtomicBool::new(false)),
                        child_task_shutdown: None,
                    });
                }
            };

        let provider = provider_name(&name);
        tracing::info!(
            server = %name,
            session_id = %session_id,
            tool_count = definitions.len(),
            "MCP actor: connected, registering tools"
        );

        let () = deps
            .services
            .bus
            .publish(RegisterTools {
                provider,
                definitions,
                session_id: Some(session_id.clone()),
            })
            .await;

        // Tools registered + connection live: we're Running.
        publish_status(&deps, &session_id, &name, McpConnectionStatus::Running).await;
        // Surface any stderr emitted during startup (e.g. `npm warn`).
        publish_log(&deps, &session_id, &name, &client.stderr_tail()).await;

        // Spawn the live stderr-debounce task. It polls the client's tail every
        // `STDERR_DEBOUNCE` and republishes `McpServerLog` when the tail changed,
        // so subscribers (the inspector) see stderr update in near-real time.
        // The task exits when `stderr_task_shutdown` is set in `on_stop`.
        let stderr_task_shutdown = Arc::new(AtomicBool::new(false));
        spawn_stderr_debounce(
            stderr_task_shutdown.clone(),
            client.stderr_buffer(),
            deps.clone(),
            session_id.clone(),
            name.clone(),
        );

        // Spawn the liveness-watch task. It polls the client's transport
        // close signal every `STDERR_DEBOUNCE` and publishes `Dead` when the
        // connection drops post-connect, so the sidebar/picker stop showing
        // "running" for a dead server. It owns a cheap `LivenessProbe` cloned
        // from the client (no shared borrow). Exits via its shutdown flag in
        // `on_stop` (which does its own `Dead` publish).
        let liveness_task_shutdown = Arc::new(AtomicBool::new(false));
        spawn_liveness_watch(
            liveness_task_shutdown.clone(),
            client.liveness_probe(),
            client.stderr_buffer(),
            stderr_task_shutdown.clone(),
            deps.clone(),
            session_id.clone(),
            name.clone(),
        );

        // Spawn the HTTP child-exit watcher — only for HTTP-mode connections,
        // where jinn owns the child directly. stdio (rmcp owns the child) and
        // remote (no child) return None from take_child(), so no watcher is
        // spawned and child_task_shutdown stays None.
        let child_task_shutdown = Arc::new(AtomicBool::new(false));
        let cancel_token = client.cancel_token();
        let child_watch_spawned = match client.take_child() {
            Some(child) => {
                spawn_child_watch(child_task_shutdown.clone(), child, cancel_token);
                true
            }
            None => false,
        };
        let child_task_shutdown = child_watch_spawned.then_some(child_task_shutdown);

        Ok(Self {
            deps,
            session_id,
            name,
            client: Some(client),
            stderr_task_shutdown,
            liveness_task_shutdown,
            child_task_shutdown,
        })
    }

    async fn on_stop(
        &mut self,
        _actor_ref: kameo::actor::WeakActorRef<Self>,
        _reason: kameo::error::ActorStopReason,
    ) -> Result<(), Self::Error> {
        // Signal the liveness-watch task first so it cannot publish a `Dead`
        // that races this teardown's own `Dead` publish below.
        self.liveness_task_shutdown.store(true, Ordering::SeqCst);
        // Then signal the stderr-debounce task to exit before we tear down.
        self.stderr_task_shutdown.store(true, Ordering::SeqCst);
        // Then signal the HTTP child-exit watcher (if any) to exit; on exit it
        // drops the `Child`, and `kill_on_drop` terminates a still-alive process.
        if let Some(flag) = self.child_task_shutdown.as_ref() {
            flag.store(true, Ordering::SeqCst);
        }

        let tail = self
            .client
            .as_ref()
            .map(jinn_mcp::McpClient::stderr_tail)
            .unwrap_or_default();
        if let Some(client) = self.client.as_mut() {
            client.shutdown().await;
        }
        publish_status(
            &self.deps,
            &self.session_id,
            &self.name,
            McpConnectionStatus::Dead,
        )
        .await;
        publish_log(&self.deps, &self.session_id, &self.name, &tail).await;
        // Teardown removes this server's session-scoped tool registrations
        // everywhere they were cached. Harmless no-op when startup failed
        // before any `RegisterTools` fired (subscribers prune by key).
        let () = self
            .deps
            .services
            .bus
            .publish(ToolsUnregistered {
                provider: provider_name(&self.name),
                session_id: self.session_id.clone(),
            })
            .await;
        Ok(())
    }
}

/// Publishes a connection-status transition for this (session × server).
async fn publish_status(
    deps: &ActorDeps,
    session_id: &SessionId,
    server: &str,
    status: McpConnectionStatus,
) {
    let () = deps
        .services
        .bus
        .publish(McpServerStatus {
            session_id: session_id.clone(),
            server: server.to_owned(),
            status,
        })
        .await;
}

/// Publishes the captured stderr tail for this (session × server).
///
/// Best-effort: an empty tail is still published so subscribers can clear
/// stale content. Called at status transitions and on shutdown.
async fn publish_log(deps: &ActorDeps, session_id: &SessionId, server: &str, tail: &str) {
    let () = deps
        .services
        .bus
        .publish(McpServerLog {
            session_id: session_id.clone(),
            server: server.to_owned(),
            tail: tail.to_owned(),
        })
        .await;
}

/// Spawns a background task that republishes `McpServerLog` whenever the
/// client's stderr tail changes, on a [`STDERR_DEBOUNCE`] cadence.
///
/// Reads the shared stderr ring buffer directly (no need to clone the full
/// client connection). Keeps the live inspector up to date without flooding
/// the bus. The task exits promptly when `shutdown` is set
/// (see [`McpActor::on_stop`]).
fn spawn_stderr_debounce(
    shutdown: Arc<AtomicBool>,
    stderr_buffer: std::sync::Arc<std::sync::Mutex<jinn_mcp::McpStderrBuffer>>,
    deps: ActorDeps,
    session_id: SessionId,
    server_name: String,
) {
    tokio::spawn(async move {
        let mut last_tail = String::new();
        loop {
            tokio::time::sleep(STDERR_DEBOUNCE).await;
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            let tail = stderr_buffer
                .lock()
                .map(|buf| buf.tail().to_owned())
                .unwrap_or_default();
            if let Some(tail) = next_tail(&tail, &last_tail) {
                publish_log(&deps, &session_id, &server_name, &tail).await;
                last_tail = tail;
            }
        }
    });
}

/// Spawns the liveness-watch task that publishes `Dead` when the connection
/// drops post-connect.
///
/// Polls the client's transport-close signal (via a cheap [`LivenessProbe`]
/// cloned from the client — no shared borrow) every [`STDERR_DEBOUNCE`]. On
/// close: stops the stderr-debounce task *first* (so it cannot overwrite the
/// final tail), then publishes `Dead` and the final captured tail, then exits.
///
/// Normal teardown wins: `on_stop` sets `liveness_shutdown` before its own
/// `Dead` publish, so the watcher exits without double-publishing.
fn spawn_liveness_watch(
    liveness_shutdown: Arc<AtomicBool>,
    probe: jinn_mcp::LivenessProbe,
    stderr_buffer: std::sync::Arc<std::sync::Mutex<jinn_mcp::McpStderrBuffer>>,
    stderr_task_shutdown: Arc<AtomicBool>,
    deps: ActorDeps,
    session_id: SessionId,
    server_name: String,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(STDERR_DEBOUNCE).await;
            if liveness_shutdown.load(Ordering::SeqCst) {
                break;
            }
            if probe.is_transport_closed() {
                // Stop the stderr-debounce first so its next poll cannot
                // republish stale content over the final tail we publish here.
                stderr_task_shutdown.store(true, Ordering::SeqCst);
                let tail = stderr_buffer
                    .lock()
                    .map(|buf| buf.tail().to_owned())
                    .unwrap_or_default();
                publish_status(&deps, &session_id, &server_name, McpConnectionStatus::Dead).await;
                publish_log(&deps, &session_id, &server_name, &tail).await;
                break;
            }
        }
    });
}

/// Spawns the HTTP child-exit watcher.
///
/// Only spawned for HTTP-mode connections (the only transport where jinn
/// owns the child directly). Polls `try_wait()` every [`STDERR_DEBOUNCE`] to
/// **reap** the child (preventing zombies) and to detect process death. On
/// death: cancels the transport via the cloned `RunningServiceCancellationToken`
/// so `is_transport_closed()` flips true — the existing liveness watcher then
/// publishes `Dead`. This watcher does **not** publish `Dead` itself, to keep a
/// single death signal and avoid double-publish races.
///
/// On loop exit (child dead or `shutdown` set): the `Child` drops, and
/// `kill_on_drop` kills a still-alive process (normal teardown case) or
/// no-ops an already-dead one (`kill -9` case).
pub(crate) fn spawn_child_watch(
    shutdown: Arc<AtomicBool>,
    mut child: tokio::process::Child,
    cancel_token: jinn_mcp::RunningServiceCancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(STDERR_DEBOUNCE).await;
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Child exited — `try_wait` reaped it (no zombie). Cancel
                    // the transport so is_transport_closed() flips true; the
                    // liveness watcher publishes Dead on its next tick.
                    cancel_token.cancel();
                    break;
                }
                Ok(None) => {} // still alive — keep polling
                Err(e) => {
                    tracing::warn!(error = ?e, "HTTP MCP child try_wait failed; stopping watcher");
                    break;
                }
            }
        }
        // `child` drops here → kill_on_drop terminates a still-alive process.
    });
}

/// Returns the tail to publish when it differs from the last-published one,
/// else `None`.
///
/// Extracted from the debounce loop so the change-detection logic is
/// unit-testable without depending on the timer cadence.
fn next_tail(current: &str, last_published: &str) -> Option<String> {
    (current != last_published).then(|| current.to_owned())
}

impl BusPublish for McpActor {
    fn bus(&self) -> &jinn_domain::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl Message<ExecuteTool> for McpActor {
    type Reply = ();

    async fn handle(&mut self, msg: ExecuteTool, _ctx: &mut Context<Self, Self::Reply>) {
        // Only handle calls for this session whose tool name carries this
        // server's namespace prefix.
        if msg.session_id != self.session_id {
            return;
        }
        let Some(tool_name) = strip_namespace(&self.name, &msg.tool_call.name).map(str::to_owned)
        else {
            return;
        };

        let Some(client) = self.client.as_ref() else {
            // Client never connected (dead on start). Report a failed result.
            self.deps
                .publish(ToolExecutionCompleted {
                    session_id: msg.session_id,
                    result: failure_result(&msg.tool_call, "MCP server is not connected"),
                })
                .await;
            return;
        };

        let session_id = msg.session_id;
        let tool_call = msg.tool_call;
        let max_output_lines = msg.max_output_lines;
        let max_output_bytes = msg.max_output_bytes;

        let arguments = match parse_arguments(&tool_call.arguments) {
            Ok(args) => args,
            Err(err_msg) => {
                self.deps
                    .publish(ToolExecutionCompleted {
                        session_id,
                        result: failure_result(&tool_call, err_msg),
                    })
                    .await;
                return;
            }
        };

        // Run the call inline. Tool calls are I/O bound and infrequent;
        // serializing per-server is acceptable. The orchestrator batches
        // concurrent calls, bounding any mailbox backlog.
        //
        // The call is raced against `tool_default_timeout_secs` (the same
        // safety ceiling builtin tools run under). Actor-routed MCP calls are
        // otherwise unbounded: a hung server would strand the session's tool
        // batch in `Sending` forever. On elapse the hung future is dropped
        // with the connection left intact and a failed result is published so
        // the pending batch self-completes. `0` disables the ceiling.
        let timeout_secs = self
            .deps
            .services
            .user_preferences_storage
            .read()
            .tool_default_timeout_secs;
        let call = client.call_tool(&tool_name, arguments);
        let result = if timeout_secs == 0 {
            let call_result = call.await;
            build_call_result(&tool_call, call_result, max_output_lines, max_output_bytes)
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), call).await {
                Ok(call_result) => {
                    build_call_result(&tool_call, call_result, max_output_lines, max_output_bytes)
                }
                Err(_) => {
                    tracing::warn!(
                        session_id = %session_id,
                        tool = %tool_call.name,
                        timeout_secs,
                        "MCP tool call exceeded the safety ceiling; failing the call"
                    );
                    failure_result(
                        &tool_call,
                        format!(
                            "MCP tool `{}` timed out after {timeout_secs}s (tool_default_timeout_secs)",
                            tool_call.name
                        ),
                    )
                }
            }
        };

        self.deps
            .publish(ToolExecutionCompleted { session_id, result })
            .await;
    }
}

/// Deterministic post-startup query: is this actor holding a live client?
///
/// Used by `McpCoordinatorActor::restart_one` after `wait_for_startup` to learn
/// whether the newly-spawned actor connected successfully, *without* relying on
/// bus-event ordering (the old status-event approach was race-prone).
pub struct ConnectionState;

impl Message<ConnectionState> for McpActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        _msg: ConnectionState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> bool {
        self.client.is_some()
    }
}

/// Parses a tool call's JSON arguments string into an MCP `JsonObject`.
///
/// Returns `Ok(None)` for an empty/blank argument string (valid — the tool
/// takes no arguments) and `Err(message)` for malformed JSON.
fn parse_arguments(raw: &str) -> Result<Option<JsonObject>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid arguments: {e}"))?;
    match value {
        serde_json::Value::Object(map) => Ok(Some(map)),
        other => Err(format!("expected JSON object for arguments, got {other}")),
    }
}

/// Builds a failed [`ToolResult`] with the given message.
fn failure_result(tool_call: &ToolCall, message: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: tool_call.id.clone(),
        name: tool_call.name.clone(),
        content: message.into(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Maps a raw `tools/call` outcome to a [`ToolResult`], preserving the
/// error-message shape of the failure path.
fn build_call_result(
    tool_call: &ToolCall,
    call_result: Result<CallToolResult, error_stack::Report<McpClientError>>,
    max_output_lines: Option<usize>,
    max_output_bytes: Option<usize>,
) -> ToolResult {
    match call_result {
        Ok(mcp_result) => {
            let success = !mcp_result.is_error.unwrap_or(false);
            let content = format_result_content(&mcp_result);
            build_result(
                tool_call,
                &content,
                success,
                max_output_lines,
                max_output_bytes,
            )
        }
        Err(report) => {
            tracing::warn!(error = %report, "MCP tools/call failed");
            failure_result(tool_call, format!("MCP tool call failed: {report}"))
        }
    }
}

/// Builds a (possibly truncated) [`ToolResult`] from raw MCP output content.
///
/// Applies the same tail-truncation as the `bash` tool: large MCP results
/// are bounded by `max_output_lines`/`max_output_bytes` (falling back to the
/// shared defaults when the orchestrator sent `None`), and the original
/// content is preserved in `full_content` when truncation occurred.
fn build_result(
    tool_call: &ToolCall,
    content: &str,
    success: bool,
    max_output_lines: Option<usize>,
    max_output_bytes: Option<usize>,
) -> ToolResult {
    let max_lines = max_output_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = max_output_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let truncated = truncate_tail(content, max_lines, max_bytes);
    ToolResult {
        tool_call_id: tool_call.id.clone(),
        name: tool_call.name.clone(),
        content: truncated.content,
        success,
        full_content: truncated.truncated.then(|| content.to_owned()),
        truncation: truncated.meta,
        pin_position: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test assertions"
    )]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn format_result_content_joins_text_blocks() {
        // Given a CallToolResult with two text blocks.
        let result = jinn_mcp::testkit::ok_result(vec![
            ContentBlock::text("line one"),
            ContentBlock::text("line two"),
        ]);

        // When formatting.
        let out = format_result_content(&result);

        // Then the text blocks are newline-joined.
        assert_eq!(out, "line one\nline two");
    }

    #[rstest::rstest]
    #[test]
    fn format_result_content_summarizes_non_text_blocks() {
        // Given a CallToolResult with an image block.
        let result = jinn_mcp::testkit::ok_result(vec![
            ContentBlock::text("before"),
            ContentBlock::image("data", "image/png"),
        ]);

        // When formatting.
        let out = format_result_content(&result);

        // Then non-text blocks are summarized as placeholders.
        assert_eq!(out, "before\n[image content]");
    }

    #[rstest::rstest]
    #[test]
    fn parse_arguments_empty_string_is_none() {
        // Given a blank arguments string.
        // When parsing.
        let result = parse_arguments("   ");

        assert!(result.is_ok_and(|opt| opt.is_none()));
    }

    #[rstest::rstest]
    #[test]
    fn parse_arguments_object_is_some() {
        // Given a JSON object arguments string.
        // When parsing.
        let result = parse_arguments(r#"{"key": "value"}"#).expect("parse");

        // Then it is Some with the object.
        let map = result.expect("some");
        assert_eq!(map.get("key").and_then(|v| v.as_str()), Some("value"));
    }

    #[rstest::rstest]
    #[test]
    fn parse_arguments_non_object_is_error() {
        // Given a JSON array arguments string.
        // When parsing.
        let result = parse_arguments("[1, 2, 3]");

        // Then it is an error.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn server_command_maps_config_fields() {
        // Given a server config.
        let config = McpServerConfig {
            command: Some("npx".to_owned()),
            args: vec!["@excalimate/mcp-server".to_owned(), "--stdio".to_owned()],
            ..Default::default()
        };

        // When building the command.
        let cmd = server_command(&config);

        // Then the program and args are copied.
        assert_eq!(cmd.program, "npx");
        assert_eq!(cmd.args, vec!["@excalimate/mcp-server", "--stdio"]);
    }

    /// Verifies tool-name filtering strips the namespace correctly.
    #[rstest::rstest]
    #[test]
    fn strip_namespace_roundtrip() {
        // Given a namespaced tool name for "excalimate".
        let namespaced = "mcp__excalimate__create_scene";

        // When stripping the namespace.
        let stripped = strip_namespace("excalimate", namespaced);

        // Then the original server-side name is recovered.
        assert_eq!(stripped, Some("create_scene"));
    }

    /// Verifies a different server's prefix does not match.
    #[rstest::rstest]
    #[test]
    fn strip_namespace_rejects_other_server() {
        // Given a namespaced tool name for "excalimate".
        let namespaced = "mcp__excalimate__create_scene";

        // When stripping with a different server name.
        let stripped = strip_namespace("other", namespaced);

        // Then it does not match.
        assert_eq!(stripped, None);
    }

    /// Ensures `provider_name` matches the prefix used for stripping.
    #[rstest::rstest]
    #[test]
    fn provider_name_matches_prefix() {
        // Given a server name.
        // When computing provider name and prefix.
        let provider = provider_name("excalimate");

        // Then the prefix is consistent with strip_namespace.
        assert!(strip_namespace("excalimate", &format!("{provider}create_scene")).is_some());
    }

    /// Ensures map_tool namespaces the tool name.
    #[rstest::rstest]
    #[test]
    fn map_tool_namespaces_name() {
        // Given an rmcp Tool.
        let mcp_tool =
            jinn_mcp::Tool::new("create_scene", "Create a scene", serde_json::Map::new());

        // When mapping.
        let def = map_tool("excalimate", &mcp_tool);

        // Then the name is namespaced.
        assert_eq!(def.name, "mcp__excalimate__create_scene");
        assert_eq!(def.description, "Create a scene");
    }

    fn sample_tool_call() -> ToolCall {
        ToolCall {
            id: "tc_x".to_owned(),
            name: "mcp__stub__echo".to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn build_result_passes_small_content_through_unchanged() {
        // Given content well within limits.
        let tool_call = sample_tool_call();

        // When building the result.
        let result = build_result(&tool_call, "hello", true, Some(100), Some(1024));

        // Then no truncation occurred.
        assert!(result.success);
        assert_eq!(result.content, "hello");
        assert!(result.full_content.is_none());
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn build_result_truncates_large_content_by_lines() {
        // Given five lines of content and a three-line limit.
        let tool_call = sample_tool_call();
        let content = "line1\nline2\nline3\nline4\nline5".to_owned();

        // When building the result with a 3-line limit.
        let result = build_result(&tool_call, &content, true, Some(3), Some(1024));

        // Then the tail is kept and the full content is preserved.
        assert_eq!(result.content, "line3\nline4\nline5");
        assert_eq!(result.full_content.as_deref(), Some(content.as_str()));
        let meta = result.truncation.expect("meta");
        assert_eq!(meta.total_lines, 5);
        assert_eq!(meta.output_lines, 3);
    }

    #[rstest::rstest]
    #[test]
    fn build_result_truncates_large_content_by_bytes() {
        // Given a single large line exceeding the byte limit.
        let tool_call = sample_tool_call();
        let content = "x".repeat(500);

        // When building the result with a 100-byte limit.
        let result = build_result(&tool_call, &content, true, Some(100), Some(100));

        // Then the content is bounded and metadata records byte truncation.
        assert!(result.content.len() <= 100);
        assert!(result.truncation.is_some());
        assert_eq!(result.full_content.as_deref(), Some(content.as_str()));
    }

    #[rstest::rstest]
    #[test]
    fn build_result_uses_defaults_when_limits_are_none() {
        // Given content under the default limits.
        let tool_call = sample_tool_call();

        // When building with None limits.
        let result = build_result(&tool_call, "small", true, None, None);

        // Then it passes through unchanged (defaults are generous).
        assert_eq!(result.content, "small");
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn next_tail_returns_none_when_unchanged() {
        // Given an unchanged tail.
        // When checking against the last-published value.
        // Then no new publish is needed.
        assert_eq!(next_tail("hello", "hello"), None);
    }

    #[rstest::rstest]
    #[test]
    fn next_tail_publishes_when_tail_grew() {
        // Given a tail that grew since the last publish.
        // When checking against the last-published value.
        // Then the new tail is returned for publishing.
        assert_eq!(
            next_tail("hello world", "hello"),
            Some("hello world".to_owned())
        );
    }

    #[rstest::rstest]
    #[test]
    fn next_tail_publishes_first_tail_from_empty_start() {
        // Given the first non-empty tail (startup).
        // When checking against the empty last-published value.
        // Then the tail is returned for publishing.
        assert_eq!(next_tail("npm warn", ""), Some("npm warn".to_owned()));
    }
}
