//! `session_fetch` built-in tool — read a window of a persisted session transcript.
//!
//! Companion to `session_search`: drill into a search hit by its stable
//! `entry_id` with an agent-chosen amount of surrounding context, or read
//! the tail of a session. Ordinals in the output are rendered positions
//! (live state), never addresses.

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_domain::feat::session_search::TranscriptWindow;

use std::fmt::Write as _;

use super::BoxedToolFuture;

/// Entries of context around the anchor when `context` is omitted.
const DEFAULT_CONTEXT: u64 = 6;
/// Upper bound on `context`.
const MAX_CONTEXT: u64 = 50;
/// Entries returned by a tail read when `limit` is omitted.
const DEFAULT_TAIL: u64 = 30;
/// Upper bound on a tail read.
const MAX_TAIL: u64 = 100;
/// Soft per-entry render cap before elision.
const MAX_ENTRY_CHARS: usize = 2_000;

/// Returns the tool definition for the `session_fetch` built-in tool.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "session_fetch".to_owned(),
        description: "Read entries from a persisted session transcript. The companion to \
 session_search: pass the entry id from a search hit to see it in context, or fetch the last \
 entries of a session to catch up on it.\n\nTwo modes:\n  anchored — pass entry_id (from a \
 session_search hit); returns it with surrounding entries. context controls how many entries \
 total (default 6, max 50).\n  tail — omit entry_id; returns the last entries of the session \
 (default 30, max 100).\n\nEach line is rendered as [position] role: text. Entries currently \
 excluded from the model's context are flagged [excluded from context] — useful for \
 understanding why something was forgotten. Positions shift as sessions grow; always address \
 entries by entry id, never by position.\n\nOmit session_id to read the current session.\n\n\
 Examples:\n  session_fetch({\"entry_id\": \"e-7f3a…\", \"context\": 10})\n  \
 session_fetch({\"session_id\": \"0196a3b2-…\", \"limit\": 15})"
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![
            "Drill into session_search hits with session_fetch; keep context small (6-10) and \
 widen only if needed."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session to read. Omits to the current session."
                },
                "entry_id": {
                    "type": "string",
                    "description": "Anchor entry id from a session_search hit. Omit for a tail read."
                },
                "context": {
                    "type": "integer",
                    "description": "Anchored mode: total entries to return around the anchor (1-50, default 6)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Tail mode: number of trailing entries to return (1-100, default 30)."
                }
            }
        }),
        server_tool_type: None,
    }
}

/// Parsed and validated tool arguments.
#[derive(Debug)]
enum FetchMode {
    /// Anchored read around a specific entry.
    Anchored { entry_id: String, context: usize },
    /// Tail read of the last N entries.
    Tail { limit: usize },
}

/// Parses and validates raw JSON arguments.
fn parse_args(raw: &str) -> Result<(Option<String>, FetchMode), String> {
    let args: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON arguments: {e}"))?;

    let session_id = args
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let entry_id = args
        .get("entry_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let mode = match entry_id {
        Some(entry_id) => {
            let context = match args.get("context") {
                None | Some(serde_json::Value::Null) => DEFAULT_CONTEXT,
                Some(v) => {
                    let n = v.as_u64().ok_or("context must be a positive integer")?;
                    n.clamp(1, MAX_CONTEXT)
                }
            };
            let context = usize::try_from(context).unwrap_or(usize::MAX);
            FetchMode::Anchored { entry_id, context }
        }
        None => {
            let limit = match args.get("limit") {
                None | Some(serde_json::Value::Null) => DEFAULT_TAIL,
                Some(v) => {
                    let n = v.as_u64().ok_or("limit must be a positive integer")?;
                    n.clamp(1, MAX_TAIL)
                }
            };
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            FetchMode::Tail { limit }
        }
    };

    Ok((session_id, mode))
}

/// Resolves which session to read: explicit id or the current session.
fn resolve_session(
    explicit: Option<String>,
    current: Option<&jinn_domain::protocol::SessionId>,
) -> Result<jinn_domain::protocol::SessionId, String> {
    if let Some(id) = explicit {
        return Ok(jinn_domain::protocol::SessionId::from(id));
    }
    current
        .cloned()
        .ok_or_else(|| "no session_id given and no current session".to_owned())
}

/// Renders a fetched window as transcript text.
fn format_window(window: &TranscriptWindow) -> String {
    let title = window.title.as_deref().unwrap_or("Untitled Session");
    let first = window.entries.first().map_or(0, |e| e.ordinal);
    let last = window.entries.last().map_or(0, |e| e.ordinal);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "session \"{title}\" ({}) — entries {first}–{last} of {}",
        window.session_id, window.total_entries
    );

    let mut prev_ordinal: Option<usize> = None;
    for item in &window.entries {
        // Make elided gaps visible so rendered positions stay trustworthy.
        if let Some(prev) = prev_ordinal
            && item.ordinal > prev + 1
        {
            let _ = writeln!(
                out,
                "[{}] … {} entries not shown …",
                prev + 1,
                item.ordinal - prev - 1
            );
        }
        prev_ordinal = Some(item.ordinal);

        let flag = if item.excluded {
            " [excluded from context]"
        } else {
            ""
        };
        let text = item.entry.text();
        let (rendered, note) = elide_entry_text(&text);
        let _ = writeln!(
            out,
            "[{}] {}{}: {rendered}",
            item.ordinal,
            item.entry.kind_str(),
            flag
        );
        if let Some(note) = note {
            let _ = writeln!(out, "    [{note}]");
        }
    }
    out
}

/// Splits an entry's text into (rendered text, optional elision note).
fn elide_entry_text(text: &str) -> (String, Option<String>) {
    let single = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.len() <= MAX_ENTRY_CHARS {
        return (single, None);
    }
    let truncated: String = single.chars().take(MAX_ENTRY_CHARS).collect();
    (
        format!("{truncated}…"),
        Some(format!(
            "entry truncated, {} of {} chars shown",
            truncated.chars().count(),
            single.chars().count()
        )),
    )
}

/// Executes the `session_fetch` built-in tool.
///
/// # Errors
///
/// Never returns `Err`; failures are reported as `ToolResult` with
/// `success = false` carrying the reason.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let args_str = call.arguments.clone();
    let tool_call_id = call.id;
    let tool_name = call.name;

    Box::pin(async move {
        let fail = |msg: String| ToolResult {
            tool_call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            content: format!("Error: {msg}"),
            success: false,
            full_content: None,
            truncation: None,
            pin_position: None,
        };

        let args = match parse_args(&args_str) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };

        let Some(store) = ctx.session_store else {
            return fail("session store unavailable".to_owned());
        };

        let session_id = match resolve_session(args.0.clone(), ctx.session_id.as_ref()) {
            Ok(id) => id,
            Err(e) => return fail(e),
        };

        let window = match &args.1 {
            FetchMode::Anchored { entry_id, context } => {
                let anchor = jinn_domain::protocol::ChatEntryId::from(entry_id.clone());
                match store.fetch_window(&session_id, &anchor, *context).await {
                    Ok(v) => v,
                    Err(e) => return fail(format!("{e:?}")),
                }
            }
            FetchMode::Tail { limit } => match store.fetch_tail(&session_id, *limit).await {
                Ok(v) => v,
                Err(e) => return fail(format!("{e:?}")),
            },
        };

        let Some(window) = window else {
            return fail(format!(
                "session {session_id} not found (or the anchor entry is not part of it; \
 re-run session_search to confirm the entry id)"
            ));
        };
        if window.entries.is_empty() {
            return fail(format!("session {session_id} has no persisted entries"));
        }

        // Apply the standard outer caps (the `read` tool convention):
        // head-truncate the transcript and carry the unclipped text in
        // `full_content` so nothing is lost.
        let max_lines = ctx
            .max_output_lines
            .unwrap_or(jinn_tools_msg::truncation::DEFAULT_MAX_LINES);
        let max_bytes = ctx
            .max_output_bytes
            .unwrap_or(jinn_tools_msg::truncation::DEFAULT_MAX_BYTES);
        let full_content = format_window(&window);
        let truncation_result =
            jinn_tools_msg::truncation::truncate_head(&full_content, max_lines, max_bytes);

        ToolResult {
            tool_call_id,
            name: tool_name,
            content: truncation_result.content,
            success: true,
            full_content: truncation_result.truncated.then_some(full_content),
            truncation: truncation_result.meta,
            pin_position: None,
        }
    })
}

#[cfg(test)]
mod tests;
