//! `interactive_term_kill` built-in tool — terminates this session's own
//! terminal's process group and collects the final state.
//!
//! Idempotent: killing an already-exited terminal still succeeds, returning
//! the cached exit info, final screen, and transcript tail. Killing when the
//! session has no terminal is an error result. The tool always targets the
//! **calling chat session's** terminal (resolved from the tool context) —
//! there is no cross-session addressing.

use std::time::Duration;

use futures::FutureExt;

use crate::feat::interactive_term::protocol::command::KillTermOutcome;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

use super::BoxedToolFuture;
use super::interactive_term::failure_result;

/// Defensive outer bound on the kill ask.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// Returns the tool definition for the `interactive_term_kill` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "interactive_term_kill".to_owned(),
        description: "Kill this session's running `interactive_term` terminal (its whole process \
            group) and receive the final screen, a transcript tail, and the exit code. Use this \
            to clean up when you are done with an interactive program (close vim, exit ssh, stop \
            htop). Safe to call on an already-exited terminal — it returns the recorded final \
            state."
            .to_owned(),
        prompt_snippet: Some(
            "Kill this session's interactive_term terminal (whole process group) and get the final state"
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Kill interactive_term terminals when done with them so no processes are left running."
                .to_owned(),
            "Killing an already-exited terminal is safe — it returns the recorded final state."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
        server_tool_type: None,
    }
}

/// Executes the `interactive_term_kill` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let tool_call_id = call.id;
    let tool_name = call.name;

    // Target: the calling chat session's own terminal. There is no
    // model-facing session argument — cross-session addressing cannot happen.
    let Some(chat_session_id) = ctx.session_id.clone() else {
        return super::interactive_term::failure_future(
            &tool_call_id,
            &tool_name,
            "interactive_term_kill requires a chat session context (none is active). \
             Spawn a terminal with interactive_term from within a conversation session first.",
        );
    };

    let Some(coordinator) = ctx.interactive_term else {
        return super::interactive_term::failure_future(
            &tool_call_id,
            &tool_name,
            "interactive-term coordinator is unavailable (this should only happen in tests)",
        );
    };

    async move {
        let outcome =
            tokio::time::timeout(ASK_TIMEOUT, coordinator.kill_term(chat_session_id.clone())).await;

        let replied = match outcome {
            Ok(Ok(replied)) => replied,
            Ok(Err(_send_err)) => {
                return failure_result(
                    &tool_call_id,
                    &tool_name,
                    "interactive-term coordinator is unreachable (mailbox failure).",
                );
            }
            Err(_) => {
                return failure_result(
                    &tool_call_id,
                    &tool_name,
                    "interactive_term_kill timed out with no reply from the coordinator.",
                );
            }
        };

        match replied {
            KillTermOutcome::Killed {
                screen,
                transcript_tail,
                exited,
            } => {
                let body = {
                    let mut body = format!(
                        "Session terminated ({summary}).\n\n\
                         == FINAL SCREEN ==\n{screen}\n",
                        summary = exited.summary(),
                    );
                    if !transcript_tail.trim().is_empty() {
                        body.push_str("\n== TRANSCRIPT TAIL ==\n");
                        body.push_str(&transcript_tail);
                        body.push('\n');
                    }
                    body.push_str(
                        "\nSession closed. Spawn a new one with interactive_term if needed.",
                    );
                    body
                };
                ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    content: body,
                    success: true,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                }
            }
            KillTermOutcome::UnknownSession => failure_result(
                &tool_call_id,
                &tool_name,
                "no live terminal for this session — spawn one with interactive_term.",
            ),
        }
    }
    .boxed()
}
