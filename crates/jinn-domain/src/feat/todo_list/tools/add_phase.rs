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

//! `add_phase` built-in tool - adds a new phase to the task list.

use crate::feat::tools_actor::BoxedToolFuture;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};

/// Returns the tool definition for `add_phase`.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "todo_add_phase".to_owned(),
        description: "Add a new phase (task group) to the todo list. \
            Phases are high-level stages of work. \
            Each phase can contain multiple tasks. \
            Returns the new phase ID and the updated todo list."
            .to_owned(),
        prompt_snippet: Some(
            "Add a new phase to the task list to organize work into stages".to_owned(),
        ),
        prompt_guidelines: vec![
            "Use todo_add_phase to define high-level stages before adding individual tasks.".to_owned(),
            "Phase descriptions should be short and descriptive (e.g., 'Research', 'Build', 'Test')."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Human-readable description of the phase (e.g., 'Research', 'Implementation')"
                }
            },
            "required": ["description"],
            "additionalProperties": false
        }),
        server_tool_type: None,
    }
}

/// Executes the `add_phase` tool.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(state) = ctx.state else {
            return ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: "Error: no application state available".to_owned(),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            };
        };
        let Some(session_id) = ctx.session_id else {
            return ToolResult {
                tool_call_id: call.id,
                name: call.name,
                content: "Error: no session ID available".to_owned(),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            };
        };

        // Parse arguments.
        let args: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);

        let description = match args.get("description").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => {
                return ToolResult {
                    tool_call_id: call.id,
                    name: call.name,
                    content: "Error: missing 'description' argument".to_owned(),
                    success: false,
                    full_content: None,
                    truncation: None,
                    pin_position: None,
                };
            }
        };

        let Some(session_cap) = &ctx.session_cap else {
            return tool_error(call, "no session capability");
        };
        let (phase_id, next_block, rendered) = state.with_session(session_cap, |view| {
            let session = view.session.map().get_unchecked_mut(&session_id);
            let list = session.task_list_mut();
            (
                list.add_phase(&description),
                list.render_next_block(),
                list.render_text_with_blockers(),
            )
        });

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
            content: format!("{next_block}\nCreated phase [{phase_id}].\n\n{rendered}"),
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

    #[rstest::rstest]
    #[test]
    fn add_phase_returns_phase_id_and_updated_list() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };

        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: r#"{"description": "Research"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success, "expected success: {:?}", result.content);
        assert!(
            result.content.contains("Phase 1: Research"),
            "should show phase: {:?}",
            result.content
        );
    }

    #[rstest::rstest]
    #[test]
    fn add_phase_requires_state() {
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: r#"{"description": "Research"}"#.to_owned(),
        };
        let ctx = make_context(None, Some(SessionId::new()));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no application state"));
    }

    #[rstest::rstest]
    #[test]
    fn add_phase_requires_session_id() {
        let app = AppState::default();
        let state = State::new(app);
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: r#"{"description": "Research"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), None);
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("no session ID"));
    }

    #[rstest::rstest]
    #[test]
    fn add_phase_requires_description() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: "{}".to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(!result.success);
        assert!(result.content.contains("missing 'description'"));
    }

    #[rstest::rstest]
    #[test]
    fn add_phase_return_has_next_block_at_top() {
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: r#"{"description": "Research"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // Empty list has no tasks to point at; NEXT block is empty (no leading '\u{2192}').
        assert!(
            !result.content.starts_with("\u{2192}"),
            "empty task list should produce no NEXT block, got: {:?}",
            result.content
        );
        assert!(result.content.contains("Created phase"));
    }

    #[rstest::rstest]
    #[test]
    fn add_phase_new_trailing_phase_renders_with_blocker_when_active_phase_has_work() {
        // Existing phase has a pending task; the newly added trailing phase
        // has no tasks yet, so it must NOT carry the blocker prefix (no pending work).
        let app = AppState::default();
        let state = State::new(app);
        let session_id = {
            let r = state.read();
            r.session.active_session_id().clone()
        };
        {
            let mut w = state.write_test_no_cap();
            let session = w.session_mut(&session_id);
            let p1 = session.task_list_mut().add_phase("First");
            session
                .task_list_mut()
                .add_task(&p1, "work", TaskPosition::End)
                .unwrap();
        }
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "todo_add_phase".to_owned(),
            arguments: r#"{"description": "Second"}"#.to_owned(),
        };
        let ctx = make_context(Some(state), Some(session_id));
        let result = execute(call, ctx);
        let result = futures::executor::block_on(result);
        assert!(result.success);
        // Second phase has no pending tasks (empty), so it renders without prefix.
        assert!(!result.content.contains("(Blocked by previous phase)"));
    }
}
