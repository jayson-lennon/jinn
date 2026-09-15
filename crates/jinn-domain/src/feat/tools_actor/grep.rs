//! Grep built-in tool - searches files using ripgrep.
//!
//! A compatibility shim for weak LLMs that hallucinate a `grep` tool call.
//! Spawns `rg` directly (no shell), passes the pattern and optional flags
//! as arguments, captures output, applies truncation, and returns a
//! [`ToolResult`]. No streaming, no prompt steering.

use std::fmt::Write as _;
use std::process::Stdio;

use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

use super::truncation::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_tail};

use super::BoxedToolFuture;

/// Returns the tool definition for the `grep` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "grep".to_owned(),
        description: "Search files for a pattern using ripgrep (rg). \
            Returns matching lines with file paths and line numbers. \
            Output is truncated to last 2000 lines or 50KB (whichever is hit first)."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The search pattern (regular expression)"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Defaults to the current working directory."
                },
                "glob": {
                    "type": "string",
                    "description": "Include only files matching the given glob (e.g. \"*.rs\")."
                },
                "file_type": {
                    "type": "string",
                    "description": "Search only files of the given type (e.g. \"rust\", \"python\")."
                }
            },
            "required": ["pattern"]
        }),
        server_tool_type: None,
    }
}

/// Arguments parsed from the tool call JSON.
#[derive(serde::Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    file_type: Option<String>,
}

/// Parses the arguments from the tool call JSON.
fn parse_args(raw: &str) -> Result<GrepArgs, serde_json::Error> {
    serde_json::from_str(raw)
}

/// Creates an error [`ToolResult`] with the given fields and `success: false`.
fn error_tool_result(tool_call_id: String, name: String, content: String) -> ToolResult {
    ToolResult {
        tool_call_id,
        name,
        content,
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Formats the final [`ToolResult`] from the process output, applying
/// tail-truncation when output exceeds the configured limits.
fn format_output(
    content: &str,
    success: bool,
    tool_call_id: String,
    tool_name: String,
    max_lines: usize,
    max_bytes: usize,
) -> ToolResult {
    let truncation_result = truncate_tail(content, max_lines, max_bytes);
    if truncation_result.truncated {
        if let Some(meta) = truncation_result.meta {
            let start_line = meta.total_lines.saturating_sub(meta.output_lines) + 1;
            let end_line = meta.total_lines;
            let notice = if meta.truncated_by == jinn_provider::tool_types::TruncatedBy::Bytes {
                format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit)]",
                    meta.total_lines,
                    format_size(max_bytes)
                )
            } else {
                format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {}]",
                    meta.total_lines
                )
            };
            let mut output = truncation_result.content;
            output.push_str(&notice);
            ToolResult {
                tool_call_id,
                name: tool_name,
                content: output,
                success,
                full_content: Some(content.to_owned()),
                truncation: Some(meta),
                pin_position: None,
            }
        } else {
            ToolResult {
                tool_call_id,
                name: tool_name,
                content: truncation_result.content,
                success,
                full_content: Some(content.to_owned()),
                truncation: None,
                pin_position: None,
            }
        }
    } else {
        ToolResult {
            tool_call_id,
            name: tool_name,
            content: content.to_owned(),
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    }
}

/// Builds the combined stdout+stderr content string.
fn build_content(stdout: &str, stderr: &str) -> String {
    let mut content = stdout.to_owned();
    if !stderr.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        let _ = write!(content, "{stderr}");
    }
    content
}

/// Executes the `grep` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let args = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return error_tool_result(
                    call.id,
                    call.name,
                    format!("failed to parse arguments: {e}"),
                );
            }
        };

        if args.pattern.is_empty() {
            return error_tool_result(call.id, call.name, "pattern is empty".to_owned());
        }

        let mut std_cmd = std::process::Command::new("rg");
        std_cmd
            .arg("--color")
            .arg("never")
            .arg("--no-heading")
            .arg(&args.pattern)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&ctx.cwd);

        if let Some(ref path) = args.path {
            std_cmd.arg(path);
        }
        if let Some(ref glob) = args.glob
            && !glob.is_empty()
        {
            std_cmd.arg("--glob").arg(glob);
        }
        if let Some(ref file_type) = args.file_type
            && !file_type.is_empty()
        {
            std_cmd.arg("--type").arg(file_type);
        }

        // Terminal isolation: detach from jinn's session/console so rg can
        // never write over the TUI (see jinn_common::process_isolation).
        jinn_common::process_isolation::isolate(&mut std_cmd);
        let mut cmd = tokio::process::Command::from(std_cmd);

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return error_tool_result(call.id, call.name, format!("failed to execute rg: {e}"));
            }
        };

        let max_lines = ctx.max_output_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = ctx.max_output_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let code = output.status.code().unwrap_or(-1);
        let success = matches!(code, 0 | 1);

        let content = build_content(&stdout, &stderr);

        format_output(&content, success, call.id, call.name, max_lines, max_bytes)
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::format_push_string,
        reason = "test code"
    )]
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy:
                crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
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

    fn test_ctx_with_cwd(cwd: PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            command_policy:
                crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
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

    fn tool_call(pattern: &str) -> ToolCall {
        ToolCall {
            id: "call_1".to_owned(),
            name: "grep".to_owned(),
            arguments: serde_json::json!({ "pattern": pattern }).to_string(),
        }
    }

    fn tool_call_with_args(pattern: &str, extra: serde_json::Value) -> ToolCall {
        let mut args = serde_json::json!({ "pattern": pattern });
        if let serde_json::Value::Object(ref mut map) = args
            && let serde_json::Value::Object(extra_map) = extra
        {
            for (k, v) in extra_map {
                map.insert(k, v);
            }
        }
        ToolCall {
            id: "call_1".to_owned(),
            name: "grep".to_owned(),
            arguments: args.to_string(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_matching_lines() {
        // Given a directory with a file containing known text.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(
            dir.path().join("sample.txt"),
            "hello world\nfoo bar\nhello rust\n",
        )
        .expect("write file");

        let call = tool_call("hello");
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When executing the grep tool.
        let result = execute(call, ctx).await;

        // Then the result contains the matching lines.
        assert!(result.success);
        assert!(result.content.contains("hello world"));
        assert!(result.content.contains("hello rust"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_with_path_searches_specific_dir() {
        // Given two directories with different files.
        let dir = tempfile::tempdir().expect("create temp dir");
        let sub_a = dir.path().join("a");
        let sub_b = dir.path().join("b");
        std::fs::create_dir_all(&sub_a).expect("create dir a");
        std::fs::create_dir_all(&sub_b).expect("create dir b");
        std::fs::write(sub_a.join("f.txt"), "target_match in a\n").expect("write a");
        std::fs::write(sub_b.join("f.txt"), "target_match in b\n").expect("write b");

        let call = tool_call_with_args("target_match", serde_json::json!({ "path": "a" }));
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When executing with path pointing at subdirectory a.
        let result = execute(call, ctx).await;

        // Then only files in directory a are searched.
        assert!(result.success);
        assert!(result.content.contains("in a"));
        assert!(!result.content.contains("in b"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_with_glob_filters_files() {
        // Given a directory with .rs and .txt files.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("code.rs"), "fn special_func() {}\n").expect("write rs");
        std::fs::write(dir.path().join("notes.txt"), "special_func is great\n").expect("write txt");

        let call = tool_call_with_args("special_func", serde_json::json!({ "glob": "*.rs" }));
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When executing with glob filtering for .rs files.
        let result = execute(call, ctx).await;

        // Then only the .rs file is searched.
        assert!(result.success);
        assert!(result.content.contains("code.rs"));
        assert!(!result.content.contains("notes.txt"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_with_file_type_filters_by_language() {
        // Given a directory with .rs files.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("main.rs"), "fn my_unique_func() {}\n").expect("write rs");

        let call =
            tool_call_with_args("my_unique_func", serde_json::json!({ "file_type": "rust" }));
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When executing with file_type=rust.
        let result = execute(call, ctx).await;

        // Then the rust file is searched.
        assert!(result.success);
        assert!(result.content.contains("my_unique_func"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_empty_on_no_matches() {
        // Given a directory with a file.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("sample.txt"), "hello world\n").expect("write file");

        let call = tool_call("xyzzy_no_match_12345");
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When searching for a pattern that doesn't exist.
        let result = execute(call, ctx).await;

        // Then rg exit code 1 is treated as success with empty content.
        assert!(result.success);
        assert!(result.content.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_reports_rg_error_on_invalid_pattern() {
        // Given an invalid regex pattern that will cause rg to error.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("sample.txt"), "hello\n").expect("write file");

        let call = ToolCall {
            id: "call_err".to_owned(),
            name: "grep".to_owned(),
            arguments: serde_json::json!({ "pattern": "[" }).to_string(),
        };
        let ctx = test_ctx_with_cwd(dir.path().to_owned());

        // When executing with an invalid pattern.
        let result = execute(call, ctx).await;

        // Then the result indicates failure.
        assert!(!result.success);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_empty_pattern() {
        // Given a grep tool call with an empty pattern.
        let call = ToolCall {
            id: "call_empty".to_owned(),
            name: "grep".to_owned(),
            arguments: serde_json::json!({ "pattern": "" }).to_string(),
        };

        // When executing the grep tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("pattern is empty"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a grep tool call with invalid JSON.
        let call = ToolCall {
            id: "call_bad".to_owned(),
            name: "grep".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the grep tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_truncates_large_output() {
        // Given a file with many matching lines.
        let dir = tempfile::tempdir().expect("create temp dir");
        let content: String = (0..3000).fold(String::new(), |mut s, i| {
            s.push_str(&format!("match_line_{i}\n"));
            s
        });
        std::fs::write(dir.path().join("big.txt"), content).expect("write file");

        let call = tool_call("match_line");
        let mut ctx = test_ctx_with_cwd(dir.path().to_owned());
        ctx.max_output_lines = Some(10);

        // When executing with a small line limit.
        let result = execute(call, ctx).await;

        // Then output is truncated.
        assert!(result.success);
        assert!(result.content.contains("[Showing lines"));
        assert!(result.full_content.is_some());
    }

    #[rstest::rstest]
    fn build_content_stdout_only() {
        // Given stdout and empty stderr.
        // When building content.
        let result = build_content("hello", "");
        // Then only stdout is present.
        assert_eq!(result, "hello");
    }

    #[rstest::rstest]
    fn build_content_stderr_appended() {
        // Given stdout and stderr.
        // When building content.
        let result = build_content("out\n", "err");
        // Then stderr is appended after stdout.
        assert!(result.contains("out"));
        assert!(result.contains("err"));
    }

    #[rstest::rstest]
    fn build_content_stderr_only() {
        // Given empty stdout and stderr.
        // When building content.
        let result = build_content("", "error msg");
        // Then only stderr is present.
        assert_eq!(result, "error msg");
    }

    #[rstest::rstest]
    fn build_content_adds_newline_between_stdout_and_stderr() {
        // Given stdout without trailing newline and stderr.
        // When building content.
        let result = build_content("no newline", "error");
        // Then a newline is inserted between them.
        assert!(result.contains("no newline\nerror"));
    }

    #[rstest::rstest]
    fn format_output_no_truncation() {
        // Given content well within limits.
        let result = format_output(
            "hello",
            true,
            "id".to_owned(),
            "grep".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then no truncation occurs.
        assert!(result.success);
        assert_eq!(result.content, "hello");
        assert!(result.full_content.is_none());
        assert!(result.truncation.is_none());
    }

    #[rstest::rstest]
    fn format_output_with_truncation() {
        // Given content exceeding the line limit.
        let content: String = (0..3000).fold(String::new(), |mut s, i| {
            s.push_str(&format!("line {i}\n"));
            s
        });

        let result = format_output(
            &content,
            true,
            "id".to_owned(),
            "grep".to_owned(),
            10,
            DEFAULT_MAX_BYTES,
        );

        // Then output is truncated with notice.
        assert!(result.success);
        assert!(result.content.contains("[Showing lines"));
        assert!(result.full_content.is_some());
    }
}
