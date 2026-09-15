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

//! `cancel_task` built-in tool — marks a task as cancelled (not happening).

use crate::feat::todo_list::TaskId;
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `cancel_task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_cancel_task".to_owned(),
        description: "Cancel a task — mark it as not happening. Cancelled tasks remain visible \
            to the agent with a CANCELLED: prefix to prevent re-implementation of abandoned work."
            .to_owned(),
        prompt_snippet: Some("Cancel a task (not doing it)".to_owned()),
        prompt_guidelines: vec![
            "Cancelled tasks are hidden from the sidebar but remain visible in the task list \
             with a CANCELLED: prefix."
                .to_owned(),
            "Use cancel when a task is not happening at all — not just postponed.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to cancel (e.g., 't3')"
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `cancel_task` tool.
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

        let task_id_str = match args.get("task_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return tool_error(call, "missing 'task_id' argument"),
        };

        let task_id = TaskId::from_string(task_id_str);

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
            let list = session.task_list_mut();
            match list.cancel_task(&task_id) {
                Ok(()) => {
                    let next_block = list.render_next_block();
                    let rendered = list.render_text_with_blockers();
                    Ok(format!(
                        "{next_block}\nTask [{task_id}] cancelled.\n\n{rendered}"
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

    fn setup_with_task() -> (State, SessionId, String) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let tid = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let pid = session.task_list_mut().add_phase("Build");
            session
                .task_list_mut()
                .add_task(&pid, "Write code", TaskPosition::End)
                .unwrap()
                .to_string()
        };
        (state, session_id, tid)
    }

    #[rstest::rstest]
    #[test]
    fn cancel_task_marks_as_cancelled() {
        let (state, session_id, tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("cancelled"),
            "should mention cancelled"
        );
    }

    #[rstest::rstest]
    #[test]
    fn cancel_task_errors_on_unknown_task() {
        let (state, session_id, _tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: r#"{"task_id": "t99"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("task not found"));
    }

    #[rstest::rstest]
    #[test]
    fn cancel_task_errors_on_already_cancelled() {
        let (state, session_id, tid) = setup_with_task();
        let call1 = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx1 = make_context(Some(state.clone()), Some(session_id.clone()));
        let result1 = execute(call1, ctx1);
        let result1 = futures::executor::block_on(result1);
        assert!(result1.success, "first cancel should succeed");

        let call2 = ToolCall {
            id: "call-2".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx2 = make_context(Some(state), Some(session_id));
        let result2 = execute(call2, ctx2);
        let result2 = futures::executor::block_on(result2);
        assert!(!result2.success);
        assert!(
            result2.content.contains("already cancelled"),
            "expected already cancelled error, got: {:?}",
            result2.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn cancel_task_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: r#"{"task_id": "t1"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
    }

    #[rstest::rstest]
    #[test]
    fn cancel_task_return_has_next_block_at_top() {
        // Use two tasks so cancelling one still leaves a next task to point at.
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let tid = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let pid = session.task_list_mut().add_phase("Build");
            session
                .task_list_mut()
                .add_task(&pid, "First", TaskPosition::End)
                .unwrap();
            session
                .task_list_mut()
                .add_task(&pid, "Second", TaskPosition::End)
                .unwrap()
                .to_string()
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_cancel_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
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
        assert!(result.content.contains("cancelled"));
    }
}
