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
pub mod command_policy;
pub mod event;
pub mod notices;
pub mod task_registry;
pub mod todo_list;
pub mod tool_future;
pub mod tool_registry;
pub mod truncation;

pub use command::*;
pub use command_policy::*;
pub use event::*;
pub use notices::*;
pub use task_registry::*;
pub use todo_list::*;
pub use tool_future::*;
pub use tool_registry::*;

#[cfg(test)]
mod tests;
