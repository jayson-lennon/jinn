//! Tool calling types - execution context.
//!
//! [`ToolDefinition`], [`ToolCall`], and [`ToolResult`] are defined in the
//! `jinn-provider` crate and re-exported here for convenience.
//! [`ToolContext`] remains in this module because it depends on domain types.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::protocol::SessionId;

// Re-export provider types.
pub use jinn_provider::tool_types::ToolResultPinPosition;
pub use jinn_provider::{ToolCall, ToolDefinition, ToolResult};

/// Context provided to every built-in tool at execution time.
///
/// Constructed by the tool orchestrator at dispatch time from session state.
/// Contains the session's CWD (for resolving relative paths), an optional
/// execution timeout, and an optional message sink for emitting streaming events.
#[derive(Clone)]
pub struct ToolContext {
    /// Working directory for resolving relative paths.
    pub cwd: PathBuf,
    /// Compiled blocked-command rules for the project containing [`Self::cwd`],
    /// if any. Only `bash` consults it; an empty policy matches nothing.
    pub command_policy: crate::feat::tools_actor::command_policy::CompiledCommandPolicy,
    /// Optional execution timeout.
    pub timeout: Option<Duration>,
    /// Shared application state (only available for tools that need it).
    pub state: Option<State>,
    /// Session ID (only available for tools that need it).
    pub session_id: Option<SessionId>,
    /// Application filesystem paths (for tools that need filesystem access).
    pub app_paths: crate::common::app_paths::AppPaths,
    /// Bus service for emitting streaming events.
    ///
    /// Only set for tools that need to emit incremental output events
    /// (e.g., bash streaming). When `None`, the tool runs silently
    /// and returns a single `ToolResult`.
    pub bus: Option<BusService>,
    /// Maximum lines for tool output truncation. `None` uses built-in default.
    pub max_output_lines: Option<usize>,
    /// Maximum bytes for tool output truncation. `None` uses built-in default.
    pub max_output_bytes: Option<usize>,
    /// When the original LLM request was dispatched. Carried from
    /// `SendToLlmProvider` through the tool execution chain so tool
    /// events can carry accurate timing.
    pub dispatched_at: jiff::Timestamp,
    /// Authority to write session state (task lists, skill installs).
    /// Only present for tools that mutate sessions.
    pub session_cap: Option<crate::common::tcaps::session::SessionCap>,
    /// MCP coordinator actor ref — `Some` only for the `restart_mcp_server`
    /// tool, which `ask`s the coordinator directly (request/reply) to learn
    /// whether a restart connected. Resolved from
    /// `services.mcp_coordinator` at dispatch time. `None` in tests and for
    /// every tool that doesn't need it.
    pub mcp_coordinator: Option<std::sync::Arc<dyn jinn_mcp_msg::McpCoordinatorHandle>>,
    /// Interactive-term coordinator handle — `Some` only for the
    /// `interactive_term*` tools, which ask the coordinator to spawn/drive
    /// PTY sessions. Resolved from `services.interactive_term` at dispatch
    /// time. `None` in tests and for every tool that doesn't need it.
    pub interactive_term: Option<std::sync::Arc<dyn jinn_term_msg::TermHandle>>,
    /// In-flight subagent spawn registry — read by the stall watchdog to
    /// skip sessions suspended on a `task` call, and written by the `task`
    /// tool through its drop-guard. `None` in tests.
    pub task_spawns: Option<crate::feat::tools_actor::task_registry::TaskSpawnRegistry>,
    /// Session store — `Some` only for the `session_search`/`session_fetch`
    /// tools, which read persisted history across all sessions. Resolved
    /// from `services.session_store` at dispatch time. `None` in tests that
    /// build a bare `ToolContext`.
    pub session_store: Option<crate::feat::session::session_store::SessionStoreService>,
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use std::path::PathBuf;

    #[rstest::rstest]
    fn tool_context_debug_contains_cwd_and_timeout() {
        // Given a ToolContext with known values.
        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp/test"),
            command_policy:
                crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
            timeout: Some(std::time::Duration::from_secs(30)),
            state: None,
            session_id: Some(crate::protocol::SessionId::new()),
            app_paths: crate::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        };

        // When debugging.
        let debug_str = format!("{ctx:?}");

        // Then the output contains cwd, timeout, and session_id.
        assert!(debug_str.contains("/tmp/test"), "debug should contain cwd");
        assert!(
            debug_str.contains("timeout"),
            "debug should contain timeout"
        );
        assert!(
            debug_str.contains("session_id"),
            "debug should contain session_id"
        );
        assert!(
            debug_str.contains("ToolContext"),
            "debug should contain struct name"
        );
    }
}
