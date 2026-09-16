//! `restart_mcp_server` built-in tool — lets an LLM restart a dead MCP server
//! by name (or by stripping a `mcp__<server>__<tool>` namespace) and learn
//! whether the restart succeeded before retrying the original tool call.
//!
//! Architecture: this tool `ask`s the `McpCoordinatorActor` directly
//! (request/reply). The coordinator's `restart_one` kills the old actor,
//! spawns a fresh one, awaits its `on_start`, and queries its `ConnectionState`
//! to report success/failure deterministically. No bus eavesdropping, no
//! event-ordering race. The coordinator owns the timeout (60s); this tool just
//! awaits the reply.
//!
//! See the plan at `.plans/mcp-restart-tool/plan.md`.

use std::time::Duration;

use futures::FutureExt;

use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};
use jinn_slices::RestartError;

use super::BoxedToolFuture;

/// Defensive outer bound on the `ask`. The coordinator's own `restart_one`
/// already bounds at 60s; this catches a coordinator that never replies
/// (e.g. it died) so the tool loop can't hang forever. Generously above the
/// coordinator's bound to avoid racing it.
// Outer bound surfaced in the timeout error text; the actual bound lives
// in the handle impl (jinn-mcp-slice) with the same duration.
const ASK_TIMEOUT: Duration = Duration::from_secs(75);

/// Returns the tool definition for the `restart_mcp_server` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "restart_mcp_server".to_owned(),
        description: "Restart a dead or broken MCP server so its tools work again. \
            Call this when an `mcp__<server>__*` tool call fails (the server has likely died). \
            Pass the server's name (e.g. `excalimate`) — a full `mcp__<server>__<tool>` tool name \
            is also accepted and silently resolved. This waits until the server has finished \
            restarting (up to 60s for slow-boot servers), then returns whether it is back online. \
            After a successful restart, retry the original tool call. \
            **If this tool reports failure, STOP and wait for user instruction — do not retry.**"
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "The MCP server to restart, by name (e.g. `excalimate`), \
                        or the failing `mcp__<server>__<tool>` tool name (the namespace is stripped)."
                }
            },
            "required": ["server"]
        }),
        server_tool_type: None,
    }
}

/// Executes the `restart_mcp_server` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let tool_call_id = call.id;
    let tool_name = call.name;
    let args_str = call.arguments;

    // Resolve prerequisites up front; surface clear failures.
    let Some(coordinator) = ctx.mcp_coordinator else {
        return futures::future::ready(failure_result(
            &tool_call_id,
            &tool_name,
            "MCP coordinator is unavailable (this should only happen in tests)",
        ))
        .boxed();
    };
    let Some(state) = ctx.state else {
        return failure_future(&tool_call_id, &tool_name, "no application state available");
    };
    let Some(session_id) = ctx.session_id else {
        return failure_future(&tool_call_id, &tool_name, "no active session");
    };

    let server = match parse_args(&args_str, &state) {
        Ok(server) => server,
        Err(msg) => return failure_future(&tool_call_id, &tool_name, &msg),
    };

    async move {
        //
        // kameo flattens a `Result<(), RestartError>` Reply: awaiting yields
        // `Result<(), SendError<M, RestartError>>`, where
        // `SendError::HandlerError(e)` carries our domain error variants.
        // The handle bounds the ask internally (old ASK_TIMEOUT semantics
        // moved into the seam): Timeout/Mailbox surface as domain errors.
        match coordinator.restart(session_id.clone(), server.clone()).await {
            // Outer timeout: coordinator never replied.
            Err(RestartError::Timeout) => {
                return failure_result(
                    &tool_call_id,
                    &tool_name,
                    &format!(
                        "MCP restart timed out after {ASK_TIMEOUT:?} with no reply from the coordinator. \
                         **STOP and wait for user instruction.**"
                    ),
                );
            }
            // Delivery failure (actor stopped / mailbox full / ask timeout).
            Err(RestartError::Mailbox) => {
                return failure_result(
                    &tool_call_id,
                    &tool_name,
                    "MCP coordinator is unreachable (mailbox failure). \
                     **STOP and wait for user instruction.**",
                );
            }
            // Domain-level failure from the coordinator's restart_one.
            Err(domain_err) => {
                return domain_failure_result(&tool_call_id, &tool_name, &server, &domain_err);
            }
            // Success: server reconnected.
            Ok(()) => {}
        }

        ToolResult {
            tool_call_id,
            name: tool_name,
            content: format!(
                "MCP server `{server}` restarted successfully and is back online. \
                 MCP tools are available again — retry your original tool call."
            ),
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    }
    .boxed()
}

// ---------------------------------------------------------------------------
// Server-name resolution + arg parsing
// ---------------------------------------------------------------------------

/// Parses the tool's `server` argument and resolves it to a configured server.
///
/// Accepts either a bare server name (`excalimate`) or a full
/// `mcp__<server>__<tool>` tool name (the namespace is stripped silently).
/// Returns `Err(message)` on missing/invalid args or an unknown server.
fn parse_args(args_str: &str, state: &crate::common::state::State) -> Result<String, String> {
    let value: serde_json::Value = if args_str.trim().is_empty() {
        return Err("missing `server` argument".to_owned());
    } else {
        serde_json::from_str(args_str).map_err(|e| format!("invalid arguments: {e}"))?
    };
    let Some(input) = value.get("server").and_then(serde_json::Value::as_str) else {
        return Err("`server` argument must be a string".to_owned());
    };
    let configured = configured_server_names(state);
    resolve_server(input, &configured).ok_or_else(|| format!("unknown MCP server `{input}`"))
}

/// Reads the configured server names from application state.
fn configured_server_names(state: &crate::common::state::State) -> Vec<String> {
    state
        .read()
        .frontend
        .preferences
        .mcp_server
        .keys()
        .cloned()
        .collect()
}

/// Resolves a raw input to a configured server name.
///
/// - Exact match against `configured` → that name.
/// - `mcp__<X>__<rest>` form where `X` is configured → `X`.
/// - Otherwise `None`.
fn resolve_server(input: &str, configured: &[String]) -> Option<String> {
    // Exact match.
    if let Some(found) = configured.iter().find(|c| c.as_str() == input) {
        return Some(found.clone());
    }
    // Namespace-stripped form: mcp__<server>__<tool>.
    if let Some(rest) = input.strip_prefix("mcp__")
        && let Some((server, _tool)) = rest.split_once("__")
        && configured.iter().any(|c| c == server)
    {
        return Some(server.to_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// Result helpers
// ---------------------------------------------------------------------------

fn failure_future(call_id: &str, name: &str, message: &str) -> BoxedToolFuture {
    futures::future::ready(failure_result(call_id, name, message)).boxed()
}

fn failure_result(call_id: &str, name: &str, message: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.to_owned(),
        name: name.to_owned(),
        content: message.to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Maps a domain-level `RestartError` from the coordinator into a failure
/// result carrying the STOP-and-wait instruction.
fn domain_failure_result(
    call_id: &str,
    name: &str,
    server: &str,
    err: &RestartError,
) -> ToolResult {
    let content = match err {
        RestartError::UnknownServer => format!("unknown MCP server `{server}`"),
        RestartError::ConnectFailed => format!(
            "MCP server `{server}` failed to restart — it is dead. \
             **STOP and wait for user instruction.** Do not retry; the user \
             should check the server config in the inspector (`<leader>sM`)."
        ),
        RestartError::Timeout => format!(
            "MCP server `{server}` did not finish starting within 60s — it is likely a \
             slow-to-boot server that hasn't come up yet. \
             **STOP and wait for user instruction.** The user can check its status in the \
             inspector (`<leader>sM`) and retry once it shows `running`."
        ),
        RestartError::Mailbox => "the restarted MCP actor stopped mid-restart (mailbox failure). \
             **STOP and wait for user instruction.**"
            .to_owned(),
    };
    failure_result(call_id, name, &content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::state::State;

    fn configured() -> Vec<String> {
        vec!["excalimate".to_owned(), "context7".to_owned()]
    }

    #[rstest::rstest]
    #[test]
    fn resolve_server_exact_match_returns_name() {
        // Given a configured server list and a bare server name.
        // When resolving.
        let resolved = resolve_server("excalimate", &configured());

        // Then the bare name is returned.
        assert_eq!(resolved.as_deref(), Some("excalimate"));
    }

    #[rstest::rstest]
    #[test]
    fn resolve_server_strips_mcp_namespace() {
        // Given a configured server list and a full mcp__ namespaced tool name.
        // When resolving.
        let resolved = resolve_server("mcp__excalimate__create_scene", &configured());

        // Then the server name is extracted from the namespace.
        assert_eq!(resolved.as_deref(), Some("excalimate"));
    }

    #[rstest::rstest]
    #[test]
    fn resolve_server_returns_none_for_unknown_server() {
        // Given a name that is not configured.
        // When resolving.
        let resolved = resolve_server("nonexistent", &configured());

        // Then it returns None.
        assert!(resolved.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn resolve_server_returns_none_for_unknown_namespace() {
        // Given a namespaced tool whose server is not configured.
        // When resolving.
        let resolved = resolve_server("mcp__unknown__tool", &configured());

        // Then it returns None.
        assert!(resolved.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_missing_server_argument() {
        // Given a state with configured servers but no server arg.
        let state = State::new(crate::common::app_state::AppState::default());

        // When parsing empty args.
        let result = parse_args("", &state);

        // Then it errors with a clear message.
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("server"),
            "error should mention server"
        );
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_non_string_server() {
        // Given a state and a numeric server arg.
        let state = State::new(crate::common::app_state::AppState::default());

        // When parsing args with a non-string server.
        let result = parse_args(r#"{"server": 42}"#, &state);

        // Then it errors.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn parse_args_rejects_unknown_server() {
        // Given a state and an unconfigured server name.
        let state = State::new(crate::common::app_state::AppState::default());

        // When parsing args with an unknown server.
        let result = parse_args(r#"{"server": "ghost"}"#, &state);

        // Then it errors with the server name in the message.
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ghost"));
    }

    #[rstest::rstest]
    #[test]
    fn definition_has_teaching_description_with_stop_instruction() {
        // Given the tool definition.
        // When reading its description.
        let def = definition();

        // Then it teaches when to call and includes the STOP instruction.
        assert_eq!(def.name, "restart_mcp_server");
        assert!(
            def.description.contains("mcp__"),
            "should teach the model to call on mcp tool failure"
        );
        assert!(
            def.description.contains("STOP"),
            "should include the STOP-and-wait instruction"
        );
    }
}
