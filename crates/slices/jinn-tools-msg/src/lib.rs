//! Tools crossing vocabulary: the orchestrator's registration and
//! execution contracts, the shared tool registry cell, the task
//! subagent registry, the command policy, and the todo task-list
//! model.
//!
//! Pure data + pure functions — the orchestrator actor and the built-in
//! tools live in the tools slice crate; the kernel (session actor,
//! provider protocol, TUI) and sibling slices speak only the types in
//! this crate.

pub mod command;
pub mod event;
pub mod notices;
pub mod task_list_entry;
pub mod task_list_picker_state;
pub mod task_registry;
pub mod todo_list;
pub mod tool_entry;
pub mod tool_future;
pub mod tool_picker_scope;
pub mod tool_picker_state;
pub mod tool_registry;
pub mod truncation;

pub use command::*;
pub use event::*;
pub use notices::*;
pub use task_list_entry::{RowStatus, TaskListTreeEntry, render_task_list_row};
pub use task_list_picker_state::{
    RESULTS_VIEWPORT_FALLBACK as TASK_LIST_PICKER_RESULTS_VIEWPORT_FALLBACK, TaskListPickerState,
    task_list_picker_scope, task_list_picker_slot,
};
pub use task_registry::*;
pub use todo_list::*;
pub use tool_entry::ToolEntry;
pub use tool_future::*;
pub use tool_picker_scope::tool_picker_scope;
pub use tool_picker_state::{
    RESULTS_VIEWPORT_FALLBACK as TOOL_PICKER_RESULTS_VIEWPORT_FALLBACK, ToolPickerState,
    tool_picker_slot,
};
pub use tool_registry::*;
pub use truncation::*;

#[cfg(test)]
mod tests;
