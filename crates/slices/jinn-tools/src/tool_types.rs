//! Tool execution context.
//!
//! [`ToolDefinition`], [`ToolCall`], and [`ToolResult`] live in
//! `jinn-core-types`; [`ToolContext`] lives here because it depends on kernel
//! types (state and service handles) that only the slice reads.

use std::fmt;
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_core_types::ToolCallId;
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::common::state::State;
use marked_path::{Absolute, MarkedPathBuf};

/// Builds the failed [`ToolResult`] a tool returns when it cannot proceed.
///
/// The `Error: ` prefix is a de-facto wire contract with the model, so it is
/// stamped here once rather than restated by each tool: a tool that spelled the
/// failure differently would be indistinguishable to the model from one that
/// failed for a different reason.
///
/// The optional fields stay `None` — a failure is not truncated and is not
/// pinned, so nothing downstream has to treat it as either.
#[must_use]
pub fn tool_error(call: &ToolCall, msg: &str) -> ToolResult {
    failed_result(&call.id, &call.name, msg)
}

/// [`tool_error`] for a tool that has already destructured its call and holds
/// only the two ids a result needs to answer it.
#[must_use]
pub fn failed_result(tool_call_id: &ToolCallId, name: &str, msg: &str) -> ToolResult {
    ToolResult {
        tool_call_id: tool_call_id.clone(),
        name: name.to_owned(),
        content: format!("Error: {msg}"),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Builds a failed [`ToolResult`] whose content is passed through verbatim.
///
/// The counterpart to [`tool_error`], and the reason the two are separate: a
/// few tools build their own failure text and must not have `Error: ` stamped
/// onto it a second time. `bash` and `grep` phrase a failure as a sentence
/// about the command (`"command is empty"`, `"pattern is empty"`), and the
/// command-policy denial reads as a block reason rather than an error
/// message. Prepending a prefix to those would double it up and change what
/// the model sees.
///
/// This is *not* the wire contract [`tool_error`] establishes. The optional
/// fields stay `None` for the same reason — a failure is neither truncated nor
/// pinned — but `content` here means exactly what the caller passed, so a
/// caller that omits `Error: ` ships a failure the model reads as a plain
/// result string. Use [`tool_error`] unless the tool owns its phrasing.
#[must_use]
pub fn unprefixed_failure(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    failed(call.id.clone(), call.name.clone(), content)
}

/// [`unprefixed_failure`] for a tool that has already destructured its call.
///
/// `bash` and `grep` own the `ToolCall` outright by the point they fail —
/// it has been moved into their process orchestration — so they hold the two
/// ids rather than the call. `restart_mcp` likewise holds the ids, because
/// its failure text is built from a coordinator reply it has already
/// destructured.
#[must_use]
pub fn failed(
    tool_call_id: impl Into<jinn_core_types::ToolCallId>,
    name: impl Into<String>,
    content: impl Into<String>,
) -> ToolResult {
    ToolResult {
        tool_call_id: tool_call_id.into(),
        name: name.into(),
        content: content.into(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Mints a turn identity for test fixtures, the way the ledger's mint does.
///
/// Test-only: every fixture that constructs a turn-carrying event or a
/// [`ToolContext`] stamps this instead of a hand-written counter, so the
/// fixture's shape matches what production's dispatch path produces.
#[cfg(test)]
#[must_use]
pub fn test_turn() -> jinn_core_types::turn_identity::TurnToken {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    jinn_core_types::turn_identity::TurnToken::from_counter(SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

/// Context provided to every built-in tool at execution time.
///
/// Constructed by the tool orchestrator at dispatch time from session state.
/// Contains the session's CWD (for resolving relative paths), an optional
/// execution timeout, and an optional message sink for emitting streaming events.
#[derive(Clone)]
pub struct ToolContext {
    /// Working directory for resolving relative paths.
    pub cwd: MarkedPathBuf<Absolute>,
    /// Optional execution timeout.
    pub timeout: Option<Duration>,
    /// Shared application state (only available for tools that need it).
    pub state: Option<State>,
    /// The live configuration layer, so a tool reads `jinn.toml` at the
    /// point of use rather than through a value baked at registration.
    pub config: jinn_config::ConfigLayer,
    /// The shells jinn runs commands through, resolved once at launch.
    ///
    /// A tool that spawns a shell reads it from here rather than
    /// resolving one itself: the environment is probed at startup, and a
    /// tool that re-probed it would consult the environment outside
    /// program initialization.
    pub shells: jinn_common::shell::Shells,
    /// Session ID (only available for tools that need it).
    pub session_id: Option<SessionId>,
    /// Application filesystem paths (for tools that need filesystem access).
    pub app_paths: jinn_common::app_paths::AppPaths,
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
    /// The turn this execution belongs to, as minted by the turn ledger.
    /// Carried from `SendToLlmProvider` through the tool execution chain
    /// so streamed tool events are stamped with the turn they belong to.
    pub turn: jinn_core_types::turn_identity::TurnToken,
    /// MCP coordinator actor ref — `Some` only for the `restart_mcp_server`
    /// tool, which `ask`s the coordinator directly (request/reply) to learn
    /// whether a restart connected. Resolved from
    /// `services.mcp_coordinator` at dispatch time. `None` in tests and for
    /// every tool that doesn't need it.
    pub mcp_coordinator: Option<std::sync::Arc<dyn jinn_mcp_msg::McpCoordinatorHandle>>,
    /// Canvas serving handle — `Some` only for the `canvas` tool, which
    /// asks the canvas actor to serve (or re-target) an artifact and
    /// relays the resolved URL. Resolved from `services.canvas_server`
    /// at dispatch time. `None` in tests and for every tool that
    /// doesn't need it.
    pub canvas_server: Option<std::sync::Arc<dyn jinn_canvas_msg::CanvasServerHandle>>,
    /// Interactive-term coordinator handle — `Some` only for the
    /// `interactive_term*` tools, which ask the coordinator to spawn/drive
    /// PTY sessions. Resolved from `services.interactive_term` at dispatch
    /// time. `None` in tests and for every tool that doesn't need it.
    pub interactive_term: Option<std::sync::Arc<dyn jinn_term_msg::TermHandle>>,
    /// In-flight subagent spawn registry — read by the stall watchdog to
    /// skip sessions suspended on a `spawn_subagent` call, and written by the `spawn_subagent`
    /// tool through its drop-guard. `None` in tests.
    pub task_spawns: Option<jinn_tools_msg::TaskSpawnRegistry>,
    /// Session store — `Some` only for the `session_search`/`session_fetch`
    /// tools, which read persisted history across all sessions. Resolved
    /// from `services.session_store` at dispatch time. `None` in tests that
    /// build a bare `ToolContext`.
    pub session_store: Option<jinn_session_state::SessionStoreService>,
    /// The trouper fabric — `Some` only for the `spawn_subagent` tool, which spawns
    /// its phase/settle listeners onto it. Resolved from
    /// `services.trouper_system` at dispatch time. `None` in tests that
    /// build a bare `ToolContext` (the `spawn_subagent` tool fails fast then).
    pub trouper_system: Option<trouper::system::ActorSystem>,
    /// Model-name resolver — `Some` only for the `spawn_subagent` tool, which
    /// validates and resolves its `model` argument against the provider
    /// registry through it before spawning the child session. Resolved
    /// from `services.provider_registry` at dispatch time. `None` in
    /// tests that build a bare `ToolContext` (the `spawn_subagent` tool then skips
    /// validation and treats any given name as final).
    pub model_name_resolver: Option<std::sync::Arc<dyn jinn_core_types::ModelNameResolver>>,
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

    #[rstest::rstest]
    fn tool_context_debug_contains_cwd_and_timeout() {
        // Given a ToolContext with known values.
        let dir = jinn_testutil::tempdir();
        let cwd = dir.path.join("test").expect("temp root joins a bare name");
        let ctx = ToolContext {
            cwd: cwd.clone(),
            timeout: Some(std::time::Duration::from_secs(30)),
            state: None,
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            session_id: Some(jinn_core_types::SessionId::new()),
            app_paths: jinn_common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            turn: crate::tool_types::test_turn(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        };

        // When debugging.
        let debug_str = format!("{ctx:?}");

        // Then the output contains cwd, timeout, and session_id. The cwd is
        // compared in its Debug form: `PathBuf`'s Debug escapes the
        // separator, so a Windows path appears as `C:\\Users\\...` while
        // `to_string_lossy` gives `C:\Users\...`.
        assert!(
            debug_str.contains(&format!("{cwd:?}")),
            "debug should contain cwd"
        );
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
