//! Write built-in tool - writes content to a file.
//!
//! Creates the file if it doesn't exist, overwrites if it does.
//! Automatically creates parent directories. Relative paths are resolved
//! against the session's CWD.

use crate::tool_types::ToolContext;
use crate::tool_types::unprefixed_failure;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

use super::BoxedToolFuture;
use super::input_bounds;
use crate::tool_paths::resolve_path;
use marked_path::MarkedPathAccess;

/// Returns the tool definition for the `write` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "write".to_owned(),
        description: "Writes a file to the local filesystem, overwriting the existing file \n            if one exists at the provided path. Parent directories are created automatically.\n\n\n            Prefer editing existing files; never create new files unless required, and never \n            create documentation files (*.md, README) unless the user explicitly asks.".to_owned(),
        prompt_snippet: Some("Create or overwrite a file".to_owned()),
        prompt_guidelines: vec![
            "Only use `write` for new files or complete rewrites — use `edit` for changes.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Resolves a path against the CWD if relative, returns absolute as-is.
///
/// Executes the `write` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let (path, content) = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return unprefixed_failure(&call, format!("failed to parse arguments: {e}"));
            }
        };

        // Reject degenerate repetition (e.g. a model sampler loop) before any
        // filesystem work.
        {
            let lines: Vec<&str> = content.split('\n').collect();
            if input_bounds::check_repetition(&lines).is_err() {
                return unprefixed_failure(
                    &call,
                    format!(
                        "[E_EDIT_DEGENERATE] `content` has a run of ≥{} identical \
                         consecutive lines. This is usually a model decoding loop. \
                         Re-issue the write without the repeated lines.",
                        input_bounds::MAX_IDENTICAL_RUN,
                    ),
                );
            }
        }

        let resolved = resolve_path(&path, &ctx.cwd);

        // Serialize against every other file-mutating tool call on this
        // path: the guard is held from directory creation through the
        // write, so a concurrent edit can't interleave between this call's
        // read of the world and its write.
        let _path_lock = crate::file_locks::acquire(&resolved).await;

        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return unprefixed_failure(
                &call,
                format!(
                    "failed to create parent directories for '{}': {e}",
                    resolved
                ),
            );
        }

        match tokio::fs::write(&resolved, &content).await {
            Ok(()) => ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: format!("wrote {} bytes to {}", content.len(), resolved),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            },
            Err(e) => {
                unprefixed_failure(&call, format!("failed to write file '{}': {e}", resolved))
            }
        }
    })
}

fn parse_args(raw: &str) -> Result<(String, String), serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let path = v
        .get("file_path")
        .or_else(|| v.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let content = v
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Ok((path, content))
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
    use super::*;
    use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

    /// A context whose cwd is a real, existing scratch directory owned by the
    /// calling test.
    fn test_ctx(cwd: &MarkedPathBuf<Absolute>) -> ToolContext {
        ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd: cwd.clone(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_content() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("output.txt")
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_1".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "hello from write"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates success.
        assert_eq!(result.tool_call_id.as_str(), "call_1");
        assert!(result.success, "expected success, got: {}", result.content);
        assert!(result.content.contains("wrote 16 bytes"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_creates_file_with_content() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("output.txt")
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_1".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "hello from write"
            })
            .to_string(),
        };

        // When executing the write tool.
        let _result = execute(call, test_ctx(&dir.path)).await;

        // Then the file contains the written content.
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "hello from write");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_creates_parent_dirs() {
        // Given a temp directory with a nested path.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join_all(["nested", "deep", "file.txt"])
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_2".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "nested content"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates success.
        assert!(result.success, "expected success, got: {}", result.content);

        // And the file was created with parent directories.
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "nested content");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_overwrites_existing_file() {
        // Given a temp file with existing content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("existing.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "old content").expect("write existing file");

        let call = ToolCall {
            id: "call_3".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "new content"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates success.
        assert!(result.success);

        // And the file was overwritten.
        let content = std::fs::read_to_string(&file_path).expect("read overwritten file");
        assert_eq!(content, "new content");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a write call with invalid JSON.
        let dir = jinn_testutil::tempdir();
        let call = ToolCall {
            id: "call_4".into(),
            name: "write".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_relative_path() {
        // Given a temp directory as CWD.
        let dir = jinn_testutil::tempdir();
        let ctx = ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd: dir.path.clone(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        };

        let call = ToolCall {
            id: "call_5".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": "relative.txt",
                "content": "relative content"
            })
            .to_string(),
        };

        // When executing with a relative path.
        let result = execute(call, ctx).await;

        // Then the file is created via CWD resolution.
        assert!(result.success);
        let file_path = dir
            .path
            .join("relative.txt")
            .expect("temp root joins a bare name");
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "relative content");
    }

    #[rstest::rstest]
    fn parse_args_missing_path_defaults_to_empty() {
        // Given JSON with only a content field.
        let raw = r#"{"content":"hi"}"#;

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then path defaults to empty and content is preserved.
        assert_eq!(path, "");
        assert_eq!(content, "hi");
    }

    #[rstest::rstest]
    fn parse_args_missing_content_defaults_to_empty() {
        // Given JSON with only a path field.
        let raw = r#"{"path":"f.txt"}"#;

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then path is preserved and content defaults to empty.
        assert_eq!(path, "f.txt");
        assert_eq!(content, "");
    }

    #[rstest::rstest]
    fn parse_args_both_missing_defaults_to_empty() {
        // Given an empty JSON object.
        let raw = "{}";

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then both default to empty strings.
        assert_eq!(path, "");
        assert_eq!(content, "");
    }

    #[rstest::rstest]
    fn parse_args_path_is_integer_defaults_to_empty() {
        // Given JSON where path is an integer instead of a string.
        let raw = r#"{"path":42,"content":"hi"}"#;

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then path defaults to empty (as_str returns None for non-strings).
        assert_eq!(path, "");
        assert_eq!(content, "hi");
    }

    #[rstest::rstest]
    fn parse_args_content_is_boolean_defaults_to_empty() {
        // Given JSON where content is a boolean instead of a string.
        let raw = r#"{"path":"f.txt","content":true}"#;

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then content defaults to empty (as_str returns None for non-strings).
        assert_eq!(path, "f.txt");
        assert_eq!(content, "");
    }

    #[rstest::rstest]
    fn parse_args_extra_fields_are_ignored() {
        // Given JSON with extra unknown fields.
        let raw = r#"{"path":"f.txt","content":"hi","extra":123}"#;

        // When parsing arguments.
        let (path, content) = parse_args(raw).expect("parse");

        // Then the known fields are extracted and extras are ignored.
        assert_eq!(path, "f.txt");
        assert_eq!(content, "hi");
    }

    #[rstest::rstest]
    fn parse_args_invalid_json_returns_error() {
        // Given a string that is not valid JSON.
        let raw = "not json";

        // When parsing arguments.
        let result = parse_args(raw);

        // Then the result is an error.
        assert!(result.is_err());
    }

    // ── Phase 2: Content fidelity tests ──

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_empty_string() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("empty.txt")
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_e1".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": ""
            })
            .to_string(),
        };

        // When executing the write tool with empty content.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates success.
        assert!(result.success, "expected success, got: {}", result.content);

        // And the file exists and is empty.
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_newlines_and_tabs() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("whitespace.txt")
            .expect("temp root joins a bare name");
        let original = "line1\nline2\ttab";

        let call = ToolCall {
            id: "call_e2".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_embedded_quotes() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("quotes.txt")
            .expect("temp root joins a bare name");
        let original = r#"he said "hello\""#;

        let call = ToolCall {
            id: "call_e3".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_angle_brackets() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("angles.txt")
            .expect("temp root joins a bare name");
        let original = "HashMap<String, Vec<usize>>";

        let call = ToolCall {
            id: "call_e4".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_ampersands() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("ampersands.txt")
            .expect("temp root joins a bare name");
        let original = "foo & bar < baz";

        let call = ToolCall {
            id: "call_e5".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_backslashes() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("backslashes.txt")
            .expect("temp root joins a bare name");
        let original = r"C:\Users\test\file.txt";

        let call = ToolCall {
            id: "call_e6".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_mixed_line_endings() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("endings.txt")
            .expect("temp root joins a bare name");
        let original = "line1\r\nline2\nline3";

        let call = ToolCall {
            id: "call_e7".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_null_bytes() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("nulls.bin")
            .expect("temp root joins a bare name");
        let original = "before\0after";

        let call = ToolCall {
            id: "call_e8".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly (byte comparison).
        assert!(result.success);
        let bytes = std::fs::read(&file_path).expect("read written file");
        assert_eq!(bytes, original.as_bytes());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_unicode_emoji() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("emoji.txt")
            .expect("temp root joins a bare name");
        let original = "Hello \u{1F30D}\u{1F980}\u{1F389}";

        let call = ToolCall {
            id: "call_e9".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_cjk() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("cjk.txt")
            .expect("temp root joins a bare name");
        let original = "\u{4F60}\u{597D}\u{4E16}\u{754C}";

        let call = ToolCall {
            id: "call_e10".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_combining_characters() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("combining.txt")
            .expect("temp root joins a bare name");
        let original = "e\u{0301}";

        let call = ToolCall {
            id: "call_e11".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_large_payload() {
        // Given a temp directory and a 1MB payload.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("large.txt")
            .expect("temp root joins a bare name");
        let original = "x".repeat(1_000_000);

        let call = ToolCall {
            id: "call_e12".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content.len(), 1_000_000);
        assert_eq!(content, original);
    }

    // ── Phase 3: Path edge cases ──

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_with_spaces_in_name() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("my file.txt")
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_p1".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "spaces in name"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the file is created with the spaced name.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "spaces in name");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_with_unicode_name() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt")
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_p2".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "unicode name"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the file is created with the Unicode name.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "unicode name");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_deeply_nested_path() {
        // Given a temp directory with a 6-level deep path.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join_all(["a", "b", "c", "d", "e", "f.txt"])
            .expect("temp root joins a bare name");

        let call = ToolCall {
            id: "call_p3".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "deeply nested"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then all parent directories are created and the file is written.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "deeply nested");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_dot_slash_relative_path() {
        // Given a temp directory as CWD.
        let dir = jinn_testutil::tempdir();
        let ctx = ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd: dir.path.clone(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        };

        let call = ToolCall {
            id: "call_p4".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": "./local.txt",
                "content": "dot slash"
            })
            .to_string(),
        };

        // When executing with a ./ relative path.
        let result = execute(call, ctx).await;

        // Then the file is created via CWD resolution.
        assert!(result.success, "expected success, got: {}", result.content);
        let file_path = dir
            .path
            .join("local.txt")
            .expect("temp root joins a bare name");
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "dot slash");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_absolute_path() {
        // Given a temp directory with an absolute path, and a different
        // directory as the CWD.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("absolute.txt")
            .expect("temp root joins a bare name");
        let other = jinn_testutil::tempdir();

        let ctx = ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd: other.path.clone(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        };

        let call = ToolCall {
            id: "call_p5".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": "absolute path"
            })
            .to_string(),
        };

        // When executing with an absolute path and a different CWD.
        let result = execute(call, ctx).await;

        // Then the absolute path is used, not CWD.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "absolute path");
    }

    // ── Phase 4: Byte count accuracy ──

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_reports_correct_byte_count_for_ascii() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("ascii.txt")
            .expect("temp root joins a bare name");
        let content = "hello";

        let call = ToolCall {
            id: "call_b1".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the success message reports the correct ASCII byte count.
        assert!(result.success);
        assert_eq!(content.len(), 5);
        assert!(
            result.content.contains("wrote 5 bytes"),
            "expected 'wrote 5 bytes' in '{}'",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_reports_correct_byte_count_for_multibyte() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("multibyte.txt")
            .expect("temp root joins a bare name");
        let content = "\u{1F389}"; // 🎉 - 4 bytes in UTF-8

        let call = ToolCall {
            id: "call_b2".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the success message reports byte count (4), not character count (1).
        assert!(result.success);
        assert_eq!(content.len(), 4);
        assert!(
            result.content.contains("wrote 4 bytes"),
            "expected 'wrote 4 bytes' in '{}'",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_reports_correct_byte_count_for_mixed() {
        // Given a temp directory.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("mixed.txt")
            .expect("temp root joins a bare name");
        let content = "Hello \u{1F30D}"; // Hello + space + 🌍 = 5 + 1 + 4 = 10 bytes

        let call = ToolCall {
            id: "call_b3".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the success message reports the correct mixed byte count.
        assert!(result.success);
        assert_eq!(content.len(), 10);
        assert!(
            result.content.contains("wrote 10 bytes"),
            "expected 'wrote 10 bytes' in '{}'",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_rejects_content_with_long_repetition() {
        // Given a temp dir that does not yet contain the target file.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("out.txt")
            .expect("temp root joins a bare name");

        // `content` with 50 identical consecutive lines — degenerate.
        let content = std::iter::once("header\n")
            .chain(std::iter::repeat_n("x\n", 50))
            .collect::<String>();
        let call = ToolCall {
            id: "rep_50".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": content,
            })
            .to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then it is rejected as degenerate.
        assert!(!result.success);
        assert!(result.content.contains("E_EDIT_DEGENERATE"));
        // And the file is not created.
        assert!(!file_path.exists(), "file should not have been created");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_accepts_content_with_49_repetition() {
        // Given a temp dir.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("out.txt")
            .expect("temp root joins a bare name");

        // `content` with 49 identical consecutive lines — below threshold.
        let content = std::iter::once("header\n")
            .chain(std::iter::repeat_n("x\n", 49))
            .collect::<String>();
        let call = ToolCall {
            id: "rep_49".into(),
            name: "write".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string(),
                "content": content,
            })
            .to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the write succeeds (49 lines, under the threshold).
        assert!(result.success, "expected success, got: {}", result.content);
        assert!(file_path.exists());
    }
}
