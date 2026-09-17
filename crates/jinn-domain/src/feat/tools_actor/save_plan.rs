//! Save-plan built-in tool — writes a plan file and pins the tool result.
//!
//! Identical file-writing logic to the `write` tool, but the resulting
//! `ToolResult` is pinned with `PinPosition::Relative` so it survives
//! compaction. The tool description encourages the `.plans/<task>/`
//! path convention via guidelines but does not enforce it.

use std::path::{Path, PathBuf};

use crate::feat::tools_actor::tool_types::{
    ToolCall, ToolContext, ToolDefinition, ToolResult, ToolResultPinPosition,
};

use super::BoxedToolFuture;

/// Returns the tool definition for the `save_plan` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "save_plan".to_owned(),
        description: "Save a plan file to disk and pin the result so it persists in \
            conversation history through compaction. Creates the file if it doesn't \
            exist, overwrites if it does. Automatically creates parent directories."
            .to_owned(),
        prompt_snippet: Some("Save a plan file".to_owned()),
        prompt_guidelines: vec![
            "Use save_plan to write plan files — the result is pinned so the plan \
             stays visible as context shrinks."
                .to_owned(),
            "Prefer paths under `.plans/<task-slug>/` (e.g. `.plans/my-feature/plan.md`)."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the plan file (relative or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the plan file"
                }
            },
            "required": ["path", "content"]
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

/// Executes the `save_plan` built-in tool.
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
                content: format!(
                    "saved plan to {} ({} bytes)",
                    resolved.display(),
                    content.len()
                ),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: Some(ToolResultPinPosition::Relative),
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
        .get("path")
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
    fn definition_has_correct_name() {
        // Given the save_plan tool definition.
        let def = definition();

        // Then the name is "save_plan".
        assert_eq!(def.name, "save_plan");
    }

    #[rstest::rstest]
    fn definition_requires_path_and_content() {
        // Given the save_plan tool definition.
        let def = definition();

        // Then the parameters require both "path" and "content".
        let required = def
            .parameters
            .get("required")
            .expect("should have required");
        let arr = required.as_array().expect("required should be array");
        assert!(arr.contains(&serde_json::json!("path")));
        assert!(arr.contains(&serde_json::json!("content")));
    }

    #[rstest::rstest]
    fn definition_mentions_plans_dir_in_guidelines() {
        // Given the save_plan tool definition.
        let def = definition();

        // Then at least one guideline mentions .plans/.
        assert!(
            def.prompt_guidelines.iter().any(|g| g.contains(".plans")),
            "expected a guideline mentioning .plans/"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_writes_file_content() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("plan.md");

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "# Plan\nDo the thing."
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the file on disk contains the written content.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "# Plan\nDo the thing.");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_pins_on_success() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("plan.md");

        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "plan text"
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result is pinned with Relative.
        assert!(result.success);
        assert_eq!(result.pin_position, Some(ToolResultPinPosition::Relative));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_no_pin_on_bad_json() {
        // Given a call with invalid JSON.
        let call = ToolCall {
            id: "call_3".to_owned(),
            name: "save_plan".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result has no pin and is a failure.
        assert!(!result.success);
        assert_eq!(result.pin_position, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_no_pin_on_dir_creation_failure() {
        // Given a path inside /proc where create_dir_all will fail.
        let call = ToolCall {
            id: "call_4".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": "/proc/nonexistent_dir/impossible_plan.md",
                "content": "will fail"
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result has no pin and is a failure.
        assert!(!result.success);
        assert_eq!(result.pin_position, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_no_pin_on_file_write_failure() {
        // Given a temp directory with a read-only subdirectory so that
        // create_dir_all succeeds but the file write fails.
        let dir = tempfile::tempdir().expect("create temp dir");
        let readonly = dir.path().join("readonly");
        std::fs::create_dir_all(&readonly).expect("create readonly dir");
        let _perms = std::fs::set_permissions(
            &readonly,
            std::os::unix::fs::PermissionsExt::from_mode(0o555),
        );
        let file_path = readonly.join("plan.md");

        let call = ToolCall {
            id: "call_4b".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "will fail"
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result has no pin and is a failure.
        assert!(!result.success, "expected failure, got: {}", result.content);
        assert_eq!(result.pin_position, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_path_not_content() {
        // Given a temp directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("plan.md");
        let plan_body = "# My Secret Plan\nStep 1: Do the thing.";

        let call = ToolCall {
            id: "call_5".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": plan_body
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result contains the path and byte count but not the plan body.
        assert!(result.success);
        assert!(
            result.content.contains("saved plan to"),
            "expected 'saved plan to', got: {}",
            result.content
        );
        assert!(
            result.content.contains("bytes"),
            "expected byte count, got: {}",
            result.content
        );
        assert!(
            !result.content.contains("Secret Plan"),
            "result should not contain plan body, got: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_creates_parent_dirs() {
        // Given a nested path like .plans/my-task/plan.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join(".plans").join("my-task").join("plan.md");

        let call = ToolCall {
            id: "call_6".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "content": "plan content"
            })
            .to_string(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the file was created with parent directories.
        assert!(result.success, "expected success, got: {}", result.content);
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "plan content");
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
            id: "call_7".to_owned(),
            name: "save_plan".to_owned(),
            arguments: serde_json::json!({
                "path": "relative.md",
                "content": "relative content"
            })
            .to_string(),
        };

        // When executing with a relative path.
        let result = execute(call, ctx).await;

        // Then the file is created via CWD resolution.
        assert!(result.success);
        let file_path = dir.path().join("relative.md");
        let content = std::fs::read_to_string(&file_path).expect("read written file");
        assert_eq!(content, "relative content");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a save_plan call with invalid JSON.
        let call = ToolCall {
            id: "call_8".to_owned(),
            name: "save_plan".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the save_plan tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }
}
