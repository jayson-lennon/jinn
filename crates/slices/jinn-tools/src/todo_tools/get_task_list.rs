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

//! `get_list` built-in tool - returns the full task list.

use crate::BoxedToolFuture;
use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

/// Returns the tool definition for `get_list`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_get_list".to_owned(),
        description: "Get the full todo list with all phases and tasks. \
            Returns the current state of the todo list, plus the next task to work on."
            .to_owned(),
        prompt_snippet: Some("Review the current task list".to_owned()),
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `get_list` tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(state) = ctx.state else {
            return tool_error(call, "no application state available");
        };
        let Some(session_id) = ctx.session_id else {
            return tool_error(call, "no session ID available");
        };

        let rendered = {
            let r = state.read();
            let session = r.session(&session_id);
            let next_block = session.task_list().render_next_block();
            let body = session.task_list().render_text_with_blockers();
            if next_block.is_empty() {
                body
            } else {
                format!("{next_block}\n\n{body}")
            }
        };

        ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: rendered,
            success: true,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    })
}

fn tool_error(call: ToolCall, msg: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call.id,
        name: call.name,
        content: format!("Error: {msg}"),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use crate::tool_types::ToolContext;
    use jinn_core_types::tool_types::ToolCall;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::state::State;
    use jinn_domain::protocol::SessionId;
    use jinn_tools_msg::{PhaseInput, TaskStatus};

    use super::*;

    fn make_context(state: Option<State>, session_id: Option<SessionId>) -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("."),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state,
            session_id,
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        }
    }

    #[rstest::rstest]
    #[test]
    fn get_task_list_returns_placeholder_when_empty() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_list".to_owned(),
            arguments: "{}".to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        assert_eq!(result.content, "No phases defined.");
    }

    #[rstest::rstest]
    #[test]
    fn get_task_list_returns_full_list() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            session.task_list_mut().set_from_inputs(&[PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            }]);
        }

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_list".to_owned(),
            arguments: "{}".to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        assert!(result.content.contains("Phase 1: Build"));
        assert!(result.content.contains("Write code"));
    }

    #[rstest::rstest]
    #[test]
    fn get_task_list_return_has_next_block_at_top() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            session.task_list_mut().set_from_inputs(&[PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            }]);
        }

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_list".to_owned(),
            arguments: "{}".to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        assert!(
            result.content.starts_with("\u{2192}"),
            "expected NEXT block at top, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn get_task_list_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_list".to_owned(),
            arguments: "{}".to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
    }
}
