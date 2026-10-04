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

pub mod attendant_tools;
pub mod bash;

#[cfg(test)]
mod attendant_tools_tests;

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
pub mod task_list_picker_actions;
pub mod task_list_picker_render;
pub mod task_list_picker_routes;
pub mod task_list_picker_viewport;
pub mod task_phase_listener_actor;
pub mod task_settle_listener_actor;
pub mod todo_tools;
pub mod tool_paths;
pub mod tool_picker_actions;
pub mod tool_picker_render;
pub mod tool_picker_routes;
mod tool_picker_viewport;
pub mod tool_types;
pub(crate) mod visible_lines;
pub mod write;

mod orchestrator;

#[cfg(test)]
mod interactive_term_tests;
#[cfg(test)]
#[path = "orchestrator_filter_tests.rs"]
mod orchestrator_filter_tests;
#[cfg(test)]
mod task_list_picker_tests;
#[cfg(test)]
mod task_tests;
#[cfg(test)]
mod tool_picker_tests;
#[cfg(test)]
mod tools_actor_tests;
pub use orchestrator::{ToolOrchestratorActor, ToolOrchestratorActorDeps};

pub use jinn_tools_msg::tool_picker_scope;
pub use tool_picker_routes::open_from_scope as open_tool_picker_from_scope;

use jinn_kernel::common::services::Services;
use jinn_kernel::common::state::State;

/// Installs the tools slice's overlay wiring over the kernel's services.
///
/// The `tools/registry` cell is registered by the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`), not here. The cell publishes
/// MCP tool definitions to the TUI; the orchestrator actor itself is
/// spawned by composition (it needs the announce supervisor and explicit
/// spawn ordering vs. the MCP coordinator).
pub fn activate(services: &mut Services, _state: &State) {
    // Nothing to install: every tools cell comes from the catalog, and
    // every overlay, route row, and actor spawn is wired by `activate_picker`
    // and by composition.
    let _ = services;
}

/// Registers the tool picker: its cell, its overlay, its keys, and its filter
/// hook.
///
/// Split from [`activate`] because the picker needs a [`SliceHost`] (overlay
/// geometry, renderer, key routes) rather than the raw services the registry
/// cell needs. Called from composition right after `activate`.
///
/// The slot and scope are namespaced `tools`/`tool-picker`, not `tools`/
/// `picker`: the task-list picker will live in this same slice, and two
/// pickers must never share an identity.
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a wiring
/// bug.
/// The task-list picker's opener, as a dispatchable action.
///
/// The sidebar's `s` key needs to open a picker that belongs to this slice.
/// It could publish a `DynamicIntent` naming this slice's action, but a
/// published message goes to the bus and never comes back through route
/// dispatch — the action would never run and the menu would never appear.
///
/// Handing the sidebar this closure keeps the dependency honest in both
/// directions: the sidebar names the task list *browser*, not the picker's
/// scope, cell, or state.
#[must_use]
pub fn task_list_picker_opener() -> jinn_slices::route::ActionFn {
    task_list_picker_routes::task_list_opener_action()
}

///
/// # Panics
///
/// Panics if the cell catalog has not run - the picker would render against
/// an absent cell and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate_picker(host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .slices()
        .reader::<jinn_tools_msg::ToolPickerState>(&jinn_tools_msg::tool_picker_slot())
        .expect("the cell catalog registers the tool picker slot before any slice activates");

    let scope = tool_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(tool_picker_render::tool_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_tools_msg::tool_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(tool_picker_render::render_tool_picker),
    );

    // The picker's keys, and the filter's input hook, are this slice's own.
    tool_picker_routes::attach_tool_picker_rows(host.key_routes(), &cell);
    tool_picker_routes::register_tool_picker_input_hook(host.key_routes(), &cell);

    activate_task_list_picker(host);
}

/// Registers the task-list browser: its cell, its overlay, its keys, and its
/// filter hook.
///
/// A tree menu, and the second picker this slice hosts. Neither picker knows
/// the other exists — separate scopes, separate cells, separate key sets.
///
/// # Panics
///
/// Panics if the cell catalog has not run - the picker would render against
/// an absent cell and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate_task_list_picker(host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .slices()
        .reader::<jinn_tools_msg::TaskListPickerState>(&jinn_tools_msg::task_list_picker_slot())
        .expect("the cell catalog registers the task-list picker slot before any slice activates");

    let scope = jinn_tools_msg::task_list_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(task_list_picker_render::task_list_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_tools_msg::task_list_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(task_list_picker_render::render_task_list_picker),
    );

    task_list_picker_routes::attach_task_list_picker_rows(host.key_routes(), &cell);
    task_list_picker_routes::register_task_list_picker_input_hook(host.key_routes(), &cell);
}
