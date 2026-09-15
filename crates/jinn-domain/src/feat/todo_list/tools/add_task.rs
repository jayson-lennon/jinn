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

//! `add_task` built-in tool - adds a new task to a phase.

use crate::feat::todo_list::{PhaseId, TaskId, TaskPosition};
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `add_task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_add_task".to_owned(),
        description: "Add a new task to a phase in the todo list. \
            Optionally position it relative to another task using after_task or before_task. \
            When neither is specified, the task is appended to the end of the phase. \
            Returns the new task ID and the updated todo list."
            .to_owned(),
        prompt_snippet: Some("Add a task to a phase in the task list".to_owned()),
        prompt_guidelines: vec![
            "Specify after_task to insert after a specific task, or before_task to insert before one."
                .to_owned(),
            "Do not specify both after_task and before_task - they are mutually exclusive."
                .to_owned(),
            "When neither is specified, the task is appended to the end of the phase.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "phase_id": {
                    "type": "string",
                    "description": "ID of the phase to add the task to (e.g., 'p1')"
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable description of the task"
                },
                "after_task": {
                    "type": "string",
                    "description": "Optional: insert after this task ID (e.g., 't3'). Mutually exclusive with before_task."
                },
                "before_task": {
                    "type": "string",
                    "description": "Optional: insert before this task ID (e.g., 't3'). Mutually exclusive with after_task."
                }
            },
            "required": ["phase_id", "description"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `add_task` tool.
#[expect(clippy::unreachable, reason = "infallible")]
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
        let description = match args.get("description").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return tool_error(call, "missing 'description' argument"),
        };
        let after_task: Option<String> = args
            .get("after_task")
            .and_then(|v| v.as_str())
            .map(String::from);
        let before_task: Option<String> = args
            .get("before_task")
            .and_then(|v| v.as_str())
            .map(String::from);

        if after_task.is_some() && before_task.is_some() {
            return tool_error(call, "cannot specify both after_task and before_task");
        }

        let position = match (&after_task, &before_task) {
            (Some(id), None) => TaskPosition::After(TaskId::from_string(id.clone())),
            (None, Some(id)) => TaskPosition::Before(TaskId::from_string(id.clone())),
            (None, None) => TaskPosition::End,
            _ => unreachable!(),
        };

        let phase_id = PhaseId::from_string(phase_id_str);

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
            let list = session.task_list_mut();
            match list.add_task(&phase_id, &description, position) {
                Ok(task_id) => {
                    let next_block = list.render_next_block();
                    let rendered = list.render_text_with_blockers();
                    Ok(format!(
                        "{next_block}\nCreated task [{task_id}].\n\n{rendered}"
                    ))
                }
                Err(e) => Err(format!("Error: {e}")),
            }
        });

        match result {
            Ok(content) => {
                if let Some(bus) = &ctx.bus {
                    bus.publish(
                        crate::feat::session::protocol::task_list_updated::TaskListUpdated {
                            session_id: session_id.clone(),
                        },
                    )
                    .await;
                }
                ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content,
                    success: true,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                }
            }
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
            session_cap: Some(crate::common::tcaps::mint::mint_session_cap()),
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        }
    }

    fn setup_with_phase() -> (State, SessionId, String, String) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let (pid, tid) = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let pid = session.task_list_mut().add_phase("Build");
            let tid = session
                .task_list_mut()
                .add_task(&pid, "Write code", TaskPosition::End)
                .unwrap();
            (pid.to_string(), tid.to_string())
        };
        (state, session_id, pid, tid)
    }

    #[rstest::rstest]
    #[test]
    fn add_task_appends_to_phase() {
        let (state, session_id, pid, _tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: serde_json::json!({"phase_id": pid, "description": "Write tests"})
                .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Created task"),
            "should contain new task"
        );
    }

    #[rstest::rstest]
    #[test]
    fn add_task_inserts_after_reference() {
        let (state, session_id, pid, tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments:
                serde_json::json!({"phase_id": pid, "description": "Write docs", "after_task": tid})
                    .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
    }

    #[rstest::rstest]
    #[test]
    fn add_task_inserts_before_reference() {
        let (state, session_id, pid, tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: serde_json::json!({"phase_id": pid, "description": "Write docs", "before_task": tid})
                .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
    }

    #[rstest::rstest]
    #[test]
    fn add_task_errors_on_missing_phase() {
        let (state, session_id, _pid, _tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: r#"{"phase_id": "p99", "description": "Task"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("phase not found"));
    }

    #[rstest::rstest]
    #[test]
    fn add_task_errors_on_both_after_and_before() {
        let (state, session_id, pid, tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: serde_json::json!({"phase_id": pid, "description": "Task", "after_task": tid, "before_task": tid})
                .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("both after_task and before_task"));
    }

    #[rstest::rstest]
    #[test]
    fn add_task_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: r#"{"phase_id": "p1", "description": "Task"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }

    #[rstest::rstest]
    #[test]
    fn add_task_return_has_next_block_at_top() {
        let (state, session_id, pid, _tid) = setup_with_phase();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_task".to_owned(),
            arguments: serde_json::json!({"phase_id": pid, "description": "Write tests"})
                .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // NEXT block appears at the very top.
        assert!(
            result.content.starts_with("\u{2192}"),
            "expected NEXT block at top, got: {:?}",
            result.content
        );
        // Confirmation line follows NEXT block.
        assert!(result.content.contains("Created task"));
    }
}
