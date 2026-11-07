//! Tool prompt context builder - assembles "Available tools" and "Tool guidelines"
//! sections for the system prompt from registered tool definitions.
//!
//! Each tool can optionally carry a [`prompt_snippet`](jinn_core_types::ToolDefinition::prompt_snippet)
//! (a one-line summary) and [`prompt_guidelines`](jinn_core_types::ToolDefinition::prompt_guidelines)
//! (behavioral bullet points). This module collects them from all registered tools
//! and formats them into a single string for injection into the system prompt.

use std::collections::BTreeMap;

use jinn_core_types::ToolDefinition;

/// The one-line `dump_to_disk` schema property injected into every mirrored
/// tool definition. Jinn cannot edit an MCP server's schema text, so the
/// reserved argument is added where jinn mirrors every definition — the one
/// chokepoint all schemas pass through.
const DUMP_TO_DISK_PROPERTY: &str = "Optional file path to write this tool's full \
result to; the in-context response becomes a short pointer + preview.";

/// Adds the reserved `dump_to_disk` property to a definition's schema.
///
/// Applied to *copies* at the mirror points (builtin announce, actor-provided
/// registration) — the stored definitions are the dispatch source of truth
/// and are never mutated in place. Definitions whose `parameters` is not a
/// JSON object pass through unchanged.
#[must_use]
pub fn with_reserved_arguments(mut definition: ToolDefinition) -> ToolDefinition {
    let Some(properties) = definition
        .parameters
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return definition;
    };
    properties.insert(
        jinn_core_types::tool_types::DUMP_TO_DISK_ARG.to_owned(),
        serde_json::json!({ "type": "string", "description": DUMP_TO_DISK_PROPERTY }),
    );
    definition
}

/// Builds the tool context block for the system prompt.
///
/// Iterates over all registered tool definitions and collects:
/// - `prompt_snippet` → listed in the "Available tools" section
/// - `prompt_guidelines` → listed as bullet points in the "Tool guidelines" section
///
/// The block closes with the "Reserved tool-call arguments" section: the
/// convention the reserved arguments share (path semantics, preview behavior,
/// timeout sentinel) is stated once here instead of being duplicated into
/// every tool's schema.
///
/// Returns `None` if no tools have snippets or guidelines.
#[must_use]
pub fn build_tool_context_block(tools: &BTreeMap<String, ToolDefinition>) -> Option<String> {
    let mut snippets: Vec<(&str, &str)> = tools
        .values()
        .filter_map(|td| {
            td.prompt_snippet
                .as_ref()
                .map(|s| (td.name.as_str(), s.as_str()))
        })
        .collect();
    snippets.sort_unstable();

    let guidelines: Vec<&str> = tools
        .values()
        .flat_map(|td| td.prompt_guidelines.iter().map(String::as_str))
        .collect();

    if snippets.is_empty() && guidelines.is_empty() {
        return None;
    }

    let mut block = String::new();

    if !snippets.is_empty() {
        block.push_str("Available tools:\n");
        for (name, snippet) in &snippets {
            let _ = std::fmt::Write::write_fmt(&mut block, format_args!("- {name}: {snippet}\n"));
        }
    }

    if !guidelines.is_empty() {
        if !block.is_empty() {
            block.push('\n');
        }
        block.push_str("Tool guidelines:\n");
        for guideline in &guidelines {
            let _ = std::fmt::Write::write_fmt(&mut block, format_args!("- {guideline}\n"));
        }
    }

    if !block.is_empty() {
        block.push('\n');
    }
    block.push_str("Reserved tool-call arguments:\n");
    for line in reserved_argument_lines() {
        let _ = std::fmt::Write::write_fmt(&mut block, format_args!("- {line}\n"));
    }

    Some(block)
}

/// The reserved-argument conventions, one bullet per argument.
fn reserved_argument_lines() -> [&'static str; 2] {
    [
        "max_duration_secs: per-call timeout override in seconds; 0 disables the timeout.",
        "dump_to_disk: write the full tool result to this path (relative paths resolve against \
         the working directory; parent directories are created automatically). The tool response \
         becomes a pointer to the file plus a short preview — use it for large results (web page \
         fetches, bulk output) you want to read or grep from disk rather than carry in context.",
    ]
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use std::collections::BTreeMap;

    use super::*;

    fn test_tool(name: &str, snippet: Option<&str>, guidelines: Vec<&str>) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: snippet.map(str::to_owned),
            prompt_guidelines: guidelines.into_iter().map(str::to_owned).collect(),
            server_tool_type: None,
        }
    }

    /// The eight-tool catalog every rendering determinism check is run over.
    #[rustfmt::skip]
    const TOOL_CATALOG: [(&str, &str, &[&str]); 8] = [
        ("bash", "Execute shell commands", &["Prefer bash for pipelines"]),
        ("read", "Read file contents", &["Use read for file contents"]),
        ("write", "Write file contents", &["Use write for modifications"]),
        ("edit", "Edit file contents", &["Use edit for surgical changes"]),
        ("search", "Search the codebase", &["Use search before reading"]),
        ("list", "List directory entries", &["Use list to explore"]),
        ("grep", "Find text in files", &["Use grep for one-off matches"]),
        ("cd", "Change directory", &["Use cd to move between dirs"]),
    ];

    /// Builds a tool map from `TOOL_CATALOG`, in catalog order.
    fn catalog_tools() -> BTreeMap<String, ToolDefinition> {
        TOOL_CATALOG
            .iter()
            .map(|(name, snippet, guidelines)| {
                (
                    name.to_string(),
                    test_tool(name, Some(snippet), guidelines.to_vec()),
                )
            })
            .collect()
    }

    /// Builds a tool map from `TOOL_CATALOG`, in reversed catalog order.
    fn reversed_catalog_tools() -> BTreeMap<String, ToolDefinition> {
        TOOL_CATALOG
            .iter()
            .rev()
            .map(|(name, snippet, guidelines)| {
                (
                    name.to_string(),
                    test_tool(name, Some(snippet), guidelines.to_vec()),
                )
            })
            .collect()
    }

    #[rstest::rstest]
    fn tools_with_snippets_produce_available_tools_section() {
        // Given tools with snippets.
        let tools = BTreeMap::from([
            (
                "bash".to_owned(),
                test_tool("bash", Some("Execute commands"), vec![]),
            ),
            (
                "read".to_owned(),
                test_tool("read", Some("Read files"), vec![]),
            ),
        ]);

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then the block contains an "Available tools" section.
        let block = block.expect("should produce a block");
        assert!(block.contains("Available tools:"));
        assert!(block.contains("- bash: Execute commands"));
        assert!(block.contains("- read: Read files"));
    }

    #[rstest::rstest]
    fn tools_with_guidelines_produce_tool_guidelines_section() {
        // Given a tool with guidelines but no snippet.
        let tools = BTreeMap::from([(
            "bash".to_owned(),
            test_tool("bash", None, vec!["Use bash for ls"]),
        )]);

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then the block contains a "Tool guidelines" section.
        let block = block.expect("should produce a block");
        assert!(block.contains("Tool guidelines:"));
        assert!(block.contains("- Use bash for ls"));
        assert!(!block.contains("Available tools:"));
    }

    #[rstest::rstest]
    fn tools_without_metadata_are_skipped() {
        // Given tools without snippets or guidelines.
        let tools = BTreeMap::from([("echo".to_owned(), test_tool("echo", None, vec![]))]);

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then no block is produced.
        assert!(block.is_none());
    }

    #[rstest::rstest]
    fn empty_tools_produces_no_output() {
        // Given no tools.
        let tools = BTreeMap::new();

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then no block is produced.
        assert!(block.is_none());
    }

    #[rstest::rstest]
    fn both_snippets_and_guidelines_produce_both_sections() {
        // Given a tool with both snippet and guidelines.
        let tools = BTreeMap::from([(
            "edit".to_owned(),
            test_tool("edit", Some("Edit files"), vec!["Use edit for changes"]),
        )]);

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then both sections are present.
        let block = block.expect("should produce a block");
        assert!(block.contains("Available tools:"));
        assert!(block.contains("Tool guidelines:"));
        assert!(block.contains("- edit: Edit files"));
        assert!(block.contains("- Use edit for changes"));
    }

    #[rstest::rstest]
    fn both_snippets_and_guidelines_have_separator() {
        // Given a tool with both snippet and guidelines.
        let tools = BTreeMap::from([(
            "edit".to_owned(),
            test_tool("edit", Some("Edit files"), vec!["Use edit for changes"]),
        )]);

        // When building the tool context block.
        let block = build_tool_context_block(&tools);

        // Then there's a blank line between the sections.
        let block = block.expect("should produce a block");
        // The snippets section ends with the last snippet line ending in \n,
        // then there's an extra \n separator, then "Tool guidelines:".
        assert!(
            block.contains("\n\nTool guidelines:"),
            "should have blank line between sections, got: {block:?}"
        );
    }

    #[rstest::rstest]
    fn tool_block_is_stable_across_repeated_builds() {
        // Given a tool catalog and the block it renders to first.
        let tools = catalog_tools();
        let initial_block = build_tool_context_block(&tools).expect("should produce a block");

        for _ in 0..100 {
            // When building the tool context block again.
            let block = build_tool_context_block(&tools).expect("missing block");

            // Then it's the same as the initial block
            assert_eq!(block, initial_block);
        }
    }

    #[rstest::rstest]
    fn tool_block_is_stable_when_the_catalog_order_is_reversed() {
        // Given the same tool catalog in reverse insertion order, and the
        // block it renders to first.
        let tools = reversed_catalog_tools();
        let initial_block = build_tool_context_block(&tools).expect("should produce a block");

        for _ in 0..100 {
            // When building the tool context block again.
            let block = build_tool_context_block(&tools).expect("missing block");

            // Then it's the same as the initial block
            assert_eq!(block, initial_block);
        }
    }

    #[rstest::rstest]
    fn block_carries_the_reserved_arguments_section() {
        // Given a tool with a snippet.
        let tools = catalog_tools();

        // When building the block.
        let block = build_tool_context_block(&tools).expect("should produce a block");

        // Then the reserved-arguments section names both reserved arguments
        // and their conventions.
        assert!(block.contains("Reserved tool-call arguments:"));
        assert!(block.contains("max_duration_secs"));
        assert!(block.contains("dump_to_disk"));
        assert!(block.contains("parent directories are created automatically"));
    }

    #[rstest::rstest]
    fn with_reserved_arguments_adds_the_property() {
        // Given a definition whose schema carries a properties object (the
        // shape every real tool schema has).
        let mut definition = test_tool("bash", Some("Run commands"), vec![]);
        definition.parameters = serde_json::json!({
            "type": "object",
            "properties": { "command": { "type": "string" } }
        });

        // When applying the reserved-argument mapper.
        let mapped = super::with_reserved_arguments(definition);

        // Then the schema carries the dump_to_disk property alongside the
        // tool's own.
        let properties = mapped.parameters.get("properties").expect("properties");
        let property = properties
            .get("dump_to_disk")
            .expect("dump_to_disk property present");
        assert_eq!(
            property.get("type").and_then(|t| t.as_str()),
            Some("string")
        );
        assert!(properties.get("command").is_some(), "own property kept");
    }

    #[rstest::rstest]
    fn with_reserved_arguments_leaves_the_stored_definition_untouched() {
        // Given a definition with an object schema.
        let definition = test_tool("bash", Some("Run commands"), vec![]);

        // When applying the mapper to a clone (the mirror points pass copies).
        let _ = super::with_reserved_arguments(definition.clone());

        // Then the original schema is unchanged — the mapper is a copy
        // transform, never an in-place mutation.
        assert!(
            definition
                .parameters
                .get("properties")
                .and_then(|p| p.get("dump_to_disk"))
                .is_none()
        );
    }

    #[rstest::rstest]
    fn with_reserved_arguments_tolerates_non_object_schemas() {
        // Given definitions whose parameters are not objects with properties.
        let mut array_schema = test_tool("odd", None, vec![]);
        array_schema.parameters = serde_json::json!({"type": "array"});
        let mut bare_schema = test_tool("bare", None, vec![]);
        bare_schema.parameters = serde_json::json!({});

        // When applying the mapper to each.
        let mapped_array = super::with_reserved_arguments(array_schema);
        let mapped_bare = super::with_reserved_arguments(bare_schema);

        // Then both pass through without a panic and without a property.
        assert!(
            mapped_array
                .parameters
                .get("properties")
                .and_then(|p| p.get("dump_to_disk"))
                .is_none()
        );
        assert!(
            mapped_bare
                .parameters
                .get("properties")
                .and_then(|p| p.get("dump_to_disk"))
                .is_none()
        );
    }
}
