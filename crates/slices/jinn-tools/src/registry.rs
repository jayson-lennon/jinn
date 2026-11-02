//! Built-in tool registry.
//!
//! Wires each tool module into a list of (definition, execute) pairs
//! for registration by the tool orchestrator at activation.

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition};

use super::{
    BoxedToolFuture, bash, bot_tools, canvas, capabilities, edit, get_time, grep, interactive_term,
    interactive_term_kill, interactive_term_send, load_bot, read, read_image, restart_mcp,
    run_lifecycle, save_plan, session_fetch, session_search, skill, spawn_subagent, tell_session,
    write,
};

/// A built-in tool entry: its definition paired with its execute function,
/// plus whether the tool manages its own deadline.
///
/// `self_managed_timeout` must be `true` for tools that outlive a dropped
/// future: the dispatcher's outer timeout wrapper *aborts* its inner future
/// on expiry, which for a tool like `spawn_subagent` would orphan the spawned child.
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
/// `default_timeout_secs` is a *seed* only, for the shape of the registry
/// at construction. `bash`'s schema advertises the timeout, and that
/// advertised value is re-resolved from the configuration layer on every
/// publish — see [`BuiltinToolEntry`]'s live definition slot — so a reload
/// changes the schema the model actually sees.
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
            read_image::definition(),
            read_image::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
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
            canvas::definition(),
            canvas::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            capabilities::query_definition(),
            capabilities::query_execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            capabilities::apply_definition(),
            capabilities::apply_execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            bot_tools::conclude_definition(),
            bot_tools::conclude_execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            bot_tools::notify_parent_definition(),
            bot_tools::notify_parent_execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            spawn_subagent::definition(),
            spawn_subagent::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            // `spawn_subagent` blocks on a child session; the dispatcher's outer
            // timeout would drop its future and orphan the child.
            true,
        ),
        (
            tell_session::definition(),
            tell_session::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            load_bot::definition(),
            load_bot::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            run_lifecycle::definition(),
            run_lifecycle::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            // The tool awaits the script's completion event; the dispatcher's
            // outer timeout would abort that wait mid-script.
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

    // The PowerShell tool is Windows-only: on Linux and macOS the model
    // must not see a tool it cannot run.
    #[cfg(windows)]
    entries.push((
        crate::powershell::definition(default_timeout_secs),
        crate::powershell::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
        false,
    ));

    entries.extend(crate::todo_tools::tool_entries());

    entries
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use super::builtin_tools;

    #[rstest::rstest]
    #[test]
    fn capabilities_tools_are_registered() {
        // Given the builtin tool list.
        let tools = builtin_tools(30);

        // When collecting the registered tool names.
        let names: Vec<&str> = tools.iter().map(|(d, _, _)| d.name.as_str()).collect();

        // Then both capability adjustment tools are present, query before
        // apply as the registry lists them.
        let query_pos = names
            .iter()
            .position(|n| *n == "capabilities_query")
            .expect("capabilities_query must be registered");
        let apply_pos = names
            .iter()
            .position(|n| *n == "capabilities_apply")
            .expect("capabilities_apply must be registered");
        assert!(query_pos < apply_pos);
    }

    #[cfg(windows)]
    #[rstest::rstest]
    #[test]
    fn the_powershell_tool_is_registered_on_windows() {
        // Given the builtin tool list.
        let tools = builtin_tools(30);

        // When collecting the registered tool names.
        let names: Vec<&str> = tools.iter().map(|(d, _, _)| d.name.as_str()).collect();

        // Then the powershell tool is present.
        assert!(
            names.contains(&"powershell"),
            "the powershell tool must be registered on Windows; got: {names:?}"
        );
    }

    #[cfg(not(windows))]
    #[rstest::rstest]
    #[test]
    fn the_powershell_tool_is_not_registered_off_windows() {
        // Given the builtin tool list.
        let tools = builtin_tools(30);

        // When collecting the registered tool names.
        let names: Vec<&str> = tools.iter().map(|(d, _, _)| d.name.as_str()).collect();

        // Then the powershell tool is absent: the model must not see a
        // tool it cannot run.
        assert!(
            !names.contains(&"powershell"),
            "the powershell tool must not be registered off Windows; got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn restart_mcp_server_is_registered() {
        // Given the builtin tool list.
        let tools = builtin_tools(30);

        // When collecting the registered tool names.
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
            "read_image",
            "write",
            "edit",
            "skill",
            "save_plan",
            "session_search",
            "session_fetch",
            "capabilities_query",
            "capabilities_apply",
            "tell_session",
            "load_bot",
            "run_lifecycle",
            "grep",
            "spawn_subagent",
            "canvas",
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
