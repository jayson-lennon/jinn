//! Edit built-in tool — exact string replacement in UTF-8 text files.
//!
//! Mirrors the Claude Code `Edit` interface: one exact-match replacement per
//! call, unique unless `replace_all`. Matching happens on LF-normalized
//! content with BOM stripped; both are restored on the atomic write. After a
//! successful edit, a cat -n snippet of the changed region is returned so the
//! LLM can chain edits without re-reading.

mod engine;
mod line_ending;
mod response;

use crate::tool_paths::resolve_path;
use crate::tool_types::ToolContext;
use crate::tool_types::unprefixed_failure;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

use super::BoxedToolFuture;
use super::input_bounds;
use line_ending::{detect_line_ending, normalize_to_lf, restore_line_endings, strip_bom};
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use response::{build_changed_snippet, format_success_response};

/// Construct a success [`ToolResult`] from a `call` and `content`.
fn ok_result(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content,
        success: true,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Returns the tool definition for the `edit` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "edit".to_owned(),
        description: r#"Performs exact string replacements in files.

`old_string` must match existing content exactly, including indentation —
when editing from `read` output, everything after the line-number prefix is
the file content; never include the prefix itself.

The edit fails if `old_string` is not unique in the file: provide a larger
string with more surrounding context, or use `replace_all` to change every
instance (useful for renaming a variable).

Prefer editing existing files; never write new files unless required."#
            .to_owned(),
        prompt_snippet: Some("Edit a file by replacing exact text".to_owned()),
        prompt_guidelines: vec![
            "`old_string` must match the file exactly and be unique; include more surrounding context or use `replace_all` when it is not.".to_owned(),
            "A successful edit returns a numbered snippet of the changed region; use it for nearby follow-up edits without re-reading.".to_owned(),
            "Prefer `edit` over `write` for changes to existing files.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The text to replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "default": false,
                    "description": "Replace all occurrences of old_string (default false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `edit` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let args = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => return unprefixed_failure(&call, e.to_string()),
        };

        if args.file_path.is_empty() {
            return unprefixed_failure(
                &call,
                "`file_path` is required. Provide the file path, e.g. \"src/main.rs\".".to_owned(),
            );
        }

        if args.old_string.is_empty() {
            return unprefixed_failure(
                &call,
                "`old_string` must be non-empty and match existing file content exactly."
                    .to_owned(),
            );
        }

        if args.old_string == args.new_string {
            return ok_result(
                &call,
                format!(
                    "No changes made to {}: `old_string` and `new_string` are identical.",
                    args.file_path
                ),
            );
        }

        // Reject degenerate repetition (e.g. a model sampler loop) before any
        // filesystem work.
        {
            let new_lines: Vec<&str> = args.new_string.split('\n').collect();
            if input_bounds::check_repetition(&new_lines).is_err() {
                return unprefixed_failure(
                    &call,
                    format!(
                        "[E_EDIT_DEGENERATE] `new_string` has a run of ≥{} identical \n                         consecutive lines. This is usually a model \n                         decoding loop. Re-issue the edit without the repeated lines.",
                        input_bounds::MAX_IDENTICAL_RUN,
                    ),
                );
            }
        }

        let resolved = resolve_path(&args.file_path, &ctx.cwd);

        // Serialize against every other file-mutating tool call on this
        // path: the guard is held from the content read through the atomic
        // write, so a batched sibling edit applies against this call's
        // result instead of racing it on a stale read.
        let _path_lock = crate::file_locks::acquire(&resolved).await;

        let raw_content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return unprefixed_failure(
                    &call,
                    format!("failed to read file '{}': {e}", resolved),
                );
            }
        };

        // Preserve original file permissions
        let orig_mode = std::fs::metadata(&resolved).ok().map(|m| m.permissions());

        // Strip BOM and detect line endings
        let (content_without_bom, bom) = strip_bom(&raw_content);
        let detected_ending = detect_line_ending(content_without_bom);

        // Normalize to LF for processing; old/new strings are normalized the
        // same way so model-supplied CRLF still matches.
        let normalized = normalize_to_lf(content_without_bom);
        let old_string = normalize_to_lf(&args.old_string);
        let new_string = normalize_to_lf(&args.new_string);

        let occurrences = engine::count_occurrences(&normalized, &old_string);
        if occurrences == 0 {
            return unprefixed_failure(&call, engine::not_found_error(&old_string));
        }
        if occurrences > 1 && !args.replace_all {
            return unprefixed_failure(&call, engine::not_unique_error(&old_string, occurrences));
        }

        let new_content =
            engine::replace_exact(&normalized, &old_string, &new_string, args.replace_all);

        if normalized == new_content {
            return ok_result(&call, format!("No changes made to {}.", args.file_path));
        }

        // Restore line endings and BOM
        let restored = restore_line_endings(&new_content, detected_ending);
        let final_content = match bom {
            Some(b) => format!("{b}{restored}"),
            None => restored,
        };

        if let Err(report) = write_atomic(&resolved, &final_content, orig_mode.as_ref()).await {
            return unprefixed_failure(&call, report.to_string());
        }

        // Build a cat -n snippet of the changed region for chaining.
        let (first_changed, last_changed) =
            engine::compute_changed_line_range(&normalized, &new_content);
        let snippet = build_changed_snippet(&new_content, first_changed, last_changed);
        let response_text = format_success_response(&args.file_path, &snippet);

        ok_result(&call, response_text)
    })
}

/// Writes `content` to `resolved` atomically via temp-file + rename, preserving permissions.
///
/// The temp path is the target's full file name plus a suffix, so two files
/// in one directory that differ only by extension can never share a temp
/// path. Same-path writes are serialized by [`crate::file_locks`], so the
/// deterministic name is never contended.
async fn write_atomic(
    resolved: &MarkedPathBuf<Absolute>,
    content: &str,
    orig_mode: Option<&std::fs::Permissions>,
) -> Result<(), error_stack::Report<EditWriteError>> {
    use error_stack::IntoReport as _;

    let file_name = resolved
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("file"), ToOwned::to_owned);
    let mut tmp_name = file_name;
    tmp_name.push(".jinn-edit-tmp");

    let tmp_path = resolved.with_file_name(tmp_name);
    if let Err(e) = tokio::fs::write(&tmp_path, content).await {
        return Err(EditWriteError::TempWrite {
            path: tmp_path.to_string(),
            detail: e.to_string(),
        }
        .into_report());
    }

    if let Some(mode) = orig_mode
        && let Err(e) = std::fs::set_permissions(&tmp_path, mode.clone())
    {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(EditWriteError::SetPermissions {
            detail: e.to_string(),
        }
        .into_report());
    }

    if let Err(e) = std::fs::rename(&tmp_path, resolved) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(EditWriteError::Rename {
            path: resolved.to_string(),
            detail: e.to_string(),
        }
        .into_report());
    }
    Ok(())
}

/// The parsed flat edit arguments: one exact-string replacement per call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// Why the edit call's arguments could not be parsed.
///
/// The call boundary renders the message into tool-result prose; a `Report`'s
/// `Display` prints only the outermost context, so each variant's message
/// stands alone.
#[derive(Debug, wherror::Error)]
pub enum EditArgsError {
    /// The call's JSON arguments were malformed.
    #[error("failed to parse arguments: {detail}")]
    InvalidArguments {
        /// The parser's complaint.
        detail: String,
    },
    /// The arguments used the removed `edits` array shape.
    #[error(
        "the `edits` array format is no longer supported; call edit with \
             `file_path`, `old_string`, `new_string`, and optional `replace_all`"
    )]
    EditsArrayRemoved,
}

/// Why an edit could not be committed to disk.
///
/// The call boundary renders the message into tool-result prose; a `Report`'s
/// `Display` prints only the outermost context, so each variant's message
/// stands alone.
#[derive(Debug, wherror::Error)]
pub enum EditWriteError {
    /// The temp file could not be written.
    #[error("failed to write temp file '{path}': {detail}")]
    TempWrite {
        /// The temp path.
        path: String,
        /// The filesystem's complaint.
        detail: String,
    },
    /// The original file's permissions could not be applied to the temp file.
    #[error("failed to set permissions on temp file: {detail}")]
    SetPermissions {
        /// The filesystem's complaint.
        detail: String,
    },
    /// The temp file could not be renamed over the target.
    #[error("failed to rename temp file to '{path}': {detail}")]
    Rename {
        /// The target path.
        path: String,
        /// The filesystem's complaint.
        detail: String,
    },
}

/// Parses the arguments from the tool call JSON.
///
/// Accepts `path`, `oldText`, and `newText` as aliases for `file_path`,
/// `old_string`, and `new_string`.
fn parse_args(raw: &str) -> Result<EditArgs, error_stack::Report<EditArgsError>> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| EditArgsError::InvalidArguments {
            detail: e.to_string(),
        })?;

    if v.get("edits").is_some_and(serde_json::Value::is_array) {
        return Err(EditArgsError::EditsArrayRemoved.into());
    }

    let field = |primary: &str, alias: &str| {
        v.get(primary)
            .or_else(|| v.get(alias))
            .and_then(serde_json::Value::as_str)
    };

    let file_path = field("file_path", "path").unwrap_or("").to_owned();
    let old_string = field("old_string", "oldText").unwrap_or("").to_owned();
    let new_string = field("new_string", "newText").unwrap_or("").to_owned();
    let replace_all = v
        .get("replace_all")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    Ok(EditArgs {
        file_path,
        old_string,
        new_string,
        replace_all,
    })
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
    use crate::tool_types::ToolContext;
    use jinn_core_types::tool_types::ToolCall;
    use marked_path::MarkedPathAccess;

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

    fn test_ctx_with_cwd(cwd: MarkedPathBuf<Absolute>) -> ToolContext {
        ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd,
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

    fn make_call(file_path: &str, old_string: &str, new_string: &str) -> ToolCall {
        ToolCall {
            id: "call_edit".into(),
            name: "edit".into(),
            arguments: serde_json::json!({
                "file_path": file_path,
                "old_string": old_string,
                "new_string": new_string
            })
            .to_string(),
        }
    }

    #[rstest::rstest]
    fn definition_has_correct_name() {
        // Given the edit tool definition.
        let def = definition();

        // When reading the definition's name and prompt metadata.
        // Then it has the name "edit".
        assert_eq!(def.name, "edit");
        assert!(def.prompt_snippet.is_some());
        assert!(!def.prompt_guidelines.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_replaces_unique_string() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When replacing a unique string.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "world", "rust"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the edit is applied.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "hello rust"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_not_found() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When replacing a missing string.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "missing", "x"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the error instructs a re-read.
        assert!(!result.success);
        assert!(result.content.contains("E_NOT_FOUND"));
        assert!(result.content.contains("read"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_when_not_unique() {
        // Given a temp file with a repeated string.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "x is here\nx again").expect("write temp file");

        // When replacing without replace_all.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "x", "y"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the error reports the occurrence count.
        assert!(!result.success);
        assert!(result.content.contains("E_NOT_UNIQUE"));
        assert!(result.content.contains("2 times"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_replace_all_swaps_every_occurrence() {
        // Given a temp file with a repeated string.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "count\nnot_count\ncount").expect("write temp file");

        // When replacing with replace_all.
        let call = ToolCall {
            id: "call_ra".into(),
            name: "edit".into(),
            arguments: serde_json::json!({
                "file_path": file_path.to_string_lossy(),
                "old_string": "count",
                "new_string": "total",
                "replace_all": true
            })
            .to_string(),
        };
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then every occurrence is replaced.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "total\nnot_total\ntotal"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_success_response_contains_snippet() {
        // Given a temp file with three lines.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "alpha\nbeta\ngamma\n").expect("write temp file");

        // When replacing the middle line.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "beta", "BETA"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the response contains a cat -n snippet of the changed region.
        assert!(result.success, "expected success, got: {}", result.content);
        assert!(
            result.content.contains("--- lines 1-3 ---"),
            "got: {}",
            result.content
        );
        assert!(result.content.contains("\tBETA"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_accepts_alias_arguments() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When calling with path/oldText/newText aliases.
        let call = ToolCall {
            id: "call_alias".into(),
            name: "edit".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "oldText": "world",
                "newText": "rust"
            })
            .to_string(),
        };
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the edit is applied.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "hello rust"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_identical_strings_is_noop() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When old_string equals new_string.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "world", "world"),
            test_ctx(&dir.path),
        )
        .await;

        // Then it is a successful no-op and the file is untouched.
        assert!(result.success, "got: {}", result.content);
        assert!(result.content.contains("No changes"));
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "hello world"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_preserves_crlf() {
        // Given a temp file with CRLF line endings.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "line1\r\nline2\r\nline3\r\n").expect("write temp file");

        // When replacing a line.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "line2", "modified"),
            test_ctx(&dir.path),
        )
        .await;

        // Then CRLF line endings are preserved.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "line1\r\nmodified\r\nline3\r\n"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_preserves_bom() {
        // Given a temp file with a UTF-8 BOM.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "\u{feff}hello world").expect("write temp file");

        // When replacing a word.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "world", "rust"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the BOM is preserved.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "\u{feff}hello rust"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_rejects_legacy_edits_array() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When calling with the legacy edits[] shape.
        let call = ToolCall {
            id: "call_legacy".into(),
            name: "edit".into(),
            arguments: serde_json::json!({
                "path": file_path.to_string_lossy(),
                "edits": [{"op": "replace", "pos": "1#xx", "lines": ["x"]}]
            })
            .to_string(),
        };
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the error names the new parameters.
        assert!(!result.success);
        assert!(result.content.contains("no longer supported"));
        assert!(result.content.contains("old_string"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_rejects_degenerate_new_string() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        let original = "hello\nworld\n";
        std::fs::write(&file_path, original).expect("write temp file");

        // When replacing with 50 identical lines in new_string.
        let degenerate: String = vec![",".to_owned(); 50].join("\n");
        let result = execute(
            make_call(&file_path.to_string_lossy(), "hello", &degenerate),
            test_ctx(&dir.path),
        )
        .await;

        // Then it is rejected and the file is untouched.
        assert!(!result.success);
        assert!(result.content.contains("E_EDIT_DEGENERATE"));
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            original
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_line_number_prefix_misses() {
        // Given a temp file with content.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "fn main() {}\n").expect("write temp file");

        // When old_string includes a read-style line-number prefix.
        let result = execute(
            make_call(&file_path.to_string_lossy(), "1\tfn main() {}", "x"),
            test_ctx(&dir.path),
        )
        .await;

        // Then the edit fails rather than matching content.
        assert!(!result.success);
        assert!(result.content.contains("E_NOT_FOUND"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given an edit call with invalid JSON.
        let dir = jinn_testutil::tempdir();
        let call = ToolCall {
            id: "call_bad".into(),
            name: "edit".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the edit tool.
        let result = execute(call, test_ctx(&dir.path)).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_relative_path() {
        // Given a temp directory as CWD with a file.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("test.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "hello world").expect("write temp file");

        // When executing with a relative path.
        let result = execute(
            make_call("test.txt", "world", "rust"),
            test_ctx_with_cwd(dir.path.clone()),
        )
        .await;

        // Then the edit is applied via CWD resolution.
        assert!(result.success, "expected success, got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read file"),
            "hello rust"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn concurrent_non_overlapping_edits_all_land() {
        // Given a temp file two concurrent edits will anchor on separately.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("batch.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "alpha\nbeta\n").expect("write temp file");
        let path_str = file_path.to_string_lossy().into_owned();

        // When both edits run concurrently on the same path.
        let (first, second) = tokio::join!(
            execute(make_call(&path_str, "alpha", "ALPHA"), test_ctx(&dir.path)),
            execute(make_call(&path_str, "beta", "BETA"), test_ctx(&dir.path)),
        );

        // Then both edits succeed.
        assert!(first.success, "first edit failed: {}", first.content);
        assert!(second.success, "second edit failed: {}", second.content);

        // And the final content contains both replacements — neither edit
        // was applied against a stale read.
        let final_content = std::fs::read_to_string(&file_path).expect("read file");
        assert_eq!(final_content, "ALPHA\nBETA\n");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn concurrent_edits_two_files_same_extension_both_succeed() {
        // Given two files in one directory with the same extension.
        let dir = jinn_testutil::tempdir();
        let rust_path = dir
            .path
            .join("notes.rs")
            .expect("temp root joins a bare name");
        let text_path = dir
            .path
            .join("notes.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&rust_path, "let x = 1;\n").expect("write temp file");
        std::fs::write(&text_path, "x = 1\n").expect("write temp file");
        let rust_str = rust_path.to_string_lossy().into_owned();
        let text_str = text_path.to_string_lossy().into_owned();

        // When both are edited concurrently.
        let (rust_result, text_result) = tokio::join!(
            execute(
                make_call(&rust_str, "let x = 1;", "let y = 2;"),
                test_ctx(&dir.path)
            ),
            execute(make_call(&text_str, "x = 1", "x = 2"), test_ctx(&dir.path)),
        );

        // Then neither edit hits a shared temp path — both succeed.
        assert!(
            rust_result.success,
            "rust edit failed: {}",
            rust_result.content
        );
        assert!(
            text_result.success,
            "text edit failed: {}",
            text_result.content
        );

        // And each file carries only its own replacement.
        assert_eq!(
            std::fs::read_to_string(&rust_path).expect("read rust file"),
            "let y = 2;\n"
        );
        assert_eq!(
            std::fs::read_to_string(&text_path).expect("read text file"),
            "x = 2\n"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn concurrent_edits_second_overlap_fails_with_not_found() {
        // Given a temp file with a single anchor.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("single.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "only anchor\n").expect("write temp file");
        let path_str = file_path.to_string_lossy().into_owned();

        // When two concurrent edits both target that anchor.
        let (first, second) = tokio::join!(
            execute(
                make_call(&path_str, "only anchor", "changed"),
                test_ctx(&dir.path)
            ),
            execute(
                make_call(&path_str, "only anchor", "also changed"),
                test_ctx(&dir.path)
            ),
        );

        // Then exactly one succeeded and the other failed.
        let successes = [first.success, second.success]
            .into_iter()
            .filter(|ok| *ok)
            .count();
        assert_eq!(successes, 1, "expected exactly one success");

        // And the failure names the anchor, not a temp-file rename error.
        let failure = if first.success { &second } else { &first };
        assert!(
            failure.content.contains("E_NOT_FOUND"),
            "expected E_NOT_FOUND, got: {}",
            failure.content
        );
        assert!(
            !failure.content.contains("rename"),
            "failure should not be a rename error: {}",
            failure.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn concurrent_edit_and_write_file_is_exactly_one_tools_output() {
        // Given a temp file an edit and a write will race on.
        let dir = jinn_testutil::tempdir();
        let file_path = dir
            .path
            .join("race.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file_path, "anchor\n").expect("write temp file");
        let path_str = file_path.to_string_lossy().into_owned();

        let write_call = ToolCall {
            id: "call_race_write".into(),
            name: "write".to_owned(),
            arguments: serde_json::json!({
                "file_path": path_str,
                "content": "complete write output\n"
            })
            .to_string(),
        };

        // When the edit and the write run concurrently on the same path.
        let (edit_result, write_result) = tokio::join!(
            execute(
                make_call(&path_str, "anchor", "ANCHOR"),
                test_ctx(&dir.path)
            ),
            { super::super::write::execute(write_call, test_ctx(&dir.path)) }
        );

        // Then both tools report success.
        assert!(edit_result.success, "edit failed: {}", edit_result.content);
        assert!(
            write_result.success,
            "write failed: {}",
            write_result.content
        );

        // And the file is exactly one tool's complete output — never a
        // tear or merge of the two.
        let final_content = std::fs::read_to_string(&file_path).expect("read file");
        assert!(
            final_content == "ANCHOR\n" || final_content == "complete write output\n",
            "torn or merged state: {final_content:?}"
        );
    }
}
