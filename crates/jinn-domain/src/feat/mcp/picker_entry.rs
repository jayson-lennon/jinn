//! MCP server picker entry type.

use crate::feat::mcp_actor::protocol::McpConnectionStatus;
use crate::feat::theme::Theme;
use jinn_provider::ToolDefinition;

/// An MCP server entry ready for display in the MCP inspector.
///
/// Mirrors [`ToolEntry`](crate::feat::tools_actor::tool_entry::ToolEntry): a
/// name plus a dim description, with a ✓/✗ marker showing the per-session
/// enabled state. Rendering lives in the picker spec
/// ([`mcp_server_spec`](crate::feat::picker::mcp_server_spec)); this type is
/// pure data.
#[derive(Debug, Clone)]
pub struct McpServerEntry {
    /// Server name (the `[[mcp_server]].name`, unique per `jinn.toml`).
    pub name: String,
    /// Human-readable launch summary (e.g. `"npx @excalimate/mcp-server"`).
    pub description: String,
    /// Whether this server is enabled for the active session.
    pub enabled: bool,
    /// Theme for styling.
    pub theme: Theme,
    /// Live connection status (Starting/Running/Dead) for the preview's
    /// status badge. `None` when disabled or not yet seen.
    pub status: Option<McpConnectionStatus>,
    /// Captured stderr tail for the logs preview pane.
    pub stderr_tail: String,
    /// Tools advertised by this server, namespaced + stripped to
    /// `(local_name, description)` pairs for the tools preview pane.
    pub tools: Vec<(String, String)>,
    /// Which preview pane is shown: logs (status + stderr) or tools.
    pub preview_mode: McpPreviewMode,
}

/// Toggles the MCP server preview pane between logs and tools.
///
/// Defaults to [`McpPreviewMode::Logs`] so the user sees server health
/// (status badge + stderr) first; they flip to [`McpPreviewMode::Tools`]
/// to inspect the advertised tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpPreviewMode {
    /// Status badge + live stderr tail.
    #[default]
    Logs,
    /// One line per advertised tool (`name — description`).
    Tools,
}

impl McpServerEntry {
    /// Builds an entry from a server name, launch description, and enabled flag.
    #[must_use]
    pub fn new(name: String, description: String, enabled: bool, theme: Theme) -> Self {
        Self {
            name,
            description,
            enabled,
            theme,
            status: None,
            stderr_tail: String::new(),
            tools: Vec::new(),
            preview_mode: McpPreviewMode::default(),
        }
    }
}

/// A live snapshot of one MCP server's inspectable state, computed from the
/// active session's maps + tool definitions.
///
/// Pure helper: given read-only inputs it returns the values the preview pane
/// needs. The render path calls this each frame to refresh the selected entry
/// without mutating the stored item list.
///
/// Tools are collected by filtering `defs` for names carrying this server's
/// `mcp__<server>__` prefix, then stripping the prefix to recover the
/// server-side tool name.
#[must_use]
pub fn refresh_snapshot(
    server_name: &str,
    status: Option<McpConnectionStatus>,
    stderr_tail: &str,
    defs: &[ToolDefinition],
) -> (Option<McpConnectionStatus>, String, Vec<(String, String)>) {
    let prefix = jinn_mcp::provider_prefix(server_name);
    let tools = defs
        .iter()
        .filter(|d| d.name.starts_with(&prefix))
        .map(|d| {
            (
                d.name
                    .strip_prefix(prefix.as_str())
                    .unwrap_or(&d.name)
                    .to_owned(),
                d.description.clone(),
            )
        })
        .collect();
    (status, stderr_tail.to_owned(), tools)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::feat::theme::default_theme;

    fn make_entry(name: &str, description: &str, enabled: bool) -> McpServerEntry {
        McpServerEntry::new(
            name.to_owned(),
            description.to_owned(),
            enabled,
            default_theme(),
        )
    }

    fn tool_def(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: desc.to_owned(),
            parameters: serde_json::Value::Object(serde_json::Map::new()),
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
            server_tool_type: None,
        }
    }

    #[rstest::rstest]
    fn refresh_snapshot_filters_and_strips_prefix() {
        // Given tool defs mixing this server, another server, and a builtin.
        let defs = vec![
            tool_def("mcp__excalimate__create_scene", "Create a scene"),
            tool_def("mcp__excalimate__auto_animate", "Auto-animate"),
            tool_def("mcp__other__create_scene", "Other server"),
            tool_def("file_read", "A builtin"),
        ];

        // When refreshing the snapshot for "excalimate".
        let (_status, _stderr, tools) = refresh_snapshot("excalimate", None, "", &defs);

        // Then only excalimate's tools are collected, with prefixes stripped.
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].0, "create_scene");
        assert_eq!(tools[0].1, "Create a scene");
        assert_eq!(tools[1].0, "auto_animate");
    }

    #[rstest::rstest]
    fn refresh_snapshot_passes_status_and_stderr_through() {
        // Given a status and stderr tail.
        // When refreshing.
        let (status, stderr, _tools) =
            refresh_snapshot("srv", Some(McpConnectionStatus::Dead), "boom", &[]);

        // Then they pass through unchanged.
        assert_eq!(status, Some(McpConnectionStatus::Dead));
        assert_eq!(stderr, "boom");
    }

    #[rstest::rstest]
    fn refresh_snapshot_no_matching_tools_returns_empty() {
        // Given defs with no matching prefix.
        let defs = vec![tool_def("file_read", "builtin")];

        // When refreshing for an unknown server.
        let (_status, _stderr, tools) = refresh_snapshot("ghost", None, "", &defs);

        // Then no tools are collected.
        assert!(tools.is_empty());
    }

    #[rstest::rstest]
    fn new_entry_defaults_to_logs_preview_mode() {
        // Given a freshly built entry.
        let entry = make_entry("excalimate", "npx ...", true);

        // Then it defaults to the logs pane.
        assert_eq!(entry.preview_mode, McpPreviewMode::Logs);
    }
}
