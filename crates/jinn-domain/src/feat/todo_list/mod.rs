//! Todo list subsystem - phased task tracking for agent sessions.
//!
//! The data model (`TaskList`, `Phase`, `Task`, and their IDs/statuses) lives
//! in `jinn-tools-msg`; this module hosts the kernel-side presentation and
//! tool glue until the tools family migration completes: the task-list picker
//! entry and the todo tool definitions.

pub mod picker_entry;
pub mod tools;

#[cfg(test)]
mod types_tests;
