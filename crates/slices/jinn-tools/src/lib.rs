//! Tool execution — the agent's hands.
//!
//! This slice owns the whole tool family: the orchestrator actor that
//! dispatches `ExecuteToolBatch` requests, every built-in tool (file
//! editing, shell, search, plans, skills, sessions, terminals), the todo
//! tools that drive the session task list, and the `task` subagent
//! machinery with its phase/settle listeners. Protocol contracts and
//! payload types live in `jinn-tools-msg`; the tool nouns
//! (`ToolDefinition`/`ToolCall`/`ToolResult`) live in `jinn-core-types`.
//!
//! The orchestrator maintains a registry of available tools (built-in and
//! actor-provided), dispatches batches, and emits `ToolBatchCompleted`
//! when all calls finish. Actor-provided tools are routed via `ExecuteTool`
//! commands on the bus.

pub use jinn_tools_msg::BoxedToolFuture;

pub mod bash;
pub mod command_policy;
pub mod edit;
pub mod get_time;
pub mod grep;
pub(crate) mod input_bounds;
pub mod interactive_term;
pub mod interactive_term_kill;
pub mod interactive_term_send;
pub mod read;
pub mod registry;
pub mod restart_mcp;
pub mod save_plan;
pub mod session_fetch;
pub mod session_search;
pub mod skill;
pub mod task;
pub mod task_phase_listener_actor;
pub mod task_settle_listener_actor;
pub mod todo_tools;
pub mod tool_types;
pub(crate) mod visible_lines;
pub mod write;

mod orchestrator;

#[cfg(test)]
mod interactive_term_tests;
#[cfg(test)]
mod task_tests;
#[cfg(test)]
mod tools_actor_tests;
pub use orchestrator::{ToolOrchestratorActor, ToolOrchestratorActorDeps};

use jinn_domain::common::services::Services;
use jinn_domain::common::state::State;

/// Activates the tools slice over the kernel's services: registers the
/// `tools/registry` cell (idempotent — re-registering over an existing cell
/// is a no-op error we ignore deliberately, matching the term-slice
/// precedent). The cell publishes MCP tool definitions to the TUI; the
/// orchestrator actor itself is spawned by actor wiring (it needs the
/// announce supervisor and explicit spawn ordering vs. the MCP coordinator).
pub fn activate(services: &mut Services, _state: &State) {
    let _ = services.slices.register(
        jinn_tools_msg::tools_registry_slot(),
        jinn_tools_msg::ToolRegistry::default(),
    );
}
