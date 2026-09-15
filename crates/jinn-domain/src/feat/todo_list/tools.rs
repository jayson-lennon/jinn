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

//! Todo list tool registry.
//!
//! Wires each todo list tool module into a list of (definition, execute) pairs
//! for registration by the tool orchestrator.

pub mod add_phase;
pub mod add_task;
pub mod cancel_task;
pub mod complete_task;
pub mod get_phase;
pub mod get_task_list;
pub mod postpone_task;
pub mod postpone_to_phase;
pub mod set_list;
pub mod set_phase;
pub mod task_payload;

use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::registry::BuiltinToolEntry;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition};

/// Returns all todo list tool entries (definition + execute function).
///
/// Used by the tool orchestrator to register todo list tools at activation.
pub fn tool_entries() -> Vec<BuiltinToolEntry> {
    vec![
        (
            add_phase::definition(),
            add_phase::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            add_task::definition(),
            add_task::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            cancel_task::definition(),
            cancel_task::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            complete_task::definition(),
            complete_task::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            postpone_task::definition(),
            postpone_task::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            postpone_to_phase::definition(),
            postpone_to_phase::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            get_task_list::definition(),
            get_task_list::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            get_phase::definition(),
            get_phase::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            set_list::definition(),
            set_list::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
        (
            set_phase::definition(),
            set_phase::execute as fn(ToolCall, ToolContext) -> BoxedToolFuture,
            false,
        ),
    ]
}

/// Returns all todo list tool definitions (for prompt injection).
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        add_phase::definition(),
        add_task::definition(),
        cancel_task::definition(),
        complete_task::definition(),
        postpone_task::definition(),
        postpone_to_phase::definition(),
        get_task_list::definition(),
        get_phase::definition(),
        set_list::definition(),
        set_phase::definition(),
    ]
}
