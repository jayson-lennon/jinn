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
    let term_id = crate::feat::interactive_term::protocol::command::TermSessionId("term-1".into());
    let killed = crate::feat::interactive_term::protocol::command::KilledPrevious {
        session_id: crate::feat::interactive_term::protocol::command::TermSessionId(
            "term-0".into(),
        ),
        exited: crate::feat::interactive_term::pty_session::ExitInfo {
            code: 0,
            signal: None,
        },
    };

    // When formatting the success result with the kill notice.
    let result = super::interactive_term::success_result(
        "call-1",
        "interactive_term",
        &term_id,
        "screen text",
        None,
        Some(&killed),
    );

    // Then the result is success and carries the notice.
    assert!(result.success);
    assert!(
        result.content.contains("term-0"),
        "notice must name the killed session, got: {}",
        result.content
    );
    assert!(
        result.content.contains("killed"),
        "notice must say the old terminal was killed, got: {}",
        result.content
    );
}

#[rstest::rstest]
fn started_result_without_a_kill_has_no_notice() {
    // Given a started outcome for a fresh session (no previous terminal).
    let term_id = crate::feat::interactive_term::protocol::command::TermSessionId("term-2".into());

    // When formatting the success result without a kill notice.
    let result = super::interactive_term::success_result(
        "call-1",
        "interactive_term",
        &term_id,
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
