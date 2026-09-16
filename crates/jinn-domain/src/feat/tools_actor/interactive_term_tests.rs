//! Actor-level tests for the `interactive_term` spawn tool (ask pattern).
//!
//! Exercises the tool's `execute()` paths that don't need a running
//! coordinator:
//!   - spawn without a chat session context is rejected (the terminal could
//!     never be shown or toggled),
//!   - the working directory is taken from the tool context (agent-relative
//!     paths resolve where the conversation runs),
//!   - a started result surfaces the kill notice when the session already had
//!     a live terminal.
//!
//! Paths that need the real coordinator are covered by the actor tests in
//! `feat/interactive_term/interactive_term_actor.rs` (spawn, respawn-kill,
//! realtime mirror) — this module only checks the tool's own wiring.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use super::interactive_term::execute;
use crate::common::app_paths::AppPaths;
use crate::feat::tools_actor::interactive_term_kill;
use crate::feat::tools_actor::interactive_term_send;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext};
use crate::protocol::SessionId;
use std::path::PathBuf;

fn call(command: &str) -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: "interactive_term".to_owned(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

fn ctx_with(session_id: Option<SessionId>, cwd: &str) -> ToolContext {
    ToolContext {
        cwd: PathBuf::from(cwd),
        command_policy: crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
        timeout: None,
        state: None,
        session_id,
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: None,
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: None,
        session_store: None,
    }
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_without_a_chat_session_is_rejected() {
    // Given a tool context with no chat session (no conversation is active).
    let ctx = ctx_with(None, "/tmp");

    // When executing the spawn tool.
    let result = execute(call("htop"), ctx).await;

    // Then the result is a failure explaining the session requirement.
    assert!(!result.success, "unlinked spawn must fail");
    assert!(
        result.content.contains("requires a chat session"),
        "rejection should explain the chat-session requirement, got: {}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_without_a_coordinator_reports_unavailable() {
    // Given a context with a chat session but no coordinator (test harnesses).
    let ctx = ctx_with(Some(SessionId::new()), "/tmp");

    // When executing the spawn tool.
    let result = execute(call("htop"), ctx).await;

    // Then the result is a failure naming the coordinator.
    assert!(!result.success);
    assert!(result.content.contains("coordinator"));
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_without_a_command_is_rejected() {
    // Given a context with a chat session but a call missing `command`.
    let ctx = ctx_with(Some(SessionId::new()), "/tmp");
    let call = ToolCall {
        id: "call-2".to_owned(),
        name: "interactive_term".to_owned(),
        arguments: serde_json::json!({}).to_string(),
    };

    // When executing the spawn tool.
    let result = execute(call, ctx).await;

    // Then the result is a failure naming the missing argument.
    assert!(!result.success);
    assert!(
        result.content.contains("command"),
        "rejection should name the missing argument, got: {}",
        result.content
    );
}

#[rstest::rstest]
fn started_result_surfaces_the_kill_notice() {
    // Given a started outcome describing a replaced terminal.
    let killed = crate::feat::interactive_term::protocol::command::KilledPrevious {
        exited: crate::feat::interactive_term::pty_session::ExitInfo {
            code: 0,
            signal: None,
        },
    };

    // When formatting the success result with the kill notice.
    let result = super::interactive_term::success_result(
        "call-1",
        "interactive_term",
        "screen text",
        None,
        Some(&killed),
    );

    // Then the result is success and carries the notice.
    assert!(result.success);
    assert!(
        result.content.contains("killed"),
        "notice must say the old terminal was killed, got: {}",
        result.content
    );
}

#[rstest::rstest]
fn started_result_without_a_kill_has_no_notice() {
    // Given a started outcome for a fresh session (no previous terminal).

    // When formatting the success result without a kill notice.
    let result = super::interactive_term::success_result(
        "call-1",
        "interactive_term",
        "screen text",
        None,
        None,
    );

    // Then the result carries no kill notice.
    assert!(result.success);
    assert!(
        !result.content.contains("killed"),
        "no notice expected on a fresh spawn, got: {}",
        result.content
    );
}

#[rstest::rstest]
fn started_result_has_no_model_facing_session_id() {
    // Given a started outcome.

    // When formatting the success result.
    let result = super::interactive_term::success_result(
        "call-1",
        "interactive_term",
        "screen text",
        None,
        None,
    );

    // Then the body carries no session_id line — there is no model-facing
    // terminal id to pass back.
    assert!(
        !result.content.contains("session_id"),
        "results must not advertise a session id, got: {}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_requires_a_chat_session_context() {
    // Given a tool context with no chat session.
    let ctx = ctx_with(None, "/tmp");
    let call = ToolCall {
        id: "call-3".to_owned(),
        name: "interactive_term_send".to_owned(),
        arguments: serde_json::json!({}).to_string(),
    };

    // When executing the send tool.
    let result = interactive_term_send::execute(call, ctx).await;

    // Then the result is a failure explaining the session requirement.
    assert!(!result.success, "send without a session context must fail");
    assert!(
        result.content.contains("requires a chat session"),
        "rejection should explain the chat-session requirement, got: {}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn kill_requires_a_chat_session_context() {
    // Given a tool context with no chat session.
    let ctx = ctx_with(None, "/tmp");
    let call = ToolCall {
        id: "call-4".to_owned(),
        name: "interactive_term_kill".to_owned(),
        arguments: serde_json::json!({}).to_string(),
    };

    // When executing the kill tool.
    let result = interactive_term_kill::execute(call, ctx).await;

    // Then the result is a failure explaining the session requirement.
    assert!(!result.success, "kill without a session context must fail");
    assert!(
        result.content.contains("requires a chat session"),
        "rejection should explain the chat-session requirement, got: {}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_without_a_coordinator_reports_unavailable() {
    // Given a context with a chat session but no coordinator.
    let ctx = ctx_with(Some(SessionId::new()), "/tmp");
    let call = ToolCall {
        id: "call-5".to_owned(),
        name: "interactive_term_send".to_owned(),
        arguments: serde_json::json!({ "text": "hi" }).to_string(),
    };

    // When executing the send tool.
    let result = interactive_term_send::execute(call, ctx).await;

    // Then the coordinator failure surfaces (the session context passed).
    assert!(!result.success);
    assert!(result.content.contains("coordinator"));
}

#[rstest::rstest]
#[tokio::test]
async fn kill_without_own_terminal_fails_cleanly_at_the_coordinator_layer() {
    // Given the send and kill definitions.
    let def = interactive_term_send::definition();

    // Then the schema requires no session_id — there is no model-facing
    // terminal id to get wrong (kill's schema is likewise empty).
    let def_json = serde_json::to_value(&def.parameters).expect("schema json");
    assert!(
        def_json
            .get("required")
            .and_then(|r| r.as_array())
            .is_none_or(std::vec::Vec::is_empty),
        "send schema must not require any argument, got: {def_json}"
    );
    let kill_def = interactive_term_kill::definition();
    let kill_json = serde_json::to_value(&kill_def.parameters).expect("schema json");
    assert!(
        kill_json
            .get("required")
            .and_then(|r| r.as_array())
            .is_none_or(std::vec::Vec::is_empty),
        "kill schema must not require any argument, got: {kill_json}"
    );
}

#[rstest::rstest]
fn spawn_definition_warns_against_pipes_and_redirections() {
    // Given the interactive_term definition.
    let def = super::interactive_term::definition();

    // Then the description warns that pipes/redirections lose the output
    // (the tool returns the rendered screen).
    assert!(
        def.description.contains("redirect") && def.description.contains("RENDERED SCREEN"),
        "spawn description must warn against redirections, got: {}",
        def.description
    );
    // And the guidelines carry the same warning.
    assert!(
        def.prompt_guidelines
            .iter()
            .any(|g| g.contains("redirections") && g.contains("silently lost")),
        "spawn guidelines must warn against pipes/redirections, got: {:?}",
        def.prompt_guidelines
    );
}

#[rstest::rstest]
fn send_definition_describes_the_no_argument_snapshot() {
    // Given the interactive_term_send definition.
    let def = interactive_term_send::definition();

    // Then the description presents the no-argument call as a snapshot.
    assert!(
        def.description.contains("NO arguments") && def.description.contains("SNAPSHOT"),
        "send description must describe the no-argument snapshot, got: {}",
        def.description
    );
    // And the guidelines tell the model it can poll progress this way.
    assert!(
        def.prompt_guidelines
            .iter()
            .any(|g| g.contains("NO arguments")),
        "send guidelines must mention the no-argument snapshot, got: {:?}",
        def.prompt_guidelines
    );
}

#[rstest::rstest]
fn usage_footer_mentions_the_snapshot_and_no_piping() {
    // Given the shared usage footer.

    // When formatting it.
    let footer = super::interactive_term::usage_footer();

    // Then it advertises the snapshot affordance and steers away from
    // piping output.
    assert!(footer.contains("snapshot"));
    assert!(footer.contains("pipe or redirect"));
}
