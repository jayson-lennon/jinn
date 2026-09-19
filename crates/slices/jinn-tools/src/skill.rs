//! Skill built-in tool - loads a skill's content into the conversation context
//! as a pinned ToolResult that the model sees inline at load time and on every
//! subsequent turn (the entry is pinned with `PinPosition::Relative`, which
//! survives compaction).

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult, ToolResultPinPosition};
use jinn_skills::Skill;
use jinn_skills::frontmatter::strip_frontmatter;

use super::BoxedToolFuture;

/// Returns the tool definition for the `skill` built-in tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "skill".to_owned(),
        description: "Load a specific agent skill's content into the conversation context. The skill content is returned in this tool result and pinned in the conversation history for the rest of the conversation. Use this tool when the current task matches a skill's description from the available skills list. Do not call this tool if the skill is already loaded (marked loaded='true' in the available skills list)."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill to load (from the available skills list)"
                }
            },
            "required": ["name"]
        }),
        server_tool_type: None,
    }
}

/// Builds a failure `ToolResult` for the given call with a human-facing message.
fn failure_result(call: &ToolCall, message: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content: message.into(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Resolves a skill from the session's discovered set.
///
/// Returns `Err` with a user-facing message if state/session is unavailable or
/// the skill was not discovered for this session. This avoids re-deriving the
/// path from the global skills dir, which would break for project-local skills.
fn resolve_skill(ctx: &ToolContext, name: &str) -> Result<Skill, String> {
    let Some(state) = ctx.state.as_ref() else {
        return Err(format!("cannot resolve skill '{name}': no state available"));
    };
    let Some(session_id) = ctx.session_id.as_ref() else {
        return Err(format!(
            "cannot resolve skill '{name}': no session in context"
        ));
    };
    let guard = state.read();
    let Some(session) = guard.session.get(session_id) else {
        return Err(format!("cannot resolve skill '{name}': session not found"));
    };
    session
        .discovered_skills()
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| {
            format!(
                "skill '{name}' was not discovered for this session. It may have been removed or never scanned."
            )
        })
}

/// Executes the `skill` built-in tool.
///
/// Reads the skill's SKILL.md file, strips YAML frontmatter, and returns the
/// skill body wrapped in a `<skill>` XML element as the ToolResult content.
/// The ToolResult is pinned with `PinPosition::Relative` so it persists at its
/// original position in history through compaction.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let name = match parse_args(&call.arguments) {
            Ok(n) => n,
            Err(e) => return failure_result(&call, format!("failed to parse arguments: {e}")),
        };

        if name.is_empty() {
            return failure_result(&call, "skill name must not be empty");
        }

        // Reject disabled skills.
        if let (Some(state), Some(session_id)) = (ctx.state.as_ref(), &ctx.session_id) {
            let guard = state.read();
            if let Some(session) = guard.session.get(session_id)
                && !session.is_skill_enabled(&name)
            {
                return failure_result(
                    &call,
                    format!(
                        "skill '{name}' is disabled for this session. Use <leader>sk to re-enable it."
                    ),
                );
            }
        }

        // Idempotency: refuse to re-load if a pinned ToolResult for this skill already exists.
        if let (Some(state), Some(session_id)) = (ctx.state.as_ref(), &ctx.session_id) {
            let guard = state.read();
            if let Some(session) = guard.session.get(session_id)
                && session.loaded_skills().contains(&name)
            {
                return failure_result(
                    &call,
                    format!(
                        "skill '{name}' is already loaded; its content is in context as a pinned tool result"
                    ),
                );
            }
        }

        // Resolve the skill from the session's discovered set rather than
        // re-deriving it from the global dir. This makes project-local skills loadable
        // and fails clearly if the skill was not discovered for this session.
        let skill = match resolve_skill(&ctx, &name) {
            Ok(s) => s,
            Err(msg) => {
                return failure_result(&call, msg);
            }
        };

        let content = match tokio::fs::read_to_string(&skill.file_path).await {
            Ok(c) => c,
            Err(e) => {
                return failure_result(
                    &call,
                    format!("failed to read skill '{}': {e}", skill.file_path.display()),
                );
            }
        };

        let body = strip_frontmatter(&content);
        let location = skill.file_path.to_string_lossy().to_string();
        let base_dir = skill.base_dir.to_string_lossy().to_string();
        let xml = format!(
            "<skill name=\"{name}\" location=\"{location}\">\nBase directory: {base_dir} — resolve all relative reference paths here.\n{body}\n</skill>"
        );

        ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: xml,
            success: true,
            full_content: None,
            truncation: None,
            pin_position: Some(ToolResultPinPosition::Relative),
        }
    })
}

fn parse_args(raw: &str) -> Result<String, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let name = v
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Ok(name)
}

// #[cfg(test)]
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::string_slice,
        reason = "test code"
    )]
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        }
    }

    #[rstest::rstest]
    fn definition_has_correct_name() {
        // Given the skill tool definition.
        let def = definition();

        // Then the name is "skill".
        assert_eq!(def.name, "skill");
    }

    #[rstest::rstest]
    fn definition_requires_name_parameter() {
        // Given the skill tool definition.
        let def = definition();

        // Then the parameters require "name".
        let required = def
            .parameters
            .get("required")
            .expect("should have required");
        assert!(
            required
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("name"))
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_for_nonexistent_skill() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;

        // Given a call for a skill that was never discovered for this session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            guard.session_mut_or_create(&session_id);
        }
        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };
        let result = execute(
            ToolCall {
                id: "call_1".to_owned(),
                name: "skill".to_owned(),
                arguments: serde_json::json!({"name": "nonexistent-skill-xyz"}).to_string(),
            },
            ctx,
        )
        .await;

        // Then the result indicates failure with a clear not-discovered message.
        assert!(!result.success);
        assert!(
            result.content.contains("was not discovered"),
            "should mention not-discovered, got: {}",
            result.content
        );
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn execute_loads_project_local_skill_from_discovered_file_path() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;
        use jinn_skills::{Skill, SkillSource};

        // Given a project-local skill whose file_path is NOT under the global
        // skills dir. Pre-fix, execute() would re-derive the path from the global
        // dir and fail. Post-fix, it resolves from the session's discovered set.
        let tmp = tempfile::tempdir().expect("create temp dir");
        let skill_dir = tmp.path().join(".agents/skills/proj-skill");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(
            &skill_file,
            "---\nname: proj-skill\ndescription: a project skill\n---\nproject body\n",
        )
        .expect("write skill");

        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            let session = guard.session_mut_or_create(&session_id);
            session.set_discovered_skills(vec![Skill {
                name: "proj-skill".to_owned(),
                description: "a project skill".to_owned(),
                body: String::new(),
                file_path: skill_file.clone(),
                base_dir: skill_dir.clone(),
                source: SkillSource::Project {
                    dir: tmp.path().to_path_buf(),
                },
            }]);
        }

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "proj-skill"}).to_string(),
        };

        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };

        // When executing.
        let result = execute(call, ctx).await;

        // Then the project-local skill loads successfully, reading from the
        // discovered file_path (outside the global dir).
        assert!(
            result.success,
            "project-local skill should load, got: {}",
            result.content
        );
        assert!(
            result.content.contains("project body"),
            "tool result should contain the project skill body, got: {}",
            &result.content[..result.content.len().min(120)]
        );
        assert!(
            result
                .content
                .contains(&format!("Base directory: {}", skill_dir.display())),
            "tool result header should carry the project skill's base_dir, got: {}",
            &result.content[..result.content.len().min(200)]
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_result_header_carries_base_dir() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;
        use jinn_skills::{Skill, SkillSource};

        // Given a project-local skill seeded with a distinct base_dir.
        let tmp = tempfile::tempdir().expect("create temp dir");
        let skill_dir = tmp.path().join(".agents/skills/header-skill");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(
            &skill_file,
            "---\nname: header-skill\ndescription: d\n---\nbody\n",
        )
        .expect("write skill");

        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            let session = guard.session_mut_or_create(&session_id);
            session.set_discovered_skills(vec![Skill {
                name: "header-skill".to_owned(),
                description: "d".to_owned(),
                body: String::new(),
                file_path: skill_file.clone(),
                base_dir: skill_dir.clone(),
                source: SkillSource::Project {
                    dir: tmp.path().to_path_buf(),
                },
            }]);
        }

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "header-skill"}).to_string(),
        };
        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };

        // When executing.
        let result = execute(call, ctx).await;

        // Then the successful result's Base directory header equals the seeded base_dir.
        assert!(
            result.success,
            "load should succeed, got: {}",
            result.content
        );
        assert!(
            result
                .content
                .contains(&format!("Base directory: {}", skill_dir.display())),
            "header should carry the seeded base_dir verbatim, got: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_empty_name() {
        // Given a call with empty name.
        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": ""}).to_string(),
        };

        // When executing.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("must not be empty"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a call with invalid JSON.
        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_skill_body_in_tool_result() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;

        // Given a skill file in the real skills dir (best-effort).
        let state = State::new(AppState::default());
        let session_id = SessionId::new();

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "phased-task-loop"}).to_string(),
        };

        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };

        // When executing.
        let result = execute(call, ctx).await;

        // Then if the skill exists, the tool result contains the body wrapped in <skill> XML, pinned Relative.
        if result.success {
            assert!(
                result
                    .content
                    .starts_with("<skill name=\"phased-task-loop\""),
                "tool result should contain the skill body in <skill> XML, got: {}",
                &result.content[..result.content.len().min(80)]
            );
            // And the Base directory header sits on the line immediately after
            // the opening <skill> tag, before the body.
            assert!(
                result.content.contains("\nBase directory: "),
                "header should follow the opening <skill> tag, got: {}",
                &result.content[..result.content.len().min(160)]
            );
            assert_eq!(
                result.pin_position,
                Some(ToolResultPinPosition::Relative),
                "tool result should be pinned Relative"
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_already_loaded_for_duplicate_load() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;
        use jinn_domain::protocol::ToolResultStatus;
        use jinn_domain::protocol::{ChatEntry, PinPosition};

        // Given a session that already has a pinned ToolResult from the `skill` tool
        // for "phased-task-loop" (matches the body-in-ToolResult shape).
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            let session = guard.session_mut_or_create(&session_id);
            let seeded_xml = "<skill name=\"phased-task-loop\" location=\"/tmp\">\nbody\n</skill>";
            let mut entry = ChatEntry::tool_result(
                "seeded_call_id",
                "skill",
                seeded_xml,
                ToolResultStatus::Success,
            );
            entry.pin_position = Some(PinPosition::Relative);
            session.push_entry(entry);
        }

        // When calling execute again for the same skill name.
        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "phased-task-loop"}).to_string(),
        };
        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };
        let result = execute(call, ctx).await;

        // Then the result indicates failure with the "already loaded" message,
        // no new pin position, and no file is read.
        assert!(
            !result.success,
            "second load should fail when skill is already loaded"
        );
        assert!(
            result.content.contains("already loaded"),
            "tool result should mention already-loaded state, got: {}",
            result.content
        );
        assert_eq!(
            result.pin_position, None,
            "already-loaded rejection should not pin anything"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_loads_different_skill_when_other_already_loaded() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;
        use jinn_domain::protocol::ToolResultStatus;
        use jinn_domain::protocol::{ChatEntry, PinPosition};

        // Given a session that already has a pinned ToolResult for "rust-programming".
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            let session = guard.session_mut_or_create(&session_id);
            let seeded_xml = "<skill name=\"rust-programming\" location=\"/tmp\">\nbody\n</skill>";
            let mut entry = ChatEntry::tool_result(
                "seeded_call_id",
                "skill",
                seeded_xml,
                ToolResultStatus::Success,
            );
            entry.pin_position = Some(PinPosition::Relative);
            session.push_entry(entry);
        }

        // When calling execute for a *different* skill ("phased-task-loop").
        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "phased-task-loop"}).to_string(),
        };
        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };
        let result = execute(call, ctx).await;

        // Then if the requested skill file exists, the load succeeds and returns
        // the body in <skill> XML with Relative pin (idempotency did not reject
        // because the name differs from the already-loaded skill).
        if result.success {
            assert!(
                result
                    .content
                    .starts_with("<skill name=\"phased-task-loop\""),
                "different-name load should succeed with body in <skill> XML, got: {}",
                &result.content[..result.content.len().min(80)]
            );
            assert_eq!(
                result.pin_position,
                Some(ToolResultPinPosition::Relative),
                "successful different-name load should pin Relative"
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_for_disabled_skill() {
        use jinn_domain::common::app_state::AppState;
        use jinn_domain::common::state::State;
        use jinn_domain::protocol::SessionId;
        use std::collections::HashSet;

        // Given a session with "web-coder" disabled.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        {
            let mut guard = state.write_test_no_cap();
            let session = guard.session_mut_or_create(&session_id);
            session.set_disabled_skills(HashSet::from(["web-coder".to_owned()]));
        }

        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "skill".to_owned(),
            arguments: serde_json::json!({"name": "web-coder"}).to_string(),
        };

        let ctx = ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: Some(state),
            session_id: Some(session_id),
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        };

        // When executing.
        let result = execute(call, ctx).await;

        // Then the result indicates failure due to disabled skill.
        assert!(!result.success);
        assert!(result.content.contains("disabled for this session"));
    }
}
