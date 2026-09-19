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

//! `set_phase` built-in tool - writes one entire phase, keyed by description.
//!
//! The day-to-day write path: resend the phase with statuses declared inline
//! to record completions or cancellations. A phase whose description matches
//! an existing phase is replaced in place (first match wins); an unmatched
//! description appends a new phase, and the result says which happened.

use crate::BoxedToolFuture;
use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};

/// Returns the tool definition for `set_phase`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_set_phase".to_owned(),
        description: "Write one entire phase of the task list, matched by its description. \
            If a phase with this description exists, it is replaced (first match wins); \
            otherwise the phase is appended to the end of the list. Tasks declare their \
            status inline: each task is a bare string (created as pending) or an object \
            with 'description' and an optional 'status' (pending, completed, or \
            cancelled). Use this for day-to-day updates - write the entire phase \
            including unchanged tasks; use todo_set_list to restructure the whole list."
            .to_owned(),
        prompt_snippet: Some("Write one task-list phase".to_owned()),
        prompt_guidelines: vec![
            "Write the ENTIRE phase - every task you want it to contain, including \
             unchanged ones. Tasks left out of the payload are removed from the phase."
                .to_owned(),
            "Phase matching is exact (case-sensitive) on the trimmed description. \
             If no phase matches, a new phase is appended and the result says so - \
             check for typos when that wasn't intended."
                .to_owned(),
            "Record progress by resending the phase with statuses flipped: \
             {\"description\": \"...\", \"status\": \"completed\"}. Statuses: \
             pending, completed, cancelled. Bare strings stay pending."
                .to_owned(),
            "'postponed' is not a valid status - move the task to a later phase \
             or cancel it instead."
                .to_owned(),
            "To finish a whole phase and move on, flip all its tasks to completed \
             (or cancelled) in one call."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Description of the phase to write. Matched exactly against existing phase descriptions; appended as a new phase when nothing matches."
                },
                "tasks": {
                    "type": "array",
                    "description": "The complete ordered task list for this phase. Each task is a string (created as pending) or an object {description, status} with status one of: pending, completed, cancelled.",
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
        }),
        server_tool_type: None,
    }
}

/// Executes the `set_phase` tool.
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

        // Parse the single-phase payload with the shared grammar. Pure: any
        // payload error aborts before task list state is touched.
        let phase_input = match super::task_payload::parse_phase_body(&args, "phase") {
            Ok(input) => input,
            Err(msg) => return tool_error(call, &msg),
        };

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let phase_description = phase_input.description.clone();
        let result = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
            let list = session.task_list_mut();
            let replaced = list.set_phase_from_input(&phase_input);
            let next_block = list.render_next_block();
            let rendered = list.render_text_with_blockers();
            let verdict = if replaced {
                format!("Phase \"{}\" replaced.", phase_description)
            } else {
                format!("No phase matched \"{phase_description}\" - appended as a new phase.")
            };
            Ok(format!("{next_block}\n{verdict}\n\n{rendered}"))
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
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
        }
    }

    fn setup_with_two_phases() -> (State, SessionId) {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            session.task_list_mut().set_from_inputs(&[
                PhaseInput {
                    description: "Build".to_owned(),
                    tasks: vec![
                        ("Write code".to_owned(), TaskStatus::Pending),
                        ("Review code".to_owned(), TaskStatus::Pending),
                    ],
                },
                PhaseInput {
                    description: "Test".to_owned(),
                    tasks: vec![("Run suite".to_owned(), TaskStatus::Pending)],
                },
            ]);
        };
        (state, session_id)
    }

    fn set_phase_call(description: &str, tasks: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            name: "todo_set_phase".to_owned(),
            arguments: serde_json::json!({ "description": description, "tasks": tasks })
                .to_string(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_writes_entire_phase() {
        // Given a session with a Build phase holding two pending tasks.
        let (state, session_id) = setup_with_two_phases();

        // When writing the entire Build phase (one dropped, one completed, one added).
        let call = set_phase_call(
            "Build",
            serde_json::json!([
                { "description": "Write code", "status": "completed" },
                { "description": "Ship it" }
            ]),
        );
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the phase holds exactly the sent tasks with declared statuses.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let tasks = &session.task_list().phases()[0].tasks;
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].description, "Write code");
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].description, "Ship it");
        assert_eq!(tasks[1].status, TaskStatus::Pending);
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_preserves_other_phases() {
        // Given a session with Build and Test phases.
        let (state, session_id) = setup_with_two_phases();

        // When rewriting only Build.
        let call = set_phase_call("Build", serde_json::json!(["a", "b"]));
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success);

        // Then Test is untouched at its original position.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let phases = session.task_list().phases();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[1].description(), "Test");
        assert_eq!(phases[1].tasks()[0].description, "Run suite");
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_no_match_appends_and_says_so() {
        // Given a session whose phases don't include "Deploy".
        let (state, session_id) = setup_with_two_phases();

        // When writing a phase named Deploy.
        let call = set_phase_call("Deploy", serde_json::json!(["Push"]));
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the result explicitly reports the append.
        assert!(
            result
                .content
                .contains("No phase matched \"Deploy\" - appended as a new phase."),
            "got: {:?}",
            result.content
        );
        // And the new phase is at the end.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let phases = session.task_list().phases();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[2].description(), "Deploy");
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_first_match_replaced_on_duplicate_descriptions() {
        // Given a session with two phases both named Build.
        let (state, session_id) = setup_with_two_phases();
        {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            session.task_list_mut().set_from_inputs(&[
                PhaseInput {
                    description: "Build".to_owned(),
                    tasks: vec![("First build task".to_owned(), TaskStatus::Pending)],
                },
                PhaseInput {
                    description: "Build".to_owned(),
                    tasks: vec![("Second build task".to_owned(), TaskStatus::Pending)],
                },
                PhaseInput {
                    description: "Test".to_owned(),
                    tasks: vec![("Run suite".to_owned(), TaskStatus::Pending)],
                },
            ]);
        }

        // When writing to the duplicated description.
        let call = set_phase_call("Build", serde_json::json!(["rewritten"]));
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success);

        // Then only the first Build phase was replaced.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let phases = session.task_list().phases();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].tasks()[0].description, "rewritten");
        // And the second Build keeps its own task.
        assert_eq!(phases[1].tasks()[0].description, "Second build task");
    }

    #[rstest::rstest]
    #[test]
    fn multi_completion_in_one_set_phase_call() {
        // Given a session with a Build phase of three pending tasks.
        let (state, session_id) = setup_with_two_phases();

        // When flipping two tasks to completed and one to cancelled in one call.
        let call = set_phase_call(
            "Build",
            serde_json::json!([
                { "description": "Write code", "status": "completed" },
                { "description": "Review code", "status": "completed" },
                { "description": "Ship it", "status": "cancelled" }
            ]),
        );
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success, "expected success: {:?}", result.content);

        // Then the statuses are as declared, in a single tool call.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        let tasks = &session.task_list().phases()[0].tasks;
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].status, TaskStatus::Completed);
        assert_eq!(tasks[2].status, TaskStatus::Cancelled);
    }

    #[rstest::rstest]
    #[case("postponed")]
    #[case("deferred")]
    #[test]
    fn set_phase_rejects_postponed_status(#[case] status: &str) {
        // Given a payload declaring the non-declarable status.
        let (state, session_id) = setup_with_two_phases();

        // When executing the tool.
        let call = set_phase_call(
            "Build",
            serde_json::json!([{ "description": "t", "status": status }]),
        );
        let ctx = make_context(Some(state.clone()), Some(session_id.clone()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then the call fails with guidance.
        assert!(!result.success);
        assert!(
            result.content.contains("not a declarable status"),
            "got: {:?}",
            result.content
        );
        // And the phase is untouched.
        let snapshot = state.read();
        let session = snapshot.session.get(&session_id).expect("session present");
        assert_eq!(session.task_list().phases()[0].tasks().len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_replaced_result_says_replaced() {
        // Given a session with a Build phase.
        let (state, session_id) = setup_with_two_phases();

        // When rewriting it.
        let call = set_phase_call("Build", serde_json::json!(["x"]));
        let ctx = make_context(Some(state), Some(session_id));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success);

        // Then the result reports a replacement, not an append.
        assert!(
            result.content.contains("Phase \"Build\" replaced."),
            "got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn set_phase_result_has_next_block_at_top() {
        // Given a session with a Build phase.
        let (state, session_id) = setup_with_two_phases();

        // When rewriting it.
        let call = set_phase_call("Build", serde_json::json!(["x"]));
        let ctx = make_context(Some(state), Some(session_id));
        let result = futures::executor::block_on(execute(call, ctx));
        assert!(result.success);

        // Then the result opens with the NEXT cue.
        assert!(
            result.content.starts_with("\u{2192}"),
            "got: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_phase_publishes_task_list_updated() {
        // Given a session and a recorder on the bus.
        let harness = jinn_domain::common::bus::test_harness::TestHarness::new().await;
        let (state, session_id) = setup_with_two_phases();
        let recorder = harness
            .spawn_recorder::<jinn_session_history_msg::TaskListUpdated>()
            .await;
        let mut ctx = make_context(Some(state), Some(session_id.clone()));
        ctx.bus = Some(harness.bus());

        // When writing a phase.
        let call = set_phase_call("Build", serde_json::json!(["x"]));
        let result = execute(call, ctx).await;
        assert!(result.success, "expected success: {:?}", result.content);

        // Then exactly one TaskListUpdated is published for the session.
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
    fn set_phase_requires_state() {
        // Given no application state in the context.
        let call = set_phase_call("Build", serde_json::json!(["x"]));

        // When executing the tool.
        let ctx = make_context(None, Some(SessionId::new()));
        let result = futures::executor::block_on(execute(call, ctx));

        // Then it fails legibly.
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }
}
