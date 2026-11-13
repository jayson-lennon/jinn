// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Tests for the inspector's live tool projection.

#![allow(
    clippy::indexing_slicing,
    reason = "test code: fixture vectors with known lengths"
)]

#[rstest::rstest]
#[test]
fn tools_for_collects_only_the_named_servers_tools() {
    // Given tool definitions from this server, another server, and a builtin.
    let definitions = vec![
        tool_def("mcp__excalimate__create_scene", "Create a scene"),
        tool_def("mcp__excalimate__auto_animate", "Auto-animate"),
        tool_def("mcp__other__create_scene", "Other server"),
        tool_def("file_read", "A builtin"),
    ];

    // When projecting the tools for "excalimate".
    let tools = super::mcp_picker_actions::tools_for("excalimate", &definitions);

    // Then only excalimate's tools are collected, with prefixes stripped.
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].0, "create_scene");
    assert_eq!(tools[0].1, "Create a scene");
    assert_eq!(tools[1].0, "auto_animate");
}

#[rstest::rstest]
#[test]
fn tools_for_returns_empty_when_no_definition_matches() {
    // Given definitions with no matching prefix.
    let definitions = vec![tool_def("file_read", "builtin")];

    // When projecting the tools for an unknown server.
    let tools = super::mcp_picker_actions::tools_for("ghost", &definitions);

    // Then no tools are collected.
    assert_eq!(tools, [] as [(std::string::String, std::string::String); 0]);
}

/// A minimal tool definition the projection tests only need a shape for.
fn tool_def(name: &str, description: &str) -> jinn_core_types::ToolDefinition {
    jinn_core_types::ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: serde_json::Value::Object(serde_json::Map::new()),
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
        server_tool_type: None,
    }
}

#[rstest::rstest]
#[test]
fn registered_mcp_defs_collect_only_the_mcp_prefixed_definitions() {
    // Given a registry mixing built-ins, an mcp server's tools, and another
    // prefix that merely begins with `mcp_`.
    let defs = vec![
        tool_def("read", "builtin"),
        tool_def("mcp__excalimate__draw", "Draw"),
        tool_def("mcp__excalimate__shapes", "Shapes"),
        tool_def("mcp_server", "not an mcp tool"),
    ];

    // When projecting the registered MCP definitions.
    let mcp = super::mcp_picker_actions::registered_mcp_defs(&defs);

    // Then only the `mcp__`-prefixed definitions survive.
    let names: Vec<&str> = mcp.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mcp__excalimate__draw", "mcp__excalimate__shapes"]
    );
}

#[rstest::rstest]
#[test]
fn refresh_cost_figure_counts_registered_tools_only() {
    // Given a session registry holding one enabled server's two tools and a
    // built-in; a disabled server's tools would not be registered at all.
    let defs = vec![
        tool_def("read", "builtin"),
        tool_def("mcp__a__one", "First"),
        tool_def("mcp__a__two", "Second"),
    ];

    // When refreshing the figure.
    let mut state = jinn_mcp_msg::McpPickerState::default();
    super::mcp_picker_actions::refresh_cost_figure(&mut state, &defs);

    // Then the figure equals the schema cost of the two MCP tools — and no
    // built-in is folded in.
    let counter = jinn_llm_support::token_estimator::TiktokenCounter::o200k_base();
    let expected = jinn_llm_support::token_estimator::toolset_context_tokens(
        &[
            tool_def("mcp__a__one", "First"),
            tool_def("mcp__a__two", "Second"),
        ],
        &counter,
    );
    assert_eq!(state.context_tokens, expected);
    assert!(state.context_tokens > 0);
    let names: Vec<&str> = state
        .context_definitions
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, vec!["mcp__a__one", "mcp__a__two"]);
}

#[rstest::rstest]
#[test]
fn refresh_cost_figure_skips_an_unchanged_registry() {
    // Given a state whose figure was computed from a registry.
    let defs = vec![tool_def("mcp__a__one", "First")];
    let mut state = jinn_mcp_msg::McpPickerState::default();
    super::mcp_picker_actions::refresh_cost_figure(&mut state, &defs);
    let figure = state.context_tokens;

    // When refreshing again over the same registry.
    super::mcp_picker_actions::refresh_cost_figure(&mut state, &defs);

    // Then the figure is stable and the signature guard held.
    assert_eq!(state.context_tokens, figure);

    // When the registry gains a tool.
    let grown = vec![
        tool_def("mcp__a__one", "First"),
        tool_def("mcp__a__two", "Second"),
    ];

    // Then the next refresh picks the growth up — the figure follows the
    // live registry, not the open-time state.
    super::mcp_picker_actions::refresh_cost_figure(&mut state, &grown);
    assert!(state.context_tokens > figure);
}
