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

//! `postpone_to_phase` built-in tool — postpones a task to the end of a phase.

use crate::feat::todo_list::{PhaseId, TaskId};
use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `postpone_to_phase`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_postpone_to_phase".to_owned(),
        description: "Postpone a task by marking it as postponed (\u{25bc}) and creating a \
            pending copy at the end of a target phase. Using target_phase_id moves \
            the task to an existing phase. Using phase_description creates a new \
            phase and moves the task there. These two options are mutually exclusive. \
            The source task remains in place but is excluded from agent-facing task \
            list output."
            .to_owned(),
        prompt_snippet: Some("Postpone a task to a phase".to_owned()),
        prompt_guidelines: vec![
            "Use target_phase_id to postpone to an existing phase. Use phase_description to create \
             a new phase and postpone into it. These are mutually exclusive."
                .to_owned(),
            "The copy is always appended to the end of the target phase. \
             Use todo_postpone_task if you need to position the copy relative to a specific task."
                .to_owned(),
            "The source task is marked as postponed (\u{25bc}) and will not appear in \
             agent-facing task list queries."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to postpone"
                },
                "target_phase_id": {
                    "type": "string",
                    "description": "ID of an existing phase to postpone the task to. Mutually exclusive with phase_description."
                },
                "phase_description": {
                    "type": "string",
                    "description": "Description for a new phase to create and postpone the task into. Mutually exclusive with target_phase_id."
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `postpone_to_phase` tool.
///
/// # Panics
///
/// Does not panic under normal operation. Panics indicate a bug.
#[expect(clippy::expect_used, reason = "infallible")]
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
        let target_phase_id: Option<String> = args
            .get("target_phase_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let phase_description: Option<String> = args
            .get("phase_description")
            .and_then(|v| v.as_str())
            .map(String::from);

        if target_phase_id.is_some() && phase_description.is_some() {
            return tool_error(
                call,
                "cannot specify both target_phase_id and phase_description",
            );
        }
        if target_phase_id.is_none() && phase_description.is_none() {
            return tool_error(
                call,
                "must specify either target_phase_id or phase_description",
            );
        }

        let source_id = TaskId::from_string(task_id_str);

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
                let list = session.task_list_mut();

                // Determine target phase ID.
                let target_pid = if let Some(desc) = &phase_description {
                    list.add_phase(desc)
                } else {
                    PhaseId::from_string(
                        target_phase_id
                            .clone()
                            .expect("validated above that one is present"),
                    )
                };

                match list.postpone_to_phase(&source_id, &target_pid) {
                    Ok(new_task_id) => {
                        let next_block = list.render_next_block();
                        let rendered = list.render_text_with_blockers();
                        Ok(format!(
                            "{next_block}\nPostponed task [{source_id}] \u{2192} created copy [{new_task_id}] in phase [{target_pid}].\n\n{rendered}"
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

    fn setup_with_two_phases() -> (State, SessionId, String, String, String) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let (p1, t1, p2) = {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let p1 = session.task_list_mut().add_phase("Research");
            let t1 = session
                .task_list_mut()
                .add_task(&p1, "Read docs", TaskPosition::End)
                .unwrap();
            let p2 = session.task_list_mut().add_phase("Build");
            (p1.to_string(), t1.to_string(), p2.to_string())
        };
        (state, session_id, p1, t1, p2)
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_existing_phase_appends_copy() {
        let (state, session_id, _p1, t1, p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "target_phase_id": p2}).to_string(),
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
    fn postpone_to_new_phase_creates_and_appends() {
        let (state, session_id, _p1, t1, _p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "phase_description": "Testing"})
                .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Testing"),
            "should mention new phase name"
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_rejects_both_options() {
        let (state, session_id, _p1, t1, p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({
                "task_id": t1,
                "target_phase_id": p2,
                "phase_description": "Testing"
            })
            .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(
            result
                .content
                .contains("both target_phase_id and phase_description"),
            "expected mutual exclusion error, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_requires_one_option() {
        let (state, session_id, _p1, t1, _p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(
            result
                .content
                .contains("must specify either target_phase_id or phase_description"),
            "expected option requirement error, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_errors_on_unknown_phase() {
        let (state, session_id, _p1, t1, _p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "target_phase_id": "p99"}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("phase not found"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_errors_on_unknown_task() {
        let (state, session_id, _p1, _t1, p2) = setup_with_two_phases();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": "t99", "target_phase_id": p2}).to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("task not found"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_errors_on_already_postponed() {
        let (state, session_id, _p1, t1, p2) = setup_with_two_phases();
        // Postpone once.
        let call1 = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "target_phase_id": p2}).to_string(),
        };
        let ctx1 = make_context(Some(state.clone()), Some(session_id.clone()));
        let result1 = execute(call1, ctx1);
        let result1 = futures::executor::block_on(result1);
        assert!(result1.success, "first postpone should succeed");

        // Try to postpone again.
        let call2 = ToolCall {
            id: "call-2".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: serde_json::json!({"task_id": t1, "target_phase_id": p2}).to_string(),
        };
        let ctx2 = make_context(Some(state), Some(session_id));
        let result2 = execute(call2, ctx2);
        let result2 = futures::executor::block_on(result2);
        assert!(!result2.success);
        assert!(result2.content.contains("already postponed"));
    }

    #[rstest::rstest]
    #[test]
    fn postpone_to_phase_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_postpone_to_phase".to_owned(),
            arguments: r#"{"task_id": "t1", "target_phase_id": "p1"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }
}
