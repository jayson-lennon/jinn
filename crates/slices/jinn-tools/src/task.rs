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
//! Spawns a regular session linked to the caller (empty history, empty task
//! list, inheriting the parent's model, CWD, persona, tools, skills, and MCP
//! servers), enqueues the given prompt into it, and blocks until the child
//! reaches [`PhaseKind::Idle`](jinn_session_msg::PhaseKind). The
//! child's last chat entry becomes the tool result. Subagents are just
//! sessions: they appear in the sidebar, can be steered, and persist.
//!
//! The task list is deliberately not inherited. A subagent receives its whole
//! assignment in the prompt, and a copied parent list is context it did not ask
//! for and cannot act on: it describes the parent's plan, not the subagent's
//! scope. Any todo work a subagent does must come from the prompt.
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

use crate::BoxedToolFuture;
use crate::task_phase_listener_actor::{TaskPhaseListenerActor, TaskPhaseListenerDeps};
use crate::task_settle_listener_actor::{TaskSettleListenerActor, TaskSettleListenerDeps};
use crate::tool_types::ToolContext;
use crate::tool_types::tool_error;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_core_types::{ChatEntry, ChatEntryKind, ModelSelection, NameFilter, SessionId};
use jinn_inference_msg::CancelTurn;
use jinn_session_lifecycle_msg::SessionCreated;
use jinn_session_state::ChatSessionState;

/// The `task` tool's registration name, shared by the registry and the
/// suppression sites: subagent spawn stamps it into the child's
/// `tool_filter`, fork strips it from the fork's set.
pub const TASK_TOOL_NAME: &str = "task";

/// Returns the tool definition for `task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TASK_TOOL_NAME.to_owned(),
        description: r#"
Delegate a self-contained task to a fresh subagent session.

Each call creates one subagent. To use multiple subagents, make multiple task
calls in the same assistant turn. Subagents inherit the model, cwd, tools,
skills, and MCP servers, but not the parent conversation history or the parent
task list — a subagent starts with an empty task list and receives its entire
assignment in the prompt. Only the subagent's final message is returned; its
intermediate tool calls and output remain in its own context.

WHEN TO USE:
 - open-ended exploration or research
 - multi-step work whose intermediate output you do not need
 - broad reviews, comparisons, investigations, or implementation work that contains multiple independent concerns
 - work that can be divided into separate, bounded workstreams

**AVOID SPAWNING INDIVIDUAL SUBAGENTS. PREFER SPAWNING 2+ SUBAGENTS WITH EXPLICIT BOUNDED TASKS**

PARALLEL DELEGATION DEFAULT:

For requests containing two or more independent workstreams, the default is
to delegate each workstream to a separate subagent and launch the first wave
concurrently.

Before making the first task call:
 1. Identify the relevant workstreams.
 2. Separate independent work from dependent work.
 3. For each independent workstream, write a distinct, self-contained brief.
 4. Emit all first-wave task calls in the same assistant turn.

Do not default to one subagent merely because this tool creates one subagent
per call. Multiple calls are the mechanism for fan-out.

A single call is appropriate when the work is genuinely indivisible, very
small, strictly sequential, or when delegation would cost more than doing the
work directly.

USE WAVES FOR DEPENDENT WORK:

For dependent tasks, launch only the independent portion of the current wave
concurrently. After collecting those results, determine the next wave from
their outputs.

The parent agent remains responsible for:
 - assigning clear boundaries to each workstream
 - collecting and comparing results
 - identifying duplicate or conflicting findings
 - resolving conflicts and integrating changes
 - performing final verification

Do not parallelize work that:
 - depends directly on another task's result
 - would duplicate substantially the same investigation
 - is too small to benefit from delegation
 - requires multiple agents to modify the same files concurrently

For concurrent code changes, assign disjoint files or scopes. Prefer parallel
read-only investigation when ownership is unclear; let the parent integrate
the changes.

EXAMPLES:
 - "Find relevant implementation files" + "Find relevant tests" -> two concurrent task calls.
 - "Review correctness" + "Review security" + "Review test coverage" -> three concurrent task calls.
 - "Investigate possible causes A and B" -> two concurrent calls, followed by parent synthesis.
 - "Implement A, then use A's result to implement B" -> sequential waves.
 - "Make changes to files A, B, C" -> three concurrent task calls.

PROMPT REQUIREMENTS:
 - The prompt must be self-contained: the subagent cannot see this conversation
   or the user's original intent.
 - State the goal, relevant context, constraints, scope, and expected result.
 - Avoid giving multiple overlapping workstreams to the same subagent.

TIMEOUT: unlimited by default. Pass max_duration_secs to bound the
subagent; on expiry the subagent is cancelled and a failure is returned.
        "# .to_owned(),



     prompt_snippet: Some(
       r#"Delegate one self-contained workstream to a fresh subagent. For multiple independent workstreams, make multiple task calls in the same assistant turn."#
            .to_owned(),
    ),

    prompt_guidelines: vec![
       r#"Before delegating, identify independent workstreams and their dependencies. If two or more workstreams can proceed without waiting for each other, launch them concurrently as separate task calls in the same assistant turn."#
            .to_owned(),

       r#"Treat fan-out as the default for multi-part work. Do not stop after one subagent when the request contains additional independent concerns. Use one subagent only when the work is tiny, indivisible, strictly sequential, or not worth coordinating."#
            .to_owned(),

       r#"Start with a small bounded fan-out, typically 2–4 subagents, and increase it only when the workstreams are substantial, clearly independent, and non-conflicting."#
            .to_owned(),

       r#"For dependent work, use waves: launch the independent tasks in the current wave, collect and synthesize their results, then launch the next wave."#
            .to_owned(),

       r#"Give each subagent a distinct scope. Avoid overlapping prompts, and do not ask multiple agents to edit the same files concurrently. Prefer read-only investigation when file ownership is unclear."#
            .to_owned(),

       r#"Write every subagent prompt as a complete brief: goal, context, constraints, relevant paths or symbols, whether to research or make changes, and the expected output."#
            .to_owned(),

       r#"A subagent starts with an empty task list; it does not see yours. Put the entire assignment in the prompt — the subagent must not be expected to read context from your task list, and its todo mutations never propagate back to you."#
            .to_owned(),
    ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Self-contained brief for the subagent. It sees nothing else — no conversation history, no user intent."
                },
                "description": {
                    "type": "string",
                    "description": "A 3-5 word summary of the task, shown as the subagent session's title in the sidebar."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the subagent. Defaults to this session's model. Omit field to inherit current session model. Do not provide empty strings or use 'inherit' as the model. This field is only when you need to explicitly set the model (you almost never need to do this)."
                },
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
        // A child has to carry a filter either way: the parent's may be
        // absent, and "no filter" cannot be edited to withhold the task tool.
        let mut child_filter = profile
            .tool_filter
            .clone()
            .unwrap_or_else(NameFilter::inherited);
        // Subagents cannot spawn further subagents unless re-enabled via the
        // tool picker; the stamp is per-session, so the picker reflects it.
        // Unconditional: even a re-enabled subagent's child starts suppressed.
        // Withheld through the filter rather than by inserting a name, so
        // this reads correctly whichever mode the parent carried — inserting
        // into an allow list would instead strip the child of that one tool
        // while leaving everything else permitted.
        child_filter.withhold(crate::task::TASK_TOOL_NAME);
        p.tool_filter = Some(child_filter);
        p.skill_filter.clone_from(&profile.skill_filter);
    }
    child.set_cwd(parent.cwd().to_path_buf());
    // Subagents inherit the parent's project association (stamped at the
    // parent's creation; the child's cwd may differ but the project does not).
    child.set_project(parent.project().map(std::path::Path::to_path_buf));
    // Home resolves fresh at creation in every other path (runtime-only,
    // not persisted); the `task` tool's ctx carries the app paths.
    child.set_home(app_home.to_path_buf());
    child.set_enabled_mcp_servers(parent.enabled_mcp_servers().clone());
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
    system: &trouper::system::ActorSystem,
    bus: &jinn_kernel::common::services::bus_service::BusService,
    child_id: &SessionId,
    expected_servers: &std::collections::BTreeSet<String>,
    budget: Duration,
) {
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
    let listener = TaskSettleListenerActor::spawn(TaskSettleListenerDeps {
        system: system.clone(),
        bus: bus.clone(),
        child_id: child_id.clone(),
        expected_servers: expected_servers.clone(),
        settled: settled_tx,
    })
    .await;
    // Ok(quorum met) or Err(budget elapsed): both proceed. On expiry the
    // receiver drops with this future and the listener notices the closed
    // channel on its next event.
    let _ = tokio::time::timeout(budget, settled_rx).await;
    // Stop a listener that lingered past the budget. Idempotent: it already
    // self-stopped on quorum, which surfaces as a stop of a stopped path.
    system.stop(&listener).await;
}

/// Result of the await step.
async fn await_child(
    bus: &jinn_kernel::common::services::bus_service::BusService,
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
                bus.publish(CancelTurn {
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
        return tool_error(&call, "no application state available");
    };
    let Some(parent_id) = ctx.session_id else {
        return tool_error(&call, "no session ID available");
    };
    let Some(bus) = ctx.bus else {
        return tool_error(&call, "no message bus available");
    };
    let args = match parse_args(&call.arguments) {
        Ok(args) => args,
        Err(msg) => return tool_error(&call, &msg),
    };
    let deadline = args
        .max_duration_secs
        .filter(|s| *s > 0)
        .map(Duration::from_secs);

    // Snapshot the parent and build the child under the read lock.
    let child = {
        let guard = state.read();
        let Some(parent) = guard.session.get(&parent_id) else {
            return tool_error(&call, "parent session not found in state");
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
    state.with_session(|view| {
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
    let Some(system) = ctx.trouper_system.clone() else {
        return tool_error(&call, "no actor system available");
    };
    let _phase_listener = TaskPhaseListenerActor::spawn(TaskPhaseListenerDeps {
        system: system.clone(),
        bus: bus.clone(),
        child_id: child_id.clone(),
        completion: completion_tx,
    })
    .await;

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
    await_discovery_settlement(&system, &bus, &child_id, &expected_servers, SETTLE_BUDGET).await;

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
