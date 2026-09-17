//! Write built-in tool - writes content to a file.
//!
//! Creates the file if it doesn't exist, overwrites if it does.
//! Automatically creates parent directories. Relative paths are resolved
//! against the session's CWD.

use std::path::{Path, PathBuf};

use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

use super::BoxedToolFuture;
use super::input_bounds;

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
fn resolve_path(path: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_owned()
    } else {
        cwd.join(p)
    }
}

/// Executes the `write` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let (path, content) = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: format!("failed to parse arguments: {e}"),
                    success: false,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                };
            }
        };

        // Reject degenerate repetition (e.g. a model sampler loop) before any
        // filesystem work.
        {
            let lines: Vec<&str> = content.split('\n').collect();
            if input_bounds::check_repetition(&lines).is_err() {
                return ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: format!(
                        "[E_EDIT_DEGENERATE] `content` has a run of ≥{} identical \
                         consecutive lines. This is usually a model decoding loop. \
                         Re-issue the write without the repeated lines.",
                        input_bounds::MAX_IDENTICAL_RUN,
                    ),
                    success: false,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                };
            }
        }

        let resolved = resolve_path(&path, &ctx.cwd);

        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: format!(
                    "failed to create parent directories for '{}': {e}",
                    resolved.display()
                ),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            };
        }

        match tokio::fs::write(&resolved, &content).await {
            Ok(()) => ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: format!("wrote {} bytes to {}", content.len(), resolved.display()),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
            Err(e) => ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: format!("failed to write file '{}': {e}", resolved.display()),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
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

// #[cfg(test)]
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
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
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
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_content() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("output.txt");

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "hello from write"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates success.
        assert_eq!(result.tool_call_id, "call_1");
        assert!(result.success, "expected success, got: {}", result.content);
        assert!(result.content.contains("wrote 16 bytes"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_creates_file_with_content() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("output.txt");

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "hello from write"
            })
            .to_string(),
        };

        // When executing the write tool.
        let _result = execute(call, test_ctx()).await;

        // Then the file contains the written content.
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "hello from write");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_creates_parent_dirs() {
        // Given a temp directory with a nested path.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("nested").join("deep").join("file.txt");

        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "nested content"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("existing.txt");
        std::fs::write(&file_path, "old content").expect("write existing file");

        let call = ToolCall {
            id: "call_3".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "new content"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let call = ToolCall {
            id: "call_4".to_owned(),
            name: "write".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_relative_path() {
        // Given a temp directory as CWD.
        let dir = tempfile::tempdir().expect("create temp dir");
        let ctx = ToolContext {
            cwd: dir.path().to_owned(),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
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

        let call = ToolCall {
            id: "call_5".to_owned(),
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
        let file_path = dir.path().join("relative.txt");
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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("empty.txt");

        let call = ToolCall {
            id: "call_e1".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": ""
            })
            .to_string(),
        };

        // When executing the write tool with empty content.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("whitespace.txt");
        let original = "line1\nline2\ttab";

        let call = ToolCall {
            id: "call_e2".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_embedded_quotes() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("quotes.txt");
        let original = r#"he said "hello\""#;

        let call = ToolCall {
            id: "call_e3".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_angle_brackets() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("angles.txt");
        let original = "HashMap<String, Vec<usize>>";

        let call = ToolCall {
            id: "call_e4".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_ampersands() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("ampersands.txt");
        let original = "foo & bar < baz";

        let call = ToolCall {
            id: "call_e5".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_backslashes() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("backslashes.txt");
        let original = r"C:\Users\test\file.txt";

        let call = ToolCall {
            id: "call_e6".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_mixed_line_endings() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("endings.txt");
        let original = "line1\r\nline2\nline3";

        let call = ToolCall {
            id: "call_e7".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_null_bytes() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("nulls.bin");
        let original = "before\0after";

        let call = ToolCall {
            id: "call_e8".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly (byte comparison).
        assert!(result.success);
        let bytes = std::fs::read(&file_path).expect("read written file");
        assert_eq!(bytes, original.as_bytes());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_unicode_emoji() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("emoji.txt");
        let original = "Hello \u{1F30D}\u{1F980}\u{1F389}";

        let call = ToolCall {
            id: "call_e9".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_cjk() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("cjk.txt");
        let original = "\u{4F60}\u{597D}\u{4E16}\u{754C}";

        let call = ToolCall {
            id: "call_e10".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_combining_characters() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("combining.txt");
        let original = "e\u{0301}";

        let call = ToolCall {
            id: "call_e11".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the content round-trips exactly.
        assert!(result.success);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, original);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_roundtrips_large_payload() {
        // Given a temp directory and a 1MB payload.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("large.txt");
        let original = "x".repeat(1_000_000);

        let call = ToolCall {
            id: "call_e12".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": original
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("my file.txt");

        let call = ToolCall {
            id: "call_p1".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "spaces in name"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the file is created with the spaced name.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "spaces in name");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_with_unicode_name() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt");

        let call = ToolCall {
            id: "call_p2".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "unicode name"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then the file is created with the Unicode name.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "unicode name");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_deeply_nested_path() {
        // Given a temp directory with a 6-level deep path.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("d")
            .join("e")
            .join("f.txt");

        let call = ToolCall {
            id: "call_p3".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "deeply nested"
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

        // Then all parent directories are created and the file is written.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "deeply nested");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_dot_slash_relative_path() {
        // Given a temp directory as CWD.
        let dir = tempfile::tempdir().expect("create temp dir");
        let ctx = ToolContext {
            cwd: dir.path().to_owned(),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
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

        let call = ToolCall {
            id: "call_p4".to_owned(),
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
        let file_path = dir.path().join("local.txt");
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "dot slash");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_absolute_path() {
        // Given a temp directory with an absolute path.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("absolute.txt");

        let ctx = ToolContext {
            cwd: PathBuf::from("/completely/different/cwd"),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
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

        let call = ToolCall {
            id: "call_p5".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("ascii.txt");
        let content = "hello";

        let call = ToolCall {
            id: "call_b1".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("multibyte.txt");
        let content = "\u{1F389}"; // 🎉 - 4 bytes in UTF-8

        let call = ToolCall {
            id: "call_b2".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("mixed.txt");
        let content = "Hello \u{1F30D}"; // Hello + space + 🌍 = 5 + 1 + 4 = 10 bytes

        let call = ToolCall {
            id: "call_b3".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": content
            })
            .to_string(),
        };

        // When executing the write tool.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("out.txt");

        // `content` with 50 identical consecutive lines — degenerate.
        let content = std::iter::once("header\n")
            .chain(std::iter::repeat_n("x\n", 50))
            .collect::<String>();
        let call = ToolCall {
            id: "rep_50".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": content,
            })
            .to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx()).await;

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("out.txt");

        // `content` with 49 identical consecutive lines — below threshold.
        let content = std::iter::once("header\n")
            .chain(std::iter::repeat_n("x\n", 49))
            .collect::<String>();
        let call = ToolCall {
            id: "rep_49".to_owned(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": content,
            })
            .to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx()).await;

        // Then the write succeeds (49 lines, under the threshold).
        assert!(result.success, "expected success, got: {}", result.content);
        assert!(file_path.exists());
    }
}
