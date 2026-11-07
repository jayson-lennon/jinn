//! `canvas` built-in tool — serves one HTML artifact through the canvas
//! actor and returns the URL the agent relays to the user.
//!
//! Architecture: the tool `ask`s the canvas actor through
//! [`jinn_canvas_msg::CanvasServerHandle`] (the `restart_mcp` seam
//! precedent). The actor owns the server exclusively: the first call
//! establishes the listener and the directory watcher, later calls swap
//! the served artifact on the live listener at the same URL. The tool
//! resolves its path against the session's working directory — the same
//! arithmetic every filesystem tool runs — before handing the absolute
//! path over.

use futures::FutureExt as _;
use marked_path::MarkedPathAccess as _;

use crate::tool_paths::resolve_path;
use crate::tool_types::ToolContext;
use crate::tool_types::failed;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

use super::BoxedToolFuture;

/// Returns the tool definition for the `canvas` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "canvas".to_owned(),
        description: "Serve an HTML artifact file to the user's browser with live reload — \
            the running jinn hosts it and the result text names the URL to give the user. \
            The first call starts the server on an OS-assigned port on the interface the \
            [canvas] bind config names (loopback by default); every later call swaps the \
            served file on the same URL and connected browsers follow. \
            After edits you do NOT need to call this again — saving the file (or any \
            sibling in its directory) hot-reloads the page within a quarter second. \
            Build artifacts from the canvas skill's skeleton; their /vendor/ paths are \
            served by this server offline."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![
            "After serving, put the returned URL in your reply so the user can click it."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path of the HTML artifact to serve (relative or absolute). \
                        A relative path resolves against the session's working directory."
                }
            },
            "required": ["path"]
        }),
        server_tool_type: None,
    }
}

/// Executes the `canvas` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let tool_call_id = call.id;
    let tool_name = call.name;

    let Some(server) = ctx.canvas_server else {
        return futures::future::ready(failed(
            tool_call_id,
            tool_name,
            "the canvas server is not wired (the canvas slice did not activate; \
             this should not happen in a running jinn)",
        ))
        .boxed();
    };

    let path = match parse_args(&call.arguments) {
        Ok(path) => path,
        Err(message) => {
            return futures::future::ready(failed(tool_call_id, tool_name, &message)).boxed();
        }
    };
    let absolute = resolve_path(&path, &ctx.cwd)
        .as_path()
        .to_string_lossy()
        .into_owned();

    async move {
        // The actor answers every command it receives; a transport error
        // here means the round trip never completed, and the sentence
        // names that (the domain refusals — missing file, bind failure
        // — arrive as `ok: false` outcomes).
        match server.serve(absolute).await {
            Ok(outcome) if outcome.ok => ToolResult {
                content: format!(
                    "Serving {} at {}\n\n\
                     Put the URL in your reply so the user can click it. \
                     Edits to the file (or its siblings) hot-reload the page automatically — \
                     do not call this tool again after edits; call it only to switch to a \
                     different artifact.",
                    outcome.artifact, outcome.url
                ),
                success: true,
                ..failed_shaped(tool_call_id, tool_name)
            },
            Ok(outcome) => failed(tool_call_id, tool_name, &outcome.error),
            Err(jinn_canvas_msg::CanvasServeTransportError::Timeout) => failed(
                tool_call_id,
                tool_name,
                "the canvas actor did not answer in time — retry once; \
                 a second timeout means the serving actor is wedged \
                 (**STOP and wait for user instruction**)",
            ),
            Err(jinn_canvas_msg::CanvasServeTransportError::Mailbox) => failed(
                tool_call_id,
                tool_name,
                "the canvas actor is unreachable (mailbox failure) — \
                 **STOP and wait for user instruction**",
            ),
        }
    }
    .boxed()
}

/// The neutral `ToolResult` skeleton the success arm fills in: the
/// idiom's shape comes from the failure helper, minus its failure
/// semantics.
fn failed_shaped(tool_call_id: jinn_core_types::ToolCallId, name: String) -> ToolResult {
    ToolResult {
        tool_call_id,
        name,
        content: String::new(),
        success: true,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Why canvas arguments could not be resolved.
///
/// The message is the tool-result prose, worded to name the fix.
fn parse_args(args_str: &str) -> Result<String, String> {
    if args_str.trim().is_empty() {
        return Err("missing `path` argument: pass the HTML artifact to serve".to_owned());
    }
    let value: serde_json::Value =
        serde_json::from_str(args_str).map_err(|e| format!("invalid arguments: {e}"))?;
    value
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "`path` argument must be a string naming the HTML artifact".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "test assertions"
    )]

    use super::parse_args;

    #[rstest::rstest]
    #[test]
    fn parse_args_reads_the_path_argument() {
        // Given the tool's argument JSON.
        // When parsing.
        let path = parse_args(r#"{"path": "chart.html"}"#).expect("parses");

        // Then the path comes back.
        assert_eq!(path, "chart.html");
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_a_missing_path_with_the_fix() {
        // Given an arguments object without `path`.
        // When parsing.
        let error = parse_args(r#"{"file": "chart.html"}"#).expect_err("refused");

        // Then the message names the argument and the fix.
        assert!(error.contains("`path`"), "{error}");
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_empty_arguments_by_name() {
        // Given no arguments at all.
        // When parsing.
        let error = parse_args("").expect_err("refused");

        // Then the message names `path`.
        assert!(error.contains("`path`"), "{error}");
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_malformed_json() {
        // Given malformed JSON.
        // When parsing.
        let error = parse_args("{nope").expect_err("refused");

        // Then the message names the parse failure.
        assert!(error.contains("invalid arguments"), "{error}");
    }
}
