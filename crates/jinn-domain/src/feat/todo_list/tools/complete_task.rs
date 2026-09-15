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

//! `complete_task` built-in tool - marks a task as completed.

use crate::feat::todo_list::TaskId;
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `complete_task`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_complete_task".to_owned(),
        description: "Mark a task in the todo list as completed.".to_owned(),
        prompt_snippet: Some("Mark a task as done".to_owned()),
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to mark as completed (e.g., 't3')"
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `complete_task` tool.
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
            match list.complete_task(&task_id) {
                Ok(()) => {
                    let phase_id = list.phase_id_for_task(&task_id);
                    let next_block = match &phase_id {
                        Some(pid) => list.render_next_block_after_completion(pid),
                        None => list.render_next_block(),
                    };
                    let rendered = list.render_text_with_blockers();
                    Ok(format!(
                        "{next_block}\nTask [{task_id}] marked as completed.\n\n{rendered}"
                    ))
                }
                Err(e) => {
                    let rendered = list.render_text_with_blockers();
                    Err(format!("Error: {e}\n\n{rendered}"))
                }
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
    fn complete_task_marks_as_completed() {
        let (state, session_id, tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("[✓]"),
            "should show completed indicator"
        );
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_errors_on_unknown_task() {
        let (state, session_id, _tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
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
    fn complete_task_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: r#"{"task_id": "t1"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_next_block_appears_at_top() {
        let (state, session_id, tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // NEXT block (→) appears before the confirmation line.
        let next_idx = result.content.find("→").unwrap_or(usize::MAX);
        let confirm_idx = result
            .content
            .find("marked as completed")
            .unwrap_or(usize::MAX);
        assert!(
            next_idx < confirm_idx,
            "NEXT block must precede confirmation: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_next_block_when_more_tasks_in_phase() {
        // Two tasks in same phase; complete the first.
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let (tid, second_id) = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let pid = session.task_list_mut().add_phase("Build");
            let first = session
                .task_list_mut()
                .add_task(&pid, "First", TaskPosition::End)
                .unwrap()
                .to_string();
            let second = session
                .task_list_mut()
                .add_task(&pid, "Second", TaskPosition::End)
                .unwrap()
                .to_string();
            (first, second)
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // NEXT block names the second task.
        assert!(
            result
                .content
                .contains(&format!("→ NEXT: {} — Second", second_id)),
            "expected NEXT naming second task, got: {:?}",
            result.content
        );
        assert!(result.content.contains("1 pending in phase"));
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_next_block_when_phase_done_with_later_blocked() {
        // Complete last task in P1 while P2 still has pending work.
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let tid = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let p1 = session.task_list_mut().add_phase("Build");
            let only = session
                .task_list_mut()
                .add_task(&p1, "Only", TaskPosition::End)
                .unwrap()
                .to_string();
            let p2 = session.task_list_mut().add_phase("Test");
            session
                .task_list_mut()
                .add_task(&p2, "First", TaskPosition::End)
                .unwrap();
            // P3 stays blocked — P2 has work, so P2 becomes active after P1 completes.
            let p3 = session.task_list_mut().add_phase("Ship");
            session
                .task_list_mut()
                .add_task(&p3, "Later", TaskPosition::End)
                .unwrap();
            only
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        assert!(
            result.content.contains("complete — proceed to verify"),
            "expected phase-complete NEXT, got: {:?}",
            result.content
        );
        // After completing P1, P2 becomes active (has pending work). P3 still blocked.
        assert!(result.content.contains("(Blocked by previous phase)"));
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_next_block_when_all_phases_complete() {
        // Complete the last pending task across all phases.
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
                .add_task(&pid, "Only", TaskPosition::End)
                .unwrap()
                .to_string()
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: serde_json::json!({"task_id": tid}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // When the last task in the last phase completes, the model still
        // needs to verify — so the message is still 'phase complete — proceed to verify'.
        // The 'all phases complete — stop' message comes from non-completion tools
        // (used after verification passes).
        assert!(
            result.content.contains("complete — proceed to verify"),
            "expected phase-complete NEXT (verify still pending), got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn complete_task_error_includes_rendered_list() {
        // Error path should also return the prefix-aware list for self-correction.
        let (state, session_id, _tid) = setup_with_task();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_complete_task".to_owned(),
            arguments: r#"{"task_id": "t99"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("task not found"));
        // The rendered list (phase header) follows the error line.
        assert!(result.content.contains("Build"));
    }
}
