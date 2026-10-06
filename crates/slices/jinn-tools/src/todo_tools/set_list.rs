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
use crate::tool_types::tool_error;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

/// Returns the tool definition for `set_list`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_set_list".to_owned(),
        description: "Replace the entire task list with a new one. \
            Accepts an ordered list of phases, each containing an ordered list of tasks. \
            The list you send IS the list - anything omitted is deleted, and task \
            statuses are declared inline, not remembered. Each task is an object with \
            'description' and 'status' (pending, completed, or cancelled). Pass an empty \
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
            "Send every task as an object with both fields: \
             {\"description\": \"...\", \"status\": \"pending\"} \
             (statuses: pending, completed, cancelled)."
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
                                "description": "Ordered list of tasks for this phase. An empty or omitted list is valid.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "description": {
                                            "type": "string",
                                            "description": "What the task is."
                                        },
                                        "status": {
                                            "type": "string",
                                            "enum": ["pending", "completed", "cancelled"],
                                            "description": "Declared status of this task. 'pending' if the work has not been done yet."
                                        }
                                    },
                                    "required": ["description", "status"],
                                    "additionalProperties": false
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
            return tool_error(&call, "no application state available");
        };
        let Some(session_id) = ctx.session_id else {
            return tool_error(&call, "no session ID available");
        };

        let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
            Ok(args) => args,
            Err(_) => return tool_error(&call, "arguments are not valid JSON"),
        };
        let Some(object) = args.as_object() else {
            return tool_error(&call, "arguments must be a JSON object");
        };

        // The presence check comes first: an absent 'phases' key is a caller
        // mistake worth reporting, and must never be normalised into an
        // empty list that wipes the plan.
        let Some(phases_val) = object.get("phases") else {
            return tool_error(&call, "missing 'phases' argument");
        };
        let phases_arr = match super::task_payload::normalize_array(phases_val, "phases") {
            Ok(entries) => entries,
            Err(msg) => return tool_error(&call, &msg),
        };

        // Parse into declarative phase inputs (bare strings → Pending; objects
        // carry an explicit status). Parsing is pure: any payload error aborts
        // here, before task list state is touched. An empty array is valid —
        // it means "clear the task list entirely".
        let phase_inputs = {
            let parsed = super::task_payload::parse_phases_array(&phases_arr);
            match parsed {
                Ok(inputs) => inputs,
                Err(msg) => return tool_error(&call, &msg),
            }
        };

        let result = state.with_session(|view| {
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
                content: content,
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
        }
    })
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
    use jinn_core_types::SessionId;
    use jinn_core_types::tool_types::ToolCall;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_tools_msg::{PhaseInput, TaskStatus};

    use super::*;

    fn make_context(state: Option<State>, session_id: Option<SessionId>) -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("."),
            config: jinn_config::testutil::config_layer(""),
            timeout: None,
            state,
            session_id,
            app_paths: jinn_kernel::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
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
            let mut w = state.write();
            let session = w.session_mut(&session_id);
            session.task_list_mut().set_from_inputs(&[PhaseInput {
                description: "Old Phase".to_owned(),
                tasks: vec![("Old task".to_owned(), TaskStatus::Pending)],
            }]);
        };
        (state, session_id)
    }

    /// Builds a `todo_set_list` call carrying a raw `phases` value.
    fn set_list_call(phases: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({ "phases": phases }).to_string(),
        }
    }

    /// Reads back the session's task list.
    fn read_list(state: &State, session_id: &SessionId) -> jinn_tools_msg::TaskList {
        let snapshot = state.read();
        let session = snapshot.session.get(session_id).expect("session present");
        session.task_list().clone()
    }

    #[rstest::rstest]
    #[test]
    fn set_list_accepts_single_object_wrapped_in_item_envelope() {
        // Given the exact payload shape a model emitted: a single phase
        // object wrapped in an `item` envelope with an empty tasks field.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!({
            "item": { "tasks": "", "description": "Probe" }
        }));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the write succeeds with the single intended phase.
        assert!(result.success, "expected success: {:?}", result.content);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases().len(), 1);
        assert_eq!(list.phases()[0].description(), "Probe");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_accepts_items_envelope_around_array() {
        // Given a payload wrapping the array in an `items` envelope.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!({
            "items": [
                { "description": "Research" },
                { "description": "Build" }
            ]
        }));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then both phases land in the sent order.
        assert!(result.success, "expected success: {:?}", result.content);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases().len(), 2);
        assert_eq!(list.phases()[0].description(), "Research");
        assert_eq!(list.phases()[1].description(), "Build");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_accepts_json_encoded_array() {
        // Given a payload that JSON-encodes the whole array into a string.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!("[{\"description\": \"A\"}]"));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the encoded array is decoded and written.
        assert!(result.success, "expected success: {:?}", result.content);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases().len(), 1);
        assert_eq!(list.phases()[0].description(), "A");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_empty_string_clears_list() {
        // Given a session with an existing list and an empty-string payload.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!(""));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the list is cleared, matching the empty-array behaviour.
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(read_list(&state, &session_id).is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn set_list_empty_string_tasks_renders_no_tasks() {
        // Given a phase whose tasks field is an empty string.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!([
            { "description": "Planning", "tasks": "" }
        ]));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the phase is written task-less rather than erroring.
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(result.content.contains("(no tasks)"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_unrecoverable_phases_shape_reports_worked_example() {
        // Given a phases value with no recoverable array.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!(42));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the failure names the expectation and shows an example.
        assert!(!result.success);
        assert!(
            result.content.contains("must be an array of phase objects"),
            "got: {:?}",
            result.content
        );
        assert!(
            result.content.contains("\"description\""),
            "got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn set_list_missing_phases_key_fails_and_keeps_list() {
        // Given a payload with no 'phases' key at all.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({}).to_string(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then it reports the missing argument instead of clearing the list.
        assert!(!result.success);
        assert!(
            result.content.contains("missing 'phases'"),
            "got: {:?}",
            result.content
        );
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases()[0].description(), "Old Phase");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_unparseable_arguments_fail_and_keep_list() {
        // Given arguments that are not valid JSON.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then it fails rather than degrading into a list clear.
        assert!(!result.success);
        assert!(
            result.content.contains("not valid JSON"),
            "got: {:?}",
            result.content
        );
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases()[0].description(), "Old Phase");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_malformed_phase_leaves_list_intact() {
        // Given a payload whose first phases are valid but whose last is not.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!([
            { "description": "Research" },
            { "tasks": ["no description here"] }
        ]));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then nothing is written - the earlier phases are not committed.
        assert!(!result.success);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases().len(), 1);
        assert_eq!(list.phases()[0].description(), "Old Phase");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_object_task_without_status_defaults_to_pending() {
        // Given an object task that omits 'status'.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!([
            { "description": "Build", "tasks": [{ "description": "x" }] }
        ]));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the task is created as Pending despite the schema requiring it.
        assert!(result.success, "expected success: {:?}", result.content);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases()[0].tasks()[0].status, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_task_items_schema_advertises_no_union() {
        // Given the published set_list schema.
        let schema = serde_json::to_value(&definition().parameters).expect("serializes");

        // When inspecting the phase item's task item schema.
        let items = schema
            .pointer("/properties/phases/items/properties/tasks/items")
            .expect("task items schema present");

        // Then it is a single object type, not a string-or-object union.
        assert_eq!(
            items.get("type").and_then(serde_json::Value::as_str),
            Some("object")
        );
        assert!(items.get("oneOf").is_none(), "no union expected: {items}");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_task_items_require_description_and_status() {
        // Given the published set_list schema.
        let schema = serde_json::to_value(&definition().parameters).expect("serializes");

        // When inspecting the task item's required keys.
        let items = schema
            .pointer("/properties/phases/items/properties/tasks/items")
            .expect("task items schema present");
        let required = items
            .get("required")
            .and_then(serde_json::Value::as_array)
            .expect("required present");

        // Then both fields are required.
        let required: Vec<&str> = required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(required, vec!["description", "status"]);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_task_items_reject_unknown_keys() {
        // Given the published set_list schema.
        let schema = serde_json::to_value(&definition().parameters).expect("serializes");

        // When inspecting the task item's closed-ness and status enum.
        let items = schema
            .pointer("/properties/phases/items/properties/tasks/items")
            .expect("task items schema present");

        // Then unknown keys are rejected and the enum is unchanged.
        assert_eq!(
            items
                .get("additionalProperties")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        let statuses = items
            .pointer("/properties/status/enum")
            .and_then(serde_json::Value::as_array)
            .expect("status enum present");
        assert_eq!(statuses.len(), 3);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_replaces_entire_list() {
        // Given a session that already holds an "Old Phase" list.
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

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the result reports the replacement and carries only the new list.
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
        // Given a session that already holds an "Old Phase" list.
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

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the taskless phase renders an empty-task marker.
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(result.content.contains("(no tasks)"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_with_empty_phases_clears_list() {
        // Given a session that already holds an "Old Phase" list.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({ "phases": [] }).to_string(),
        };
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the result reports the clear and the session's task list is empty.
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Task list cleared"),
            "expected clear message, got: {:?}",
            result.content
        );
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert!(session.task_list().is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn set_list_clear_on_already_empty_succeeds() {
        // Given a session whose task list is already empty.
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

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call succeeds and the list is still empty.
        assert!(result.success, "expected success: {:?}", result.content);
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert!(session.task_list().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_list_clear_publishes_task_list_updated() {
        // Given a session with an existing list and a recorder on the bus.
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
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
        let events = jinn_testutil::bus_harness::await_recorded(
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
        // Given a session that already holds an "Old Phase" list.
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

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call fails naming the missing field.
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
        // Given a tool context with no application state.
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Test" }]
            })
            .to_string(),
        };
        let ctx = make_context(None, Some(SessionId::new()));

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call fails naming the missing state.
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }

    #[rstest::rstest]
    #[test]
    fn set_list_return_has_next_block_at_top() {
        // Given a session that already holds an "Old Phase" list.
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

        // When executing the call.
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the NEXT block leads the result.
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
        // Given a payload declaring a status jinn cannot represent.
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

        // Then the call fails, naming the word and the accepted vocabulary.
        assert!(!result.success);
        assert!(
            result
                .content
                .contains(&format!("unknown status \"{status}\"")),
            "got: {:?}",
            result.content
        );
        assert!(
            result
                .content
                .contains("expected pending, completed, or cancelled"),
            "got: {:?}",
            result.content
        );
        // And the existing list is untouched (no partial write).
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert_eq!(session.task_list().phases()[0].description(), "Old Phase");
    }

    #[rstest::rstest]
    #[test]
    fn set_list_accepts_model_status_vocabulary() {
        // Given a payload using the status words a model trained on other
        // task tooling reaches for.
        let (state, session_id) = setup_with_existing_list();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_list".to_owned(),
            arguments: serde_json::json!({
                "phases": [{ "description": "Build", "tasks": [
                    { "description": "Running", "status": "in_progress" },
                    { "description": "Finished", "status": "done" },
                    { "description": "Dropped", "status": "skipped" }
                ]}]
            })
            .to_string(),
        };

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the call is written rather than failed, with each alias
        // coerced to the declarable status it stands for.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let tasks = &session.task_list().phases()[0].tasks;
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert_eq!(tasks[1].status, TaskStatus::Completed);
        assert_eq!(tasks[2].status, TaskStatus::Cancelled);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_alias_and_declared_statuses_return_identical_results() {
        // Given the same list written twice, once with model vocabulary and
        // once with the declared statuses.
        let (alias_state, alias_session) = setup_with_existing_list();
        let (declared_state, declared_session) = setup_with_existing_list();
        let alias_call = set_list_call(serde_json::json!([
            { "description": "Build", "tasks": [
                { "description": "One", "status": "in_progress" },
                { "description": "Two", "status": "done" },
                { "description": "Three", "status": "skipped" }
            ]}
        ]));
        let declared_call = set_list_call(serde_json::json!([
            { "description": "Build", "tasks": [
                { "description": "One", "status": "pending" },
                { "description": "Two", "status": "completed" },
                { "description": "Three", "status": "cancelled" }
            ]}
        ]));

        // When executing both calls.
        let alias_result = futures::executor::block_on(execute(
            alias_call,
            make_context(Some(alias_state.clone()), Some(alias_session.clone())),
        ));
        let declared_result = futures::executor::block_on(execute(
            declared_call,
            make_context(Some(declared_state), Some(declared_session)),
        ));
        assert!(
            alias_result.success,
            "alias call failed: {:?}",
            alias_result.content
        );

        // Then the coercion leaves no trace in what the caller reads back.
        assert_eq!(alias_result.content, declared_result.content);
    }

    #[rstest::rstest]
    #[test]
    fn set_list_unknown_status_aborts_the_whole_write() {
        // Given a payload whose phases are valid until one task's status is not.
        let (state, session_id) = setup_with_existing_list();
        let call = set_list_call(serde_json::json!([
            { "description": "Research" },
            { "description": "Build", "tasks": [
                { "description": "Fine", "status": "done" },
                { "description": "Bad", "status": "blocked" }
            ]}
        ]));

        // When executing the tool.
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call fails and the valid phases are not committed.
        assert!(!result.success);
        let list = read_list(&state, &session_id);
        assert_eq!(list.phases().len(), 1);
        assert_eq!(list.phases()[0].description(), "Old Phase");
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
