// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! `task` built-in tool — delegate a sub-task to a fresh subagent session.
//!
//! Spawns a regular session linked to the caller (empty history, inheriting
//! the parent's model, CWD, persona, tools, skills, and MCP servers), enqueues
//! the given prompt into it, and blocks until the child reaches
//! [`PhaseKind::Idle`](jinn_domain::feat::session::phase_machine::PhaseKind). The
//! child's last chat entry becomes the tool result. Subagents are just
//! sessions: they appear in the sidebar, can be steered, and persist.
//!
//! Ordering guarantees: both listeners (completion and discovery settlement)
//! are spawned and subscribed before `SessionCreated` is published (other
//! actors react to that event), and the in-flight spawn is registered before
//! the wait begins (so the stall watchdog sees the suspended parent). The
//! default duration is unlimited; `max_duration_secs` overrides per call. The
//! deadline is managed *here*, not by the dispatcher's outer timeout wrapper —
//! that wrapper drops its future on expiry, which would orphan the child. On
//! deadline expiry this tool cancels the child itself.
//!
//! Between `SessionCreated` and the first `EnqueueUserMessage`, the spawn
//! waits for the child's discovery to settle — the context-files, skills, and
//! prompt-template scans plus a terminal status from every enabled MCP
//! server — bounded by [`SETTLE_BUDGET`]. The gate only delays: on expiry the
//! message is sent regardless, so a hung discovery never blocks the child.

use std::time::Duration;

use kameo::actor::Spawn;

use crate::BoxedToolFuture;
use crate::task_phase_listener_actor::{TaskPhaseListenerActor, TaskPhaseListenerDeps};
use crate::task_settle_listener_actor::{TaskSettleListenerActor, TaskSettleListenerDeps};
use crate::tool_types::ToolContext;
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_domain::feat::chat_input::protocol::command::EnqueueUserMessage;
use jinn_domain::feat::session::chat_session::ChatSessionState;
use jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated;
use jinn_domain::protocol::SessionId;
use jinn_domain::protocol::{ChatEntry, ChatEntryKind};
use jinn_inference_msg::CancelStream;

/// The `task` tool's registration name, shared by the registry and the
/// suppression sites: subagent spawn stamps it into the child's
/// `disabled_tools`, fork strips it from the fork's set.
pub const TASK_TOOL_NAME: &str = "task";

/// Returns the tool definition for `task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TASK_TOOL_NAME.to_owned(),
        description: "Delegate a task to a fresh subagent session and block until it finishes. \
            Spawns a new session inheriting your model, cwd, tools, skills, MCP servers, and a \
            snapshot of your current task list — \
            but with empty history: it sees only the prompt you give it. When its session goes \
            idle, its final chat entry is returned as this tool's result. \
            \
            WHEN TO USE: open-ended search or exploration where the first try may miss; \
            multi-step sub-tasks whose intermediate tool output you don't need; \
            independent sub-tasks in parallel. \
            \
            WHEN NOT TO USE: reading a specific file or symbol (use read/grep directly); \
            one obvious tool call; tasks that depend on each other's results. \
            \
            Usage notes: \
            (1) The prompt must be self-contained — the subagent cannot see this conversation \
            or the user's intent; say whether it should research or make changes. \
            (2) Launch independent tasks concurrently — multiple task calls in one message. \
            (3) The user can watch and steer the subagent live; cancelling it returns the \
            cancel to you as the result. \
            (4) Only the final message comes back; intermediate work stays in the subagent's \
            session. \
            \
            TIMEOUT: unlimited by default. \
            Pass `max_duration_secs` to bound the subagent; on expiry the subagent is \
            cancelled and a failure is returned."
            .to_owned(),
        prompt_snippet: Some(
            "Spawn a subagent session for a self-contained sub-task and await its result"
                .to_owned(),
        ),
        prompt_guidelines: vec![
            "Prefer task for open-ended exploration; keep focused lookups (read/grep) in \
            your own context."
                .to_owned(),
            "Write the prompt as a complete brief: goal, constraints, and whether to \
            research or make changes. Include a description so the user can follow along \
            in the sidebar."
                .to_owned(),
            "The subagent inherits a snapshot of your task list as of spawn and owns that \
            copy — its todo mutations never propagate back to you; reconcile your own list \
            from its result. If it doesn't need the list, it can clear it with an empty \
            todo_set_list."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Self-contained brief for the subagent. It sees nothing \
                    else — no conversation history, no user intent."
                },
                "description": {
                    "type": "string",
                    "description": "A 3-5 word summary of the task, shown as the subagent \
                    session's title in the sidebar."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model id for the subagent. Defaults to this \
                    session's model."
                },
                "max_duration_secs": {
                    "type": "number",
                    "description": "Maximum duration in seconds to wait for the subagent. \
                    Unlimited by default; 0 also means unlimited. On expiry the subagent \
                    session is cancelled and a failure is returned."
                }
            },
            "required": ["prompt"]
        }),
        server_tool_type: None,
    }
}

/// Parsed `task` tool arguments.
struct TaskArgs {
    /// The self-contained brief for the subagent.
    prompt: String,
    /// Optional 3-5 word title for the child session.
    description: Option<String>,
    /// Optional model override.
    model: Option<String>,
    /// Optional wait budget in seconds; `None` (or 0) means unlimited.
    max_duration_secs: Option<u64>,
}

/// Parses the tool call arguments.
fn parse_args(raw: &str) -> Result<TaskArgs, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON arguments: {e}"))?;
    let prompt = v
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    if prompt.is_empty() {
        return Err("prompt is empty; provide a self-contained brief for the subagent".to_owned());
    }
    let model = v
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    let description = v
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    let max_duration_secs = crate::orchestrator::extract_max_duration(raw);
    Ok(TaskArgs {
        prompt,
        description,
        model,
        max_duration_secs,
    })
}

/// Executes the `task` tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move { run(call, ctx).await })
}

/// How the awaited child run ended.
enum ChildOutcome {
    /// The child reached `Idle` — read its last entry.
    Finished,
    /// The wait budget expired — the child was cancelled.
    TimedOut(u64),
    /// The completion channel closed without a signal (listener died).
    ListenerGone,
}

/// Builds the child session linked to `parent_id` with inherited config.
fn build_child(
    parent: &ChatSessionState,
    parent_id: &SessionId,
    args: &TaskArgs,
    app_home: &std::path::Path,
) -> ChatSessionState {
    let mut child = ChatSessionState::new_child(parent_id, true);
    let profile = parent.profile();
    let model = args
        .model
        .clone()
        .map_or_else(|| profile.model.clone(), ModelSelection::Single);
    {
        let p = child.profile_mut();
        p.model = model;
        p.persona_name.clone_from(&profile.persona_name);
        p.reasoning_effort = profile.reasoning_effort;
        p.endpoint.clone_from(&profile.endpoint);
        p.disabled_tools.clone_from(&profile.disabled_tools);
        // Subagents cannot spawn further subagents unless re-enabled via the
        // tool picker; the stamp is per-session, so the picker reflects it.
        // Unconditional: even a re-enabled subagent's child starts suppressed.
        p.disabled_tools
            .insert(crate::task::TASK_TOOL_NAME.to_owned());
        p.disabled_skills.clone_from(&profile.disabled_skills);
    }
    child.set_cwd(parent.cwd().to_path_buf());
    // Subagents inherit the parent's project association (stamped at the
    // parent's creation; the child's cwd may differ but the project does not).
    child.set_project(parent.project().map(std::path::Path::to_path_buf));
    // Home resolves fresh at creation in every other path (runtime-only,
    // not persisted); the `task` tool's ctx carries the app paths.
    child.set_home(app_home.to_path_buf());
    child.set_enabled_mcp_servers(parent.enabled_mcp_servers().clone());
    // Snapshot the parent's task list into the child: parent and child own
    // fully independent copies after spawn — child mutations never propagate
    // back to the parent.
    *child.task_list_mut() = parent.task_list().clone();
    let title = args
        .description
        .clone()
        .unwrap_or_else(|| title_from_prompt(&args.prompt));
    child.set_title(title);
    // Persistable: the child has been meaningfully created even though no
    // user keystrokes landed in it. Without this the child vanishes from
    // disk on archive.
    child.mark_interacted();
    child
}

/// Derives a fallback session title from the first line of the prompt.
fn title_from_prompt(prompt: &str) -> String {
    prompt
        .lines()
        .next()
        .unwrap_or("subagent task")
        .chars()
        .take(40)
        .collect()
}

/// Backstop for discovery events that never arrive. The expected path is
/// milliseconds — bounded directory walks and local MCP handshakes.
const SETTLE_BUDGET: Duration = Duration::from_secs(15);

/// Waits until the child's discovery quorum settles, or `budget` expires —
/// whichever comes first. Either way the caller proceeds to enqueue.
///
/// Spawns a [`TaskSettleListenerActor`] subscribed to the child's discovery
/// events (context files, skills, prompt templates, MCP server statuses) and
/// races its oneshot against the budget. Must be called *after* the actor's
/// subscriptions are live but *before* `EnqueueUserMessage` is published.
pub(crate) async fn await_discovery_settlement(
    bus: &jinn_domain::common::services::bus_service::BusService,
    child_id: &SessionId,
    expected_servers: &std::collections::BTreeSet<String>,
    budget: Duration,
) {
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let listener = TaskSettleListenerActor::spawn(TaskSettleListenerDeps {
        bus: bus.clone(),
        child_id: child_id.clone(),
        expected_servers: expected_servers.clone(),
        settled: settled_tx,
    });
    listener.wait_for_startup().await;
    // Ok(quorum met) or Err(budget elapsed): both proceed. On expiry the
    // receiver drops with this future and the listener notices the closed
    // channel on its next event.
    let _ = tokio::time::timeout(budget, settled_rx).await;
    // Stop a listener that lingered past the budget. Idempotent: it already
    // self-stopped on quorum, which surfaces as a send error — ignore.
    let _ = listener.stop_gracefully().await;
}

/// Result of the await step.
async fn await_child(
    bus: &jinn_domain::common::services::bus_service::BusService,
    completion: tokio::sync::oneshot::Receiver<()>,
    child_id: SessionId,
    deadline: Option<Duration>,
) -> ChildOutcome {
    let wait = async {
        // `send` fails only if the listener died; closing without a signal
        // is itself a signal of abnormal termination.
        match completion.await {
            Ok(()) => ChildOutcome::Finished,
            Err(_) => ChildOutcome::ListenerGone,
        }
    };
    match deadline {
        None => wait.await,
        Some(budget) => match tokio::time::timeout(budget, wait).await {
            Ok(outcome) => outcome,
            Err(_) => {
                bus.publish(CancelStream {
                    session_id: child_id,
                })
                .await;
                ChildOutcome::TimedOut(budget.as_secs())
            }
        },
    }
}

/// Classifies the child's final entry into a forwarded tool result message
/// and success flag.
fn classify_final_entry(entry: &ChatEntry) -> (bool, String) {
    match &entry.kind {
        ChatEntryKind::Error(text) => (false, text.clone()),
        _ => (true, entry.text()),
    }
}

async fn run(call: ToolCall, ctx: ToolContext) -> ToolResult {
    // Fail fast on missing context, mirroring restart_mcp.
    let Some(state) = ctx.state else {
        return tool_error(call, "no application state available");
    };
    let Some(parent_id) = ctx.session_id else {
        return tool_error(call, "no session ID available");
    };
    let Some(bus) = ctx.bus else {
        return tool_error(call, "no message bus available");
    };
    let Some(session_cap) = ctx.session_cap else {
        return tool_error(call, "no session authority available");
    };
    let args = match parse_args(&call.arguments) {
        Ok(args) => args,
        Err(msg) => return tool_error(call, &msg),
    };
    let deadline = args
        .max_duration_secs
        .filter(|s| *s > 0)
        .map(Duration::from_secs);

    // Snapshot the parent and build the child under the read lock.
    let child = {
        let guard = state.read();
        let Some(parent) = guard.session.get(&parent_id) else {
            return tool_error(call, "parent session not found in state");
        };
        build_child(parent, &parent_id, &args, ctx.app_paths.home_dir())
    };
    let child_id = child.session_id().clone();
    let child_cwd = child.cwd().to_path_buf();
    // The settle gate's expectation set, frozen at spawn: the MCP coordinator
    // reconciles exactly the child's enabled set on SessionCreated, so these
    // are the servers whose terminal statuses the gate waits on.
    let expected_servers = child.enabled_mcp_servers().clone();

    // Insert the child before any event flies. Every later writer (session
    // actor on EnqueueUserMessage, MCP coordinator on SessionCreated) looks
    // the session up by id — insertion must precede publication or they
    // would each `get_or_create` a bare session over the real child.
    state.with_session(&session_cap, |view| {
        let map = view.session.map();
        map.insert(child);
        // Stamp the parent's tool-call entry with the child link. The UI
        // reads the entry to offer "open subagent session" and to show the
        // waiting line. The tool-call entry exists by the time this future
        // runs (the executor only starts after the provider finished
        // streaming the call); a missing entry is a defensive no-op.
        if let Some(parent) = map.get_mut(&parent_id) {
            parent.edit_history().with_last_matching_mut(
                |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id, .. } if id == &call.id),
                |entry| {
                    if let ChatEntryKind::ToolCall { child_session, .. } = &mut entry.kind {
                        *child_session = Some(child_id.clone());
                    }
                },
            );
        }
    });

    // Register the in-flight pair before blocking so the stall watchdog sees
    // the suspended parent. The guard covers success, failure, and abort
    // (parent tool-call future dropped) paths.
    let registry = ctx.task_spawns.clone().unwrap_or_default();
    let guard = registry.guard(parent_id.clone(), child_id.clone());

    // Listener before publication: guarantees the Idle subscription exists
    // before SessionCreated/EnqueueUserMessage can trigger any phase change.
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    let listener = TaskPhaseListenerActor::spawn(TaskPhaseListenerDeps {
        bus: bus.clone(),
        child_id: child_id.clone(),
        completion: completion_tx,
    });
    listener.wait_for_startup().await;

    // Publish: lifecycle actors react to SessionCreated (MCP reconcile,
    // scans, persistence); the session actor turns EnqueueUserMessage into
    // the child's first dispatch.
    bus.publish(SessionCreated {
        session_id: child_id.clone(),
        cwd: child_cwd,
    })
    .await;

    // Settle gate: give the discovery actors (context files, skills, prompt
    // templates, MCP servers) a bounded chance to land so the child's first
    // prompt is complete. Only delays — never fails the spawn.
    await_discovery_settlement(&bus, &child_id, &expected_servers, SETTLE_BUDGET).await;

    bus.publish(EnqueueUserMessage {
        session_id: child_id.clone(),
        entry: ChatEntry::user(args.prompt.clone()),
    })
    .await;

    let outcome = await_child(&bus, completion_rx, child_id.clone(), deadline).await;
    guard.defuse();

    match outcome {
        ChildOutcome::Finished => {
            let (success, content) = {
                let snapshot = state.read();
                snapshot
                    .session
                    .get(&child_id)
                    .and_then(|child| child.history().last())
                    .map_or_else(
                        || (false, "subagent produced no output".to_owned()),
                        classify_final_entry,
                    )
            };
            forward(call, success, content)
        }
        ChildOutcome::TimedOut(secs) => forward(
            call,
            false,
            format!(
                "Subagent task timed out after {secs}s and was cancelled. \
                 Retry with a larger max_duration_secs value, or split the task."
            ),
        ),
        ChildOutcome::ListenerGone => forward(
            call,
            false,
            "Subagent completion listener terminated unexpectedly.".to_owned(),
        ),
    }
}

/// Builds the final [`ToolResult`].
fn forward(call: ToolCall, success: bool, content: String) -> ToolResult {
    ToolResult {
        tool_call_id: call.id,
        name: call.name,
        content,
        success,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

fn tool_error(call: ToolCall, msg: &str) -> ToolResult {
    forward(call, false, format!("Error: {msg}"))
}
