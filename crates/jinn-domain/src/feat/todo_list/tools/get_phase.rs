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

//! `get_phase` built-in tool - returns a single phase's tasks.

use crate::feat::todo_list::PhaseId;
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `get_phase`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_get_phase".to_owned(),
        description: "Get a single phase with its tasks from the todo list. \
            Returns the phase description and all tasks in that phase."
            .to_owned(),
        prompt_snippet: Some("Get details for a specific phase".to_owned()),
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "phase_id": {
                    "type": "string",
                    "description": "ID of the phase to retrieve (e.g., 'p1')"
                }
            },
            "required": ["phase_id"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `get_phase` tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(state) = ctx.state else {
            return tool_error(call, "no application state available");
        };
        let Some(session_id) = ctx.session_id else {
            return tool_error(call, "no session ID available");
        };

        let args: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);

        let phase_id_str = match args.get("phase_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return tool_error(call, "missing 'phase_id' argument"),
        };

        let phase_id = PhaseId::from_string(phase_id_str.clone());

        let result = {
            let r = state.read();
            let session = r.session(&session_id);
            let list = session.task_list();
            let rendered_phase = list.render_phase_text(&phase_id);
            let next_block = list.render_next_block();
            match rendered_phase {
                Some(rendered) => Ok(format!("{next_block}\n{rendered}")),
                None => Err(format!("Error: phase not found: {phase_id_str}")),
            }
        };

        match result {
            Ok(content) => ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content,
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
            Err(content) => ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content,
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
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
    use crate::common::app_state::AppState;
    use crate::common::state::State;
    use crate::feat::todo_list::TaskPosition;
    use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext};
    use crate::protocol::SessionId;

    use super::*;

    fn make_context(state: Option<State>, session_id: Option<SessionId>) -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("."),
            command_policy:
                crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
            timeout: None,
            state,
            session_id,
            app_paths: crate::common::app_paths::AppPaths::default(),
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

    fn setup_with_phase() -> (State, SessionId, String) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let pid = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let pid = session.task_list_mut().add_phase("Research");
            session
                .task_list_mut()
                .add_task(&pid, "Read docs", TaskPosition::End)
                .unwrap();
            pid.to_string()
        };
        (state, session_id, pid)
    }

    #[rstest::rstest]
    #[test]
    fn get_phase_returns_phase_tasks() {
        let (state, session_id, pid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_phase".to_owned(),
            arguments: serde_json::json!({"phase_id": pid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(result.content.contains("Phase 1: Research"));
        assert!(result.content.contains("Read docs"));
    }

    #[rstest::rstest]
    #[test]
    fn get_phase_errors_on_missing_phase() {
        let (state, session_id, _pid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_phase".to_owned(),
            arguments: r#"{"phase_id": "p99"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("phase not found"));
    }

    #[rstest::rstest]
    #[test]
    fn get_phase_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_phase".to_owned(),
            arguments: r#"{"phase_id": "p1"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
    }

    #[rstest::rstest]
    #[test]
    fn get_phase_return_has_next_block_at_top() {
        let (state, session_id, pid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_get_phase".to_owned(),
            arguments: serde_json::json!({"phase_id": pid}).to_string(),
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
}
