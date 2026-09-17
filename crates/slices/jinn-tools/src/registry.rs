//! Built-in tool registry.
//!
//! Wires each tool module into a list of (definition, execute) pairs
//! for registration by the tool orchestrator at activation.

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition};

use super::{
    BoxedToolFuture, bash, edit, get_time, grep, interactive_term, interactive_term_kill,
    interactive_term_send, read, restart_mcp, save_plan, session_fetch, session_search, skill,
    task, write,
};

/// A built-in tool entry: its definition paired with its execute function,
/// plus whether the tool manages its own deadline.
///
/// `self_managed_timeout` must be `true` for tools that outlive a dropped
/// future: the dispatcher's outer timeout wrapper *aborts* its inner future
/// on expiry, which for a tool like `task` would orphan the spawned child.
/// A self-managed tool receives the per-call `max_duration_secs` (peeked via
/// `extract_max_duration`) inside its `ToolContext.timeout` instead, and is
/// responsible for enforcing (or ignoring) that budget itself.
pub type BuiltinToolEntry = (
    ToolDefinition,
    fn(ToolCall, ToolContext) -> BoxedToolFuture,
    bool,
);

/// Returns the built-in tool definitions and their execute functions.
///
/// `default_timeout_secs` flows into `bash::definition` so the resolved global timeout
/// is surfaced in the schema the model sees.
pub fn builtin_tools(default_timeout_secs: u64) -> Vec<BuiltinToolEntry> {
    let mut entries = vec![
        (
            get_time::definition(),
            get_time::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            bash::definition(default_timeout_secs),
            bash::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            read::definition(),
            read::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            write::definition(),
            write::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            edit::definition(),
            edit::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            skill::definition(),
            skill::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            save_plan::definition(),
            save_plan::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            session_search::definition(),
            session_search::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            session_fetch::definition(),
            session_fetch::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            restart_mcp::definition(),
            restart_mcp::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            task::definition(),
            task::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            // `task` blocks on a child session; the dispatcher's outer
            // timeout would drop its future and orphan the child.
            true,
        ),
        (
            grep::definition(),
            grep::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            interactive_term::definition(),
            interactive_term::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            interactive_term_send::definition(),
            interactive_term_send::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            interactive_term_kill::definition(),
            interactive_term_kill::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
    ];
    entries.extend(crate::todo_tools::tool_entries());

    entries
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use super::builtin_tools;

    #[rstest::rstest]
    #[test]
    fn restart_mcp_server_is_registered() {
        // Given the builtin tool list.
        let tools = builtin_tools(30);
        let names: Vec<&str> = tools.iter().map(|(d, _, _)| d.name.as_str()).collect();

        // Then restart_mcp_server is present alongside the existing builtins.
        assert!(
            names.contains(&"restart_mcp_server"),
            "restart_mcp_server must be registered; got: {names:?}"
        );
        for required in [
            "get_time",
            "bash",
            "read",
            "write",
            "edit",
            "skill",
            "save_plan",
            "session_search",
            "session_fetch",
            "grep",
            "task",
            "interactive_term",
            "interactive_term_send",
            "interactive_term_kill",
        ] {
            assert!(
                names.contains(&required),
                "existing builtin {required} must still be present; got: {names:?}"
            );
        }
    }
}
