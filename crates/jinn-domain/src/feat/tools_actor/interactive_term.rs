//! `interactive_term` built-in tool — spawns an interactive program in a PTY.
//!
//! Each call is **blocking**: it asks the [`InteractiveTermActor`] to spawn
//! the command and waits for the screen to settle (quiet window capped at
//! `max_wait`), then returns the rendered screen. The session persists in
//! the coordinator across calls; drive it with `interactive_term_send` and
//! clean up with `interactive_term_kill`.
//!
//! Unlike every other tool child, this one runs **with** a controlling
//! terminal (a PTY) — the deliberate inverse of the bash tool's isolation.

use std::time::Duration;

use futures::FutureExt;

use crate::feat::interactive_term::protocol::command::{SpawnTerm, SpawnTermOutcome};
use crate::feat::interactive_term::settle::default_max_wait;
use crate::feat::interactive_term::terminal_tab_state::DEFAULT_PTY_SIZE;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

use super::BoxedToolFuture;
use super::truncation::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};

/// Identifies the tool call for streaming output (watchdog keepalive).
#[derive(Debug, Clone)]
pub struct StreamCtx {
    /// The chat session running the tool call.
    pub session_id: crate::protocol::SessionId,
    /// The tool call the deltas are attributed to.
    pub tool_call_id: String,
}

/// Shared tool-call parsing across the three interactive-term tools.
pub(crate) mod parse {
    use serde_json::Value;

    /// Extracts a string field from the tool-call JSON.
    #[must_use]
    pub fn string_field(raw: &str, field: &str) -> Option<String> {
        let v: Value = serde_json::from_str(raw).ok()?;
        v.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    }

    /// Extracts a bool field, defaulting to false.
    #[must_use]
    pub fn bool_field(raw: &str, field: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        v.get(field).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Extracts an array-of-strings field (empty when absent).
    #[must_use]
    pub fn keys_field(raw: &str, field: &str) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return Vec::new();
        };
        v.get(field)
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Per-call settle budget (`max_duration_secs` maps onto it); the actor caps
/// at its own hard bound regardless.
const SPAWN_ASK_MARGIN: Duration = Duration::from_secs(5);

/// Returns the tool definition for the `interactive_term` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "interactive_term".to_owned(),
        description: "Spawn an INTERACTIVE terminal program (vim, psql, ssh, htop, a REPL) in a \
            pseudo-terminal and return its rendered screen. Use this instead of `bash` when the \
            command needs a full-screen TUI, cursor addressing, or incremental input. \
            \
            BLOCKING: returns once the screen output settles (quiet window, capped), so you see \
            the program's actual rendered state, not raw bytes. The session persists across tool \
            calls — the program keeps running after this returns. \
            \
            Send input via `interactive_term_send` (keys like \"enter\", \"ctrl+c\", \"f4\", text, \
            or both). Kill the session with `interactive_term_kill` when done. \
            Each chat session has AT MOST ONE terminal: spawning again kills the previous \
            program (unsaved work is lost) and reports it. \
            \
            TIMEOUT: default settle budget is 3s. Pass `max_duration_secs` to extend for \
            slow-starting programs (e.g. {\"max_duration_secs\": 30})."
            .to_owned(),
        prompt_snippet: Some(
            "Spawn interactive TUI programs (vim, psql, ssh, REPLs) in a PTY; returns the rendered screen"
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Prefer `bash` for one-shot commands; use this only when the program needs a full-screen TUI, cursor addressing, or incremental input.".to_owned(),
            "Each call BLOCKS until screen output settles and returns the rendered screen — call interactive_term_send afterwards to type text or press named keys (\"enter\", \"tab\", \"ctrl+c\", \"up\").".to_owned(),
            "The spawned program keeps running between calls; its state (REPL variables, vim buffers) persists. Kill the session with interactive_term_kill when finished.".to_owned(),
            "The user may take over the terminal from the TUI. If interactive_term_send reports the user has control, stop and wait — the screen will be delivered to you when they hand it back.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The interactive command to run (e.g. \"vim notes.txt\", \"psql -d mydb\")"
                },
                "max_duration_secs": {
                    "type": "number",
                    "description": "Maximum seconds to wait for the screen to settle. Default 3. Raise for slow-starting programs (e.g. 30 for a remote ssh)."
                }
            },
            "required": ["command"]
        }),
        server_tool_type: None,
    }
}

/// Executes the `interactive_term` built-in tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    let tool_call_id = call.id;
    let tool_name = call.name;

    // Every live terminal must be reachable by the user (overlay toggle key +
    // sidebar symbol), which requires a chat-session anchor. Spawning outside
    // a session context is rejected with an explanatory error. Validated
    // before the coordinator lookup so a rejected call never depends on
    // wiring availability.
    let Some(chat_session_id) = ctx.session_id.clone() else {
        return failure_future(
            &tool_call_id,
            &tool_name,
            "interactive_term requires a chat session context (none is active). \
             Run this tool from within a conversation session so the terminal \
             can be linked to it and shown in the TUI.",
        );
    };

    let Some(command) = parse::string_field(&call.arguments, "command") else {
        return failure_future(
            &tool_call_id,
            &tool_name,
            "missing required `command` argument",
        );
    };
    let max_wait = super::extract_max_duration(&call.arguments)
        .map_or_else(default_max_wait, Duration::from_secs);

    let Some(coordinator) = ctx.interactive_term.clone() else {
        return failure_future(
            &tool_call_id,
            &tool_name,
            "interactive-term coordinator is unavailable (this should only happen in tests)",
        );
    };

    // Defensive outer bound: settle cap + margin so a dead coordinator can't
    // hang the tool loop (mirrors the restart_mcp_server shape).
    let ask_timeout = max_wait + SPAWN_ASK_MARGIN;

    // Spawn size: the overlay's inner rect once known (WYSIWYG); the VT100
    // default before the first frame. Never zero — vt100 panics on a 0-row
    // grid (`last_layout_size` is (0, 0) until the overlay's first frame).
    let size = ctx.state.as_ref().map_or(DEFAULT_PTY_SIZE, |state| {
        let s = state.read();
        s.frontend.terminal.spawn_size()
    });

    // Stream context so the coordinator's settle wait emits deltas attributed
    // to this tool call (watchdog keepalive).
    let stream_ctx = ctx.session_id.clone().map(|session_id| StreamCtx {
        session_id,
        tool_call_id: tool_call_id.clone(),
    });

    async move {
        // `AskRequest` needs `.send()` to become a future.
        let ask_fut = coordinator
            .ask(SpawnTerm {
                chat_session_id: chat_session_id.clone(),
                command: command.clone(),
                cwd: ctx.cwd.clone(),
                size,
                max_wait,
            })
            .send();
        let replied =
            super::interactive_term::with_keepalive(ctx.bus, stream_ctx, ask_timeout, ask_fut)
                .await;

        // `None` = the defensive outer bound elapsed without a reply.
        let Some(replied) = replied else {
            return failure_result(
                &tool_call_id,
                &tool_name,
                &format!(
                    "interactive_term timed out after {ask_timeout:?} with no reply from the coordinator."
                ),
            );
        };

        let replied = match replied {
            Ok(replied) => replied,
            Err(_send_err) => {
                return failure_result(
                    &tool_call_id,
                    &tool_name,
                    "interactive-term coordinator is unreachable (mailbox failure).",
                );
            }
        };

        match replied {
            SpawnTermOutcome::Started {
                screen,
                killed_previous,
            } => success_result(
                &tool_call_id,
                &tool_name,
                &screen.screen,
                screen.exited.as_ref(),
                killed_previous.as_ref(),
            ),
            SpawnTermOutcome::Failed(msg) => {
                failure_result(&tool_call_id, &tool_name, &format!("failed to spawn `{command}`: {msg}"))
            }
        }
    }
    .boxed()
}

pub(crate) fn failure_future(
    tool_call_id: &str,
    tool_name: &str,
    msg: &str,
) -> std::pin::Pin<Box<dyn Future<Output = ToolResult> + Send>> {
    futures::future::ready(failure_result(tool_call_id, tool_name, msg)).boxed()
}

/// Builds a success [`ToolResult`] with the screen, exit info, and usage hints.
pub(crate) fn success_result(
    tool_call_id: &str,
    tool_name: &str,
    screen: &str,
    exited: Option<&crate::feat::interactive_term::pty_session::ExitInfo>,
    killed_previous: Option<&crate::feat::interactive_term::protocol::command::KilledPrevious>,
) -> ToolResult {
    let exit_line = exited
        .map(|info| format!("\n\nThe program has {0}.", info.summary()))
        .unwrap_or_default();
    let kill_line = killed_previous
        .map(|killed| {
            format!(
                "\n\nNOTE: This session already had a live terminal, which was \
                 killed to start this one ({}).",
                killed.exited.summary()
            )
        })
        .unwrap_or_default();
    let body = format!("{screen}{exit_line}{kill_line}\n\n{}", usage_footer());
    let truncated = truncate_tail(&body, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let full_content = truncated.truncated.then_some(body);
    ToolResult {
        tool_call_id: tool_call_id.to_owned(),
        name: tool_name.to_owned(),
        content: truncated.content,
        success: true,
        full_content,
        truncation: truncated.meta,
        pin_position: None,
    }
}

/// The usage footer appended to every interactive-term result.
pub(crate) fn usage_footer() -> String {
    "USAGE: send input with interactive_term_send; kill with interactive_term_kill.".to_owned()
}

/// Builds a failure [`ToolResult`].
pub(crate) fn failure_result(tool_call_id: &str, tool_name: &str, msg: &str) -> ToolResult {
    ToolResult {
        tool_call_id: tool_call_id.to_owned(),
        name: tool_name.to_owned(),
        content: msg.to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Runs `ask_fut`, publishing a heartbeat [`ToolExecutionOutput`] every
/// second so the stall watchdog sees the interactive call as active.
///
/// This is the tool-layer counterpart of the bash tool's output streaming:
/// the keepalive must originate where `tool_call_id` lives (the tool), not in
/// the coordinator actor. The pacer is aborted when the ask completes.
/// Returns `None` when the ask exceeded `ask_timeout`.
pub(crate) async fn with_keepalive<F, T>(
    bus: Option<crate::common::services::bus_service::BusService>,
    stream_ctx: Option<StreamCtx>,
    ask_timeout: Duration,
    ask_fut: F,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    let pacer = bus.zip(stream_ctx).map(|(bus, ctx)| {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately; skip it (the call just started).
            tick.tick().await;
            loop {
                tick.tick().await;
                bus.publish(
                    crate::feat::tools_actor::protocol::event::ToolExecutionOutput {
                        session_id: ctx.session_id.clone(),
                        tool_call_id: ctx.tool_call_id.clone(),
                        output: "…".to_owned(),
                        kind: crate::feat::tools_actor::protocol::event::ToolOutputKind::Normal,
                    },
                )
                .await;
            }
        })
    });
    let result = tokio::time::timeout(ask_timeout, ask_fut).await;
    if let Some(pacer) = &pacer {
        pacer.abort();
    }
    result.ok()
}

#[cfg(test)]
mod tests {
    use super::StreamCtx;
    use std::time::Duration;

    #[rstest::rstest]
    #[tokio::test]
    async fn keepalive_publishes_heartbeats_during_long_ask() {
        // Given a recording bus and a stream context.
        let (bus, audit) = crate::common::services::bus_service::BusService::new_recording();
        let ctx = Some(StreamCtx {
            session_id: crate::protocol::SessionId::new(),
            tool_call_id: "call-1".to_owned(),
        });

        // When wrapping an ask that takes longer than two heartbeat ticks.
        let result = super::with_keepalive(
            Some(bus),
            ctx,
            Duration::from_secs(10),
            tokio::time::sleep(Duration::from_millis(2500)),
        )
        .await;

        // Then the ask result passes through.
        assert!(result.is_some());
        // And the watchdog saw at least two heartbeat outputs.
        let beats =
            audit.of_type::<crate::feat::tools_actor::protocol::event::ToolExecutionOutput>();
        assert!(
            beats.len() >= 2,
            "expected >=2 heartbeats over 2.5s, got {}",
            beats.len()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn keepalive_without_session_publishes_nothing() {
        // Given a recording bus and no stream context (no chat session).
        let (bus, audit) = crate::common::services::bus_service::BusService::new_recording();

        // When wrapping the same long ask.
        let result = super::with_keepalive(
            Some(bus),
            None,
            Duration::from_secs(10),
            tokio::time::sleep(Duration::from_millis(1500)),
        )
        .await;

        // Then the ask result passes through.
        assert!(result.is_some());
        // And no heartbeat was published.
        assert!(
            audit
                .of_type::<crate::feat::tools_actor::protocol::event::ToolExecutionOutput>()
                .is_empty()
        );
    }
}
