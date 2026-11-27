//! MCP wire contracts shared between the kernel and the MCP slice.
//!
//! Trouper dispatches by schema id, so every publisher and
//! subscriber must use the *same* type — mirrors would silently drop events
//! (the fabric lesson). These types therefore live in `jinn-slices`, the
//! shared-vocabulary home: the kernel publishes
//! [`McpEnablementChanged`] (session startup seeds, the MCP picker's
//! confirm/ESC-revert) while the `jinn-mcp-slice` crate consumes it and
//! publishes [`McpServerStatus`]/[`McpServerLog`].
//!
//! [`McpCoordinatorHandle`] is the kernel-side seam to the coordinator actor:
//! the actor type is private to the slice, so the kernel's `restart_mcp` tool
//! talks to this trait instead. The slice mints the impl at spawn; an unset
//! handle (tests, de-activated slice) keeps the tool's graceful-error path.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;

use jinn_slices::BusMessage;

/// Restart one (session × server) `McpActor`.
///
/// Sent to the MCP coordinator actor (by the `restart_mcp_server` tool). It
/// kills the currently spawned actor for the pair — if any — and respawns a
/// fresh one, so a wedged server process can be recovered without a full
/// enable/disable toggle through the picker. The respawn only proceeds if the
/// server is still present in the session's `enabled_mcp_servers` set;
/// otherwise it's a no-op.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Restart one MCP server actor for a session.")]
pub struct RestartMcpServer {
    /// The session whose actor should restart.
    pub session_id: SessionId,
    /// The configured server name to restart.
    pub server: String,
}

impl BusMessage for RestartMcpServer {}

/// Outcome of a [`RestartMcpServer`] request, returned by the coordinator.
///
/// `Ok(())` means the newly-spawned actor connected successfully (it holds
/// a live client). The variants explain *why* a restart failed so the
/// caller (the `restart_mcp_server` tool) can report a useful message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartError {
    /// No `[[mcp_server]]` entry matches the requested name.
    UnknownServer,
    /// The new actor spawned but its `on_start` connect failed (dead server,
    /// bad command, etc.). Captured stderr is available in the inspector.
    ConnectFailed,
    /// `on_start` did not complete within the restart timeout — the server is
    /// likely a slow-to-boot JS/Python server still in its startup phase.
    Timeout,
    /// Actor mailbox delivery failure (actor stopped mid-restart).
    Mailbox,
}

/// The set of MCP servers enabled for a session changed.
///
/// Published after the new
/// enabled set is written to the session. Carries the **full** desired set
/// (not a delta): the MCP coordinator diffs this against its spawned-actor
/// map, spawning newly-enabled servers and killing newly-disabled ones.
///
/// This is per-session — each session maintains its own enablement, and each
/// enabled (session × server) pair owns an independent connection actor +
/// child process.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's enabled MCP server set changed.")]
pub struct McpEnablementChanged {
    /// The session whose enablement set changed.
    pub session_id: SessionId,
    /// The full desired set of enabled server names after the change.
    pub enabled: BTreeSet<String>,
}

impl BusMessage for McpEnablementChanged {}

/// Coarse connection state of one connection actor's child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpConnectionStatus {
    /// `on_start` is running: the child process is being spawned / `initialize`
    /// and `tools/list` are in flight.
    Starting,
    /// `tools/list` succeeded; tools are registered and the actor answers
    /// `ExecuteTool` calls.
    Running,
    /// The connection never came up (spawn/initialize/list failed) or has been
    /// shut down. The actor may still be alive but idle; tool calls for this
    /// server return a failed result.
    Dead,
}

/// A connection-status transition for one (session × server) connection actor.
///
/// Published at every transition. Subscribers can build a live view of every
/// MCP process in the app (the sidebar's MCP servers section does).
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "An MCP server's connection state changed.")]
pub struct McpServerStatus {
    /// The session the actor serves.
    pub session_id: SessionId,
    /// The configured server name.
    pub server: String,
    /// The new connection state.
    pub status: McpConnectionStatus,
}

impl BusMessage for McpServerStatus {}

/// Captured stderr tail for one (session × server) connection actor.
///
/// Published whenever new child-process stderr is drained (debounced while
/// Running). The payload is the bounded tail (newest content); subscribers
/// keep a live view for a future log viewer. Published best-effort, alongside
/// status transitions.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "An MCP server produced stderr log output.")]
pub struct McpServerLog {
    /// The session the actor serves.
    pub session_id: SessionId,
    /// The configured server name.
    pub server: String,
    /// The newest captured stderr content (bounded).
    pub tail: String,
}

impl BusMessage for McpServerLog {}

/// The MCP runtime cell's status projection was written.
///
/// Emitted by the coordinator **strictly after** every runtime-cell
/// status write (`McpServerStatus` handling — per-session and
/// singleton alike). This is the push trigger the web UI's outbound actor
/// subscribes, deliberately a distinct schema, never a re-subscription
/// of the raw status facts:
///
/// `McpServerStatus` is an Event with **two** publishers (the
/// connection actors and the coordinator) and **one** cell writer
/// (the coordinator's handlers), so a push riding the raw event could
/// land before the cell holds the transition — and the page's re-pull
/// would read stale rows with no correction coming. The stored event is
/// emitted only where the write happens, which closes exactly the race
/// [`jinn_provider_selection_msg::ModelCacheStored`] was created to
/// close for the model cache.
///
/// The payload carries one thing: `stored_at`, the write's timestamp,
/// stamped by the emitter strictly after the cell write. It is
/// provenance, not content — every consumer still renders by reading
/// the cell (or re-pulling the modal rows built from it), never from
/// the event. The subscriber carries the timestamp on its push so the
/// frame always has a body to deliver.
///
/// Deliberately **not** emitted beside stderr writes
/// (`McpServerLog`): the MCP modal renders no stderr, so a push on
/// log writes would be noise with no rendering consumer.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The MCP runtime cell's status projection was written.")]
pub struct McpRuntimeStored {
    /// When the status write landed in the runtime cell. Read
    /// provenance for signal-only consumers; never a data source.
    pub stored_at: jiff::Timestamp,
}

impl BusMessage for McpRuntimeStored {}

/// Kernel-side seam to the MCP coordinator actor.
///
/// The coordinator actor type is private to the `jinn-mcp-slice` crate; the
/// kernel's `restart_mcp_server` tool talks to this trait instead. The slice
/// mints the impl at spawn (composition sets it into `Services`); an unset
/// handle keeps the tool's graceful-error path.
#[async_trait::async_trait]
pub trait McpCoordinatorHandle: Send + Sync {
    /// Restart one (session × server) connection: kill the current actor —
    /// if any — and respawn a fresh one. The outer timeout that bounds the
    /// ask lives inside the impl, so a hung coordinator still yields
    /// [`RestartError::Timeout`]/[`RestartError::Mailbox`] rather than
    /// hanging the caller.
    async fn restart(&self, session_id: SessionId, server: String) -> Result<(), RestartError>;

    /// Debug name (service-trait convention).
    fn name(&self) -> &'static str {
        "mcp-coordinator"
    }
}
