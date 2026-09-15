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

//! `postpone_task` built-in tool — postpones a task to a new location.

use crate::feat::todo_list::{TaskId, TaskPosition};
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `postpone_task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_postpone_task".to_owned(),
        description: "Postpone a task by marking it as postponed (\u{25bc}) and creating a \
            pending copy at a new location. The source task remains in place but \
            is excluded from agent-facing task list output. The new copy is placed \
            relative to a reference task, which determines the target phase."
            .to_owned(),
        prompt_snippet: Some("Postpone a task to a different location".to_owned()),
        prompt_guidelines: vec![
            "Exactly one of after_task or before_task is required to position the postponed copy."
                .to_owned(),
            "The source task is marked as postponed (\u{25bc}) and will not appear in agent-facing \
             task list queries."
                .to_owned(),
            "Use todo_add_phase first if you need to postpone into a new phase.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to postpone"
                },
                "after_task": {
                    "type": "string",
                    "description": "Create the new copy after this task. Mutually exclusive with before_task."
                },
                "before_task": {
                    "type": "string",
                    "description": "Create the new copy before this task. Mutually exclusive with after_task."
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `postpone_task` tool.
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

        let task_id_str = match args.get("task_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return tool_error(call, "missing 'task_id' argument"),
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
        if after_task.is_none() && before_task.is_none() {
            return tool_error(
                call,
                "must specify either after_task or before_task to position the postponed copy",
            );
        }

        let position = match (&after_task, &before_task) {
            (Some(id), None) => TaskPosition::After(TaskId::from_string(id.clone())),
            (None, Some(id)) => TaskPosition::Before(TaskId::from_string(id.clone())),
            _ => unreachable!(),
        };

        let source_id = TaskId::from_string(task_id_str);

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
                let list = session.task_list_mut();
                match list.postpone_task(&source_id, position) {
                    Ok(new_task_id) => {
                        let next_block = list.render_next_block();
                        let rendered = list.render_text_with_blockers();
                        Ok(format!(
                            "{next_block}\nPostponed task [{source_id}] \u{2192} created copy [{new_task_id}].\n\n{rendered}"
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

    fn setup_with_two_phases() -> (State, SessionId, String, String, String, String) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let (p1, t1, p2, t2) = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let p1 = session.task_list_mut().add_phase("Research");
            let t1 = session
                .task_list_mut()
                .add_task(&p1, "Read docs", TaskPosition::End)
                .unwrap();
            let p2 = session.task_list_mut().add_phase("Build");
            let t2 = session
                .task_list_mut()
                .add_task(&p2, "Write code", TaskPosition::End)
                .unwrap();
            (
                p1.to_string(),
                t1.to_string(),
                p2.to_string(),
                t2.to_string(),
            )
        };
        (state, session_id, p1, t1, p2, t2)
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_creates_copy_after_reference() {
        let (state, session_id, _p1, t1, _p2, t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "after_task": t2}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Postponed task"),
            "should mention postponed task"
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_creates_copy_before_reference() {
        let (state, session_id, _p1, t1, _p2, t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "before_task": t2}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_requires_after_or_before() {
        let (state, session_id, _p1, t1, _p2, _t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(
            result
                .content
                .contains("must specify either after_task or before_task"),
            "expected positioning requirement error, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_rejects_both_after_and_before() {
        let (state, session_id, _p1, t1, _p2, t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "after_task": t2, "before_task": t2})
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
    fn postpone_task_errors_on_unknown_task() {
        let (state, session_id, _p1, _t1, _p2, t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": "t99", "after_task": t2}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("task not found"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_errors_on_unknown_reference() {
        let (state, session_id, _p1, t1, _p2, _t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "after_task": "t99"}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("task not found"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_return_has_next_block_at_top() {
        let (state, session_id, _p1, t1, _p2, t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "after_task": t2}).to_string(),
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
        assert!(result.content.contains("Postponed task"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_errors_on_self_reference() {
        let (state, session_id, _p1, t1, _p2, _t2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "after_task": t1}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(
            result.content.contains("itself"),
            "expected self-reference error, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_task_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_task".to_owned(),
            arguments: r#"{"task_id": "t1", "after_task": "t2"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }
}
