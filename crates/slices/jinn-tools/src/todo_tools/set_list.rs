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

//! `set_list` built-in tool - replaces the entire task list in one call.

use crate::BoxedToolFuture;
use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

/// Returns the tool definition for `set_list`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_set_list".to_owned(),
        description: "Replace the entire task list with a new one. \
            Accepts an ordered list of phases, each containing an ordered list of tasks. \
            The list you send IS the list - anything omitted is deleted, and task \
            statuses are declared inline, not remembered. Each task is either a bare \
            string (created as pending) or an object with 'description' and an \
            optional 'status' (pending, completed, or cancelled). Pass an empty \
            phases array to clear the task list entirely. Use this when you have a \
            complete plan ready to materialize; use todo_set_phase for day-to-day \
            updates."
            .to_owned(),
        prompt_snippet: Some("Create a new task list".to_owned()),
        prompt_guidelines: vec![
            "Provide the full plan - all phases and tasks - in a single call. \
             Existing phases and tasks are replaced entirely."
                .to_owned(),
            "Each phase must have a description. Tasks within a phase are optional.".to_owned(),
            "Read the current list first (todo_get_list) and include everything \
             you want to keep - anything omitted is deleted."
                .to_owned(),
            "A bare string task is created as pending. To record progress, pass \
             the task as an object: {\"description\": \"...\", \"status\": \
             \"completed\"} (statuses: pending, completed, cancelled)."
                .to_owned(),
            "'postponed' is not a valid status - move the task to a later phase \
             or cancel it instead."
                .to_owned(),
            "Pass an empty phases array ({\"phases\": []}) to clear the task list entirely."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "phases": {
                    "type": "array",
                    "description": "Ordered list of phases. Each phase has a description and an optional list of tasks. An empty array clears the task list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {
                                "type": "string",
                                "description": "Name of the phase (e.g., 'Research', 'Build', 'Test')"
                            },
                            "tasks": {
                                "type": "array",
                                "description": "Ordered list of tasks for this phase. Each task is a string (created as pending) or an object {description, status} with status one of: pending, completed, cancelled.",
                                "items": {
                                    "oneOf": [
                                        { "type": "string" },
                                        {
                                            "type": "object",
                                            "properties": {
                                                "description": { "type": "string" },
                                                "status": {
                                                    "type": "string",
                                                    "enum": ["pending", "completed", "cancelled"],
                                                    "description": "Declared status of this task. Omit for pending."
                                                }
                                            },
                                            "required": ["description"],
                                            "additionalProperties": false
                                        }
                                    ]
                                }
                            }
                        },
                        "required": ["description"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["phases"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `set_list` tool.
///
/// # Panics
///
/// Does not panic under normal operation. Panics indicate a bug.
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

        let Some(phases_val) = args.get("phases") else {
            return tool_error(call, "missing 'phases' argument");
        };

        let Some(phases_arr) = phases_val.as_array() else {
            return tool_error(call, "'phases' must be an array");
        };

        // Parse into declarative phase inputs (bare strings → Pending; objects
        // carry an explicit status). Parsing is pure: any payload error aborts
        // here, before task list state is touched. An empty array is valid —
        // it means "clear the task list entirely".
        let phase_inputs = {
            let parsed = super::task_payload::parse_phases_array(phases_arr);
            match parsed {
                Ok(inputs) => inputs,
                Err(msg) => return tool_error(call, &msg),
            }
        };

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
            let list = session.task_list_mut();
            if phase_inputs.is_empty() {
                list.clear();
                return Ok("Task list cleared.".to_owned());
            }
            list.set_from_inputs(&phase_inputs);
            let next_block = list.render_next_block();
            let rendered = list.render_text_with_blockers();
            Ok(format!("{next_block}\nTask list replaced.\n\n{rendered}"))
        });

        match result {
            Ok(content) => {
                if let Some(bus) = &ctx.bus {
                    bus.publish(jinn_session_history_msg::TaskListUpdated {
                        session_id: session_id.clone(),
                    })
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
            session_cap: Some(jinn_domain::common::tcaps::mint::mint_session_cap()),
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        }
    }

    fn setup_with_existing_list() -> (State, SessionId) {
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
                description: "Old Phase".to_owned(),
                tasks: vec![("Old task".to_owned(), TaskStatus::Pending)],
            }]);
        };
        (state, session_id)
    }

    #[rstest::rstest]
    #[test]
    fn set_list_replaces_entire_list() {
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [
                    { "description": "Research", "tasks": ["Read docs", "Call API"] },
                    { "description": "Build", "tasks": ["Write code"] },
                    { "description": "Deploy" }
                ]
            })
            .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(result.content.contains("Task list replaced"));
        assert!(result.content.contains("Research"));
        assert!(result.content.contains("Build"));
        assert!(result.content.contains("Deploy"));
        assert!(result.content.contains("Read docs"));
        assert!(result.content.contains("Write code"));
        assert!(!result.content.contains("Old Phase"));
        assert!(!result.content.contains("Old task"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_with_empty_tasks() {
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [
                    { "description": "Planning" }
                ]
            })
            .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(result.content.contains("(no tasks)"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_with_empty_phases_clears_list() {
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({ "phases": [] }).to_string(),
        };
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Task list cleared"),
            "expected clear message, got: {:?}",
            result.content
        );

        // And the session's task list is empty.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert!(session.task_list().is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn set_list_clear_on_already_empty_succeeds() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({ "phases": [] }).to_string(),
        };
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);

        // And the list is still empty.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert!(session.task_list().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_list_clear_publishes_task_list_updated() {
        // Given a session with an existing list and a recorder on the bus.
        let harness = jinn_domain::common::bus::test_harness::TestHarness::new().await;
        let (state, session_id) = setup_with_existing_list();
        let recorder = harness
            .spawn_recorder::<jinn_session_history_msg::TaskListUpdated>()
            .await;
        let mut ctx = make_context(Some(state), Some(session_id.clone()));
        ctx.bus = Some(harness.bus());

        // When clearing the list via the tool.
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({ "phases": [] }).to_string(),
        };
        let result = execute(call, ctx).await;
        assert!(result.success, "expected success: {:?}", result.content);

        // Then exactly one TaskListUpdated event is published for the session.
        let events = jinn_domain::common::bus::test_harness::await_recorded(
            &recorder,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, session_id);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_errors_on_missing_phase_description() {
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [
                    { "tasks": ["Do stuff"] }
                ]
            })
            .to_string(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(
            result.content.contains("missing 'description'"),
            "expected missing description error, got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn set_list_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Test" }]
            })
            .to_string(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_return_has_next_block_at_top() {
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [
                    { "description": "Research", "tasks": ["Read docs"] },
                    { "description": "Build", "tasks": ["Write code"] }
                ]
            })
            .to_string(),
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
    fn set_list_bare_strings_default_to_pending() {
        // Given a payload authoring tasks as bare strings.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Build", "tasks": ["Write code"] }]
            })
            .to_string(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the persisted task is Pending.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let task = &session.task_list().phases()[0].tasks()[0];
        assert_eq!(task.description, "Write code");
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_declared_statuses_stick() {
        // Given a payload declaring completed and cancelled statuses.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Build", "tasks": [
                    { "description": "Done work", "status": "completed" },
                    { "description": "Dropped work", "status": "cancelled" },
                    { "description": "Todo work" }
                ]}]
            })
            .to_string(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then each persisted task carries its declared status.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let tasks = &session.task_list().phases()[0].tasks;
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].status, TaskStatus::Cancelled);
        // And the omitted status defaulted to Pending.
        assert_eq!(tasks[2].status, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[case("postponed")]
    #[case("deferred")]
    #[test]
    fn set_list_rejects_postponed_status(#[case] status: &str) {
        // Given a payload declaring the non-declarable status.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Build", "tasks": [
                    { "description": "Later work", "status": status }
                ]}]
            })
            .to_string(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call fails with guidance.
        assert!(!result.success);
        assert!(
            result.content.contains("not a declarable status"),
            "expected guidance, got: {:?}",
            result.content
        );
        // And the existing list is untouched (no partial write).
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert_eq!(session.task_list().phases()[0].description(), "Old Phase");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_replace_builds_before_swapping() {
        // Given a session whose list has a known first phase.
        let (state, session_id) = setup_with_existing_list();

        // When replacing the list.
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [
                    { "description": "A", "tasks": ["a1", "a2"] },
                    { "description": "B", "tasks": ["b1"] }
                ]
            })
            .to_string(),
        };
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the full replacement is present with no residue from the old list.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let list = session.task_list();
        assert_eq!(list.phases().len(), 2);
        assert_eq!(list.phases()[0].tasks().len(), 2);
        assert_eq!(list.phases()[1].tasks().len(), 1);
        assert!(!result.content.contains("Old Phase"));
    }
}
