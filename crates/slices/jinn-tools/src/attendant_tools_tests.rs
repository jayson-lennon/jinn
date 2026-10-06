//! Tests for the `report` and `notify_parent` attendant tools.

#![allow(clippy::expect_used, reason = "test code")]

use std::time::Duration;

use jinn_attendant_msg::AttendantTrigger;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_common::app_paths::AppPaths;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_state::ChatSessionState;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

use crate::attendant_tools::{
    conclude_definition, conclude_execute, notify_parent_definition, notify_parent_execute,
};
use crate::tool_types::ToolContext;

/// Builds a tool call with the given JSON arguments.
fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_att_1".to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Builds a tool context wired to the harness bus and shared state.
async fn ctx(harness: &TestHarness, state: &State, session_id: SessionId) -> ToolContext {
    ToolContext {
        cwd: std::path::PathBuf::from("/tmp"),
        config: jinn_config::testutil::config_layer(""),
        timeout: None,
        state: Some(state.clone()),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: Some(harness.bus()),
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: None,
        session_store: None,
        trouper_system: Some(harness.system().clone()),
    }
}

/// Seeds an attendant with a parent; returns (state, attendant_id, parent_id).
fn attendant_fixture() -> (State, SessionId, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let parent = ChatSessionState::new();
    let parent_id = parent.session_id().clone();
    let attendant = ChatSessionState::new_attendant(&parent, true);
    let attendant_id = attendant.session_id().clone();
    {
        let mut guard = state.write();
        guard.session.insert(parent);
        guard.session.insert(attendant);
    }
    (state, attendant_id, parent_id)
}

#[rstest::rstest]
#[tokio::test]
async fn conclude_appends_to_the_callers_own_log() {
    // Given an attendant session.
    let harness = TestHarness::new().await;
    let (state, attendant_id, _parent) = attendant_fixture();
    let tool_ctx = ctx(&harness, &state, attendant_id.clone()).await;

    // When the report tool runs.
    let result = conclude_execute(
        call("conclude", r#"{"body":"the build was green"}"#),
        tool_ctx,
    )
    .await;

    // Then the tool succeeds and the log holds the body.
    assert!(result.success, "report must succeed: {}", result.content);
    let guard = state.read();
    let session = guard.session.get(&attendant_id).expect("attendant");
    assert_eq!(session.attendant_reports().len(), 1);
    assert_eq!(session.attendant_reports()[0].body, "the build was green");
    assert_eq!(session.attendant_reports()[0].run, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn report_does_not_wake_the_parent() {
    // Given an attendant with a live parent, on a bus with a recorder for
    // the enqueue that would wake it.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, attendant_id, _parent) = attendant_fixture();
    let tool_ctx = ctx(&harness, &state, attendant_id).await;

    // When the report tool runs.
    let result = conclude_execute(call("conclude", r#"{"body":"a finding"}"#), tool_ctx).await;

    // Then nothing is enqueued anywhere — report is terminal.
    assert!(result.success);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(enqueues.is_empty(), "report must not wake any session");
}

#[rstest::rstest]
#[tokio::test]
async fn conclude_persists_the_session() {
    // Given an attendant on a bus with a recorder for the persist command.
    let harness = TestHarness::new().await;
    let persists = harness
        .spawn_recorder::<jinn_session_store_msg::PersistSession>()
        .await;
    let (state, attendant_id, _parent) = attendant_fixture();
    let tool_ctx = ctx(&harness, &state, attendant_id.clone()).await;

    // When the report tool runs.
    let result = conclude_execute(call("conclude", r#"{"body":"a finding"}"#), tool_ctx).await;

    // Then the calling session is queued for persistence.
    assert!(result.success);
    let persisted = await_recorded::<jinn_session_store_msg::PersistSession>(
        &persists,
        1,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].session_id, attendant_id);
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_enqueues_a_user_message_into_the_parent() {
    // Given an attendant with a live parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, attendant_id, parent_id) = attendant_fixture();
    let tool_ctx = ctx(&harness, &state, attendant_id).await;

    // When the notify tool runs.
    let result = notify_parent_execute(
        call("notify_parent", r#"{"message":"look at this"}"#),
        tool_ctx,
    )
    .await;

    // Then the parent receives a User-kind enqueue.
    assert!(result.success, "notify must succeed: {}", result.content);
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].session_id, parent_id);
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } = &wakes[0].entry.kind
    else {
        panic!("the wake must be a User entry (title-derivation trap)");
    };
    assert_eq!(display, "look at this");
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_marks_the_parent_interacted() {
    // Given an attendant with a ParentCompleted trigger and a fresh parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, attendant_id, parent_id) = attendant_fixture();
    {
        let mut guard = state.write();
        guard
            .session
            .get_mut(&attendant_id)
            .expect("attendant")
            .set_attendant_trigger(AttendantTrigger::ParentCompleted);
    }
    let tool_ctx = ctx(&harness, &state, attendant_id).await;

    // When the notify tool runs.
    let result =
        notify_parent_execute(call("notify_parent", r#"{"message":"wake up"}"#), tool_ctx).await;
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;

    // Then the parent is interacted (persistable) and its turn is a normal
    // enqueue — the notification is not a suppressed, internal wake.
    assert!(result.success);
    assert_eq!(wakes.len(), 1);
    let guard = state.read();
    let parent = guard.session.get(&parent_id).expect("parent");
    assert!(parent.has_interacted(), "parent must be persistable");
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_fails_for_a_non_attendant() {
    // Given a plain user session (no parent link).
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let state = State::new(AppState::default_with_scope_focus());
    let user_id = {
        let session = ChatSessionState::new();
        let id = session.session_id().clone();
        state.write().session.insert(session);
        id
    };
    let tool_ctx = ctx(&harness, &state, user_id).await;

    // When the notify tool runs.
    let result =
        notify_parent_execute(call("notify_parent", r#"{"message":"hi"}"#), tool_ctx).await;

    // Then the call fails and nothing is enqueued.
    assert!(!result.success);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(enqueues.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn neither_tool_exposes_a_read_path_for_conclusions() {
    // Given the two tools' definitions.
    let definitions = [conclude_definition(), notify_parent_definition()];

    // When their parameter names are collected.
    let mut names = Vec::new();
    for definition in &definitions {
        let properties = definition.parameters["properties"]
            .as_object()
            .expect("properties object");
        names.extend(properties.keys().cloned());
    }

    // Then the only parameters are the two bodies — no session, target, or
    // read parameter exists, so the only way to see a report is the session
    // that wrote it.
    assert_eq!(definitions[0].name, "conclude");
    assert_eq!(definitions[1].name, "notify_parent");
    names.sort();
    assert_eq!(names, vec!["body".to_owned(), "message".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn conclude_fails_cleanly_with_a_missing_body() {
    // Given an attendant and a call whose arguments lack `body`.
    let harness = TestHarness::new().await;
    let (state, attendant_id, _parent) = attendant_fixture();
    let tool_ctx = ctx(&harness, &state, attendant_id).await;

    // When the report tool runs.
    let result = conclude_execute(call("conclude", r#"{}"#), tool_ctx).await;

    // Then the call fails with the missing slot named.
    assert!(!result.success);
    assert!(result.content.contains("body"));
}

#[rstest::rstest]
fn every_declared_parameter_is_one_the_executor_reads() {
    // Given the conclude tool's schema.
    let def = conclude_definition();
    let declared = def.parameters["properties"]
        .as_object()
        .expect("a properties object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();

    // Then it declares exactly the argument the executor requires.
    // A schema that names a property the executor never reads accepts a
    // perfectly valid-looking call and then rejects it for a missing
    // argument, which reads to the agent as the tool being broken.
    let required = def.parameters["required"]
        .as_array()
        .expect("a required array");
    assert_eq!(
        declared,
        vec!["body".to_owned()],
        "declared parameters must be exactly what the executor reads"
    );
    // And the required list names the same one field, not a stale name.
    assert_eq!(required.len(), 1, "conclude takes exactly one argument");
    assert_eq!(required[0].as_str(), Some("body"));
}

#[rstest::rstest]
fn the_conclude_description_states_the_length_bound() {
    // Given the conclude tool's description.
    let def = conclude_definition();

    // Then it states the bound. The name carries the once-per-run idea;
    // the length limit is the one thing prose has to say, and an agent
    // that ignored the example format still reads this.
    assert!(
        def.description.contains("10 WORDS"),
        "the description must state the word bound: {}",
        def.description
    );
}
