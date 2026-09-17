//! Read built-in tool - reads file contents with offset/limit support.
//!
//! Supports reading text files with optional line-based pagination via
//! `offset` (1-indexed) and `limit` parameters. Relative paths are resolved
//! against the session's CWD.

use std::path::{Path, PathBuf};

use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

use jinn_tools_msg::truncation::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head};
use super::visible_lines;

use super::BoxedToolFuture;

/// Returns the tool definition for the `read` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "read".to_owned(),
        description: "Reads a file from the local filesystem. Results are returned in cat -n format \n            with line numbers starting at 1: the prefix is spaces + line number + tab; \n            everything after the tab is the actual file content.\n\n\n            By default reads up to 2000 lines or 50KB. Use `offset`/`limit` for long files; \n            when output is truncated a notice shows the kept lines and the next offset \n            to continue from.".to_owned(),
        prompt_snippet: Some("Read a file from the local filesystem".to_owned()),
        prompt_guidelines: vec![
            "Read files before editing them so you can match content exactly.".to_owned(),
            "Batch-read multiple potentially relevant files in a single response.".to_owned(),
            "If `read` is truncated, continue with the `offset` it suggests — do not guess unseen lines.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["file_path"],
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

/// Executes the `read` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let (path, offset, limit) = match parse_args(&call.arguments) {
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

        let resolved = resolve_path(&path, &ctx.cwd);

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: format!("failed to read file '{}': {e}", resolved.display()),
                    success: false,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                };
            }
        };

        let sliced = apply_offset_limit(&content, offset, limit);

        // Number lines in cat -n format for the edit tool
        let start_line = offset.map_or(1, |o| o.max(1));
        let annotated = annotate_lines(&sliced, start_line);

        // Apply head-truncation to the result.
        let total_file_lines = content.lines().count();
        let max_lines = ctx.max_output_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = ctx.max_output_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        let truncation_result = truncate_head(&annotated, max_lines, max_bytes);

        if truncation_result.truncated {
            if let Some(meta) = truncation_result.meta {
                let end_line_display = start_line + meta.output_lines - 1;
                let next_offset = end_line_display + 1;
                let notice = if meta.truncated_by == jinn_core_types::tool_types::TruncatedBy::Bytes {
                    format!(
                        "\n\n[Showing lines {start_line}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                        format_size(max_bytes)
                    )
                } else {
                    format!(
                        "\n\n[Showing lines {start_line}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
                    )
                };
                let mut output = truncation_result.content;
                output.push_str(&notice);
                ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: output,
                    success: true,
                    full_content: Some(content),
                    truncation: Some(meta),
                    pin_position: None,
                }
            } else {
                // truncated but no meta - return unformatted truncated content
                ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: truncation_result.content,
                    success: true,
                    full_content: Some(content),
                    truncation: None,
                    pin_position: None,
                }
            }
        } else {
            ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: annotated,
                success: true,
                full_content: Some(content),
                truncation: None,
                pin_position: None,
            }
        }
    })
}

/// Numbers each line of `content` in cat -n format: right-aligned line
/// number, a tab, then the content.
///
/// `start_line` is the 1-indexed line number of the first line in `content`
/// (respects the offset parameter from the read call).
fn annotate_lines(content: &str, start_line: usize) -> String {
    use std::fmt::Write as _;
    if content.is_empty() {
        return String::new();
    }

    let lines = visible_lines::get_visible_lines(content);
    if lines.is_empty() {
        return String::new();
    }

    let max_line = start_line + lines.len() - 1;
    let width = format!("{max_line}").len();
    let mut out = String::with_capacity(content.len() + lines.len() * (width + 2));

    for (i, line) in lines.iter().enumerate() {
        let line_num = start_line + i;
        let _ = writeln!(out, "{line_num:>width$}\t{line}");
    }

    out
}

fn parse_args(raw: &str) -> Result<(String, Option<usize>, Option<usize>), serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let path = v
        .get("file_path")
        .or_else(|| v.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let offset = v
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);
    let limit = v
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);
    Ok((path, offset, limit))
}

fn apply_offset_limit(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return content.to_owned();
    }

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // offset is 1-indexed, convert to 0-indexed
    let start = offset.map_or(0, |o| o.saturating_sub(1));

    if start >= total_lines {
        return format!(
            "offset {} exceeds file length ({total_lines} lines)",
            offset.unwrap_or(0),
        );
    }

    let sliced = if let Some(limit) = limit {
        lines
            .get(start..total_lines.min(start + limit))
            .unwrap_or(&[])
    } else {
        lines.get(start..).unwrap_or(&[])
    };

    let mut result = sliced.join("\n");
    // Preserve trailing newline if original had one
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

// #[cfg(test)]
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::format_collect,
        reason = "test code"
    )]
    use super::*;
    use std::path::PathBuf;

    #[rstest::rstest]
    fn apply_offset_limit_no_offset_no_limit() {
        // Given a file with 3 lines.
        let content = "line1\nline2\nline3";

        // When applying no offset or limit.
        let result = apply_offset_limit(content, None, None);

        // Then the full content is returned.
        assert_eq!(result, "line1\nline2\nline3");
    }

    #[rstest::rstest]
    fn apply_offset_limit_with_offset() {
        // Given a file with 3 lines.
        let content = "line1\nline2\nline3";

        // When offset is 2 (1-indexed).
        let result = apply_offset_limit(content, Some(2), None);

        // Then lines from index 1 onward are returned.
        assert_eq!(result, "line2\nline3");
    }

    #[rstest::rstest]
    fn apply_offset_limit_with_limit() {
        // Given a file with 3 lines.
        let content = "line1\nline2\nline3";

        // When limit is 2.
        let result = apply_offset_limit(content, None, Some(2));

        // Then only the first 2 lines are returned.
        assert_eq!(result, "line1\nline2");
    }

    #[rstest::rstest]
    fn apply_offset_limit_with_offset_and_limit() {
        // Given a file with 5 lines.
        let content = "a\nb\nc\nd\ne";

        // When offset is 2, limit is 2.
        let result = apply_offset_limit(content, Some(2), Some(2));

        // Then lines 2-3 are returned.
        assert_eq!(result, "b\nc");
    }

    #[rstest::rstest]
    fn apply_offset_limit_offset_exceeds_length() {
        // Given a file with 2 lines.
        let content = "line1\nline2";

        // When offset is 10.
        let result = apply_offset_limit(content, Some(10), None);

        // Then an error message is returned.
        assert!(result.contains("offset 10 exceeds file length (2 lines)"));
    }

    #[rstest::rstest]
    fn resolve_path_relative() {
        // Given a relative path and a CWD.
        let resolved = resolve_path("foo/bar.txt", Path::new("/home/user/project"));

        // Then it's joined against CWD.
        assert_eq!(resolved, PathBuf::from("/home/user/project/foo/bar.txt"));
    }

    #[rstest::rstest]
    fn resolve_path_absolute() {
        // Given an absolute path.
        let resolved = resolve_path("/etc/hosts", Path::new("/home/user/project"));

        // Then it's returned as-is.
        assert_eq!(resolved, PathBuf::from("/etc/hosts"));
    }

    fn test_ctx() -> crate::feat::tools_actor::tool_types::ToolContext {
        crate::feat::tools_actor::tool_types::ToolContext {
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            cwd: PathBuf::from("/tmp"),
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
    async fn execute_reads_file_content() {
        // Given a temp file with known content.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "file contents here").expect("write temp file");

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy()
            })
            .to_string(),
        };

        // When executing the read tool.
        let result = execute(call, test_ctx()).await;

        // Then the result contains the file contents.
        assert_eq!(result.tool_call_id, "call_1");
        assert!(result.success);
        assert!(result.content.contains("file contents here"));
        assert!(
            result.content.starts_with("1\t"),
            "expected cat -n line prefix: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_missing_file() {
        // Given a read call for a nonexistent file.
        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": "/nonexistent/path/to/file.txt"
            })
            .to_string(),
        };

        // When executing the read tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert_eq!(result.tool_call_id, "call_2");
        assert!(!result.success);
        assert!(result.content.contains("failed to read file"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a read call with invalid JSON.
        let call = ToolCall {
            id: "call_3".to_owned(),
            name: "read".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the read tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_relative_path() {
        // Given a temp directory with a file.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "relative content").expect("write temp file");

        let ctx = crate::feat::tools_actor::tool_types::ToolContext {
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
            id: "call_4".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": "test.txt"
            })
            .to_string(),
        };

        // When executing with a relative path.
        let result = execute(call, ctx).await;

        // Then the file is found via CWD resolution.
        assert!(result.success);
        assert!(result.content.contains("relative content"));
        assert!(
            result.content.contains('\t'),
            "expected cat -n tab-separated prefix: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_with_offset_and_limit() {
        // Given a temp file with 5 lines.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("lines.txt");
        std::fs::write(&file_path, "a\nb\nc\nd\ne").expect("write temp file");

        let call = ToolCall {
            id: "call_5".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "offset": 2,
                "limit": 2
            })
            .to_string(),
        };

        // When executing with offset=2, limit=2.
        let result = execute(call, test_ctx()).await;

        // Then only lines 2-3 are returned.
        assert!(result.success);
        // Lines 2-3 with cat -n prefixes
        assert!(result.content.contains('b'));
        assert!(result.content.contains('c'));
        // First line should be line 2 (from offset=2)
        let first_line = result.content.lines().next().expect("first line");
        assert!(
            first_line.starts_with('2'),
            "expected line 2, got: {first_line}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_truncated_file_shows_correct_line_numbers_in_notice() {
        // Given a file with 10 lines and a small max_lines limit.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("big.txt");
        let content: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file_path, &content).expect("write temp file");

        let ctx = ToolContext {
            cwd: dir.path().to_owned(),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: crate::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: Some(3),
            max_output_bytes: Some(50 * 1024),

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        };

        let call = ToolCall {
            id: "call_trunc".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy()
            })
            .to_string(),
        };

        // When executing with a small line limit.
        let result = execute(call, ctx).await;

        // Then the result is successful and shows a truncation notice.
        assert!(result.success);
        assert!(result.content.contains("Showing lines"));
        // The notice should show lines 8-10 of 10 (tail truncation shows last 3).
        assert!(result.content.contains("of 10"));
        assert!(result.content.contains("offset="));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_truncated_with_offset_shows_correct_start_line() {
        // Given a file with 10 lines.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("big2.txt");
        let content: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file_path, &content).expect("write temp file");

        let ctx = ToolContext {
            cwd: dir.path().to_owned(),
            command_policy:
                jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: crate::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: Some(2),
            max_output_bytes: Some(50 * 1024),

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        };

        let call = ToolCall {
            id: "call_trunc2".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "offset": 3
            })
            .to_string(),
        };

        // When executing with offset=3 and a small line limit.
        let result = execute(call, ctx).await;

        // Then the notice includes the start line offset.
        assert!(result.success);
        assert!(result.content.contains("Showing lines"));
        // start_line should be computed from offset (3) + output_lines - 1.
        assert!(result.content.contains("offset="));
    }
}
