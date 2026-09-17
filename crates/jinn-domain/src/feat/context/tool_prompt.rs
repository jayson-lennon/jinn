//! Tool prompt context builder - assembles "Available tools" and "Tool guidelines"
//! sections for the system prompt from registered tool definitions.
//!
//! Each tool can optionally carry a [`prompt_snippet`](jinn_core_types::ToolDefinition::prompt_snippet)
//! (a one-line summary) and [`prompt_guidelines`](jinn_core_types::ToolDefinition::prompt_guidelines)
//! (behavioral bullet points). This module collects them from all registered tools
//! and formats them into a single string for injection into the system prompt.

use std::collections::BTreeMap;

use jinn_core_types::ToolDefinition;

/// Builds the tool context block for the system prompt.
///
/// Iterates over all registered tool definitions and collects:
/// - `prompt_snippet` → listed in the "Available tools" section
/// - `prompt_guidelines` → listed as bullet points in the "Tool guidelines" section
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

    Some(block)
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
    fn tool_block_always_renders_the_same_so_we_get_cache_hits() {
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

        // Given a tool catalog.
        let tools = TOOL_CATALOG
            .iter()
            .map(|(name, snippet, guidelines)| {
                (
                    name.to_string(),
                    test_tool(name, Some(snippet), guidelines.to_vec()),
                )
            })
            .collect();

        let initial_block = build_tool_context_block(&tools).expect("should produce a block");

        for _ in 0..100 {
            // When building the tool context block.
            let block = build_tool_context_block(&tools).expect("missing block");

            // Then it's the same as the initial block
            assert_eq!(block, initial_block);
        }

        // Given a tool catalog.
        let tools = TOOL_CATALOG
            .iter()
            .rev()
            .map(|(name, snippet, guidelines)| {
                (
                    name.to_string(),
                    test_tool(name, Some(snippet), guidelines.to_vec()),
                )
            })
            .collect();

        let initial_block = build_tool_context_block(&tools).expect("should produce a block");

        for _ in 0..100 {
            // When building the tool context block.
            let block = build_tool_context_block(&tools).expect("missing block");

            // Then it's the same as the initial block
            assert_eq!(block, initial_block);
        }
    }
}
