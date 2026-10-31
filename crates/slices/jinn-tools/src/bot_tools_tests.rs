//! Tests for the `report` and `notify_parent` bot tools.

#![allow(clippy::expect_used, reason = "test code")]

use std::time::Duration;

use jinn_bot_msg::BotTrigger;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_common::app_paths::AppPaths;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_state::ChatSessionState;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use marked_path::{Absolute, MarkedPathBuf};

use crate::bot_tools::{
    conclude_definition, conclude_execute, notify_parent_definition, notify_parent_execute,
};
use crate::tool_types::ToolContext;

/// Builds a tool call with the given JSON arguments.
fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_att_1".into(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Builds a tool context wired to the harness bus and shared state, rooted at
/// `paths_root` — a directory the calling test owns for its whole run.
async fn ctx(
    harness: &TestHarness,
    state: &State,
    session_id: SessionId,
    paths_root: &MarkedPathBuf<Absolute>,
) -> ToolContext {
    ToolContext {
        turn: crate::tool_types::test_turn(),
        cwd: paths_root.clone(),
        config: jinn_config::testutil::config_layer(""),
        shells: Default::default(),
        timeout: None,
        state: Some(state.clone()),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(&paths_root.as_marked_path()),
        bus: Some(harness.bus()),
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        mcp_coordinator: None,
        canvas_server: None,
        interactive_term: None,
        task_spawns: None,
        session_store: None,
        trouper_system: Some(harness.system().clone()),
        model_name_resolver: None,
    }
}

/// Seeds a bot with a parent; returns (state, bot_id, parent_id).
fn bot_fixture() -> (State, SessionId, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let parent = ChatSessionState::new();
    let parent_id = parent.session_id().clone();
    let bot = ChatSessionState::new_bot(&parent, true);
    let bot_id = bot.session_id().clone();
    {
        let mut guard = state.write();
        guard.session.insert(parent);
        guard.session.insert(bot);
    }
    (state, bot_id, parent_id)
}

#[rstest::rstest]
#[tokio::test]
async fn conclude_appends_to_the_callers_own_log() {
    // Given a bot session.
    let harness = TestHarness::new().await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id.clone(), &dir.path).await;

    // When the report tool runs.
    let result = conclude_execute(
        call("conclude", r#"{"body":"the build was green"}"#),
        tool_ctx,
    )
    .await;

    // Then the tool succeeds and the log holds the body.
    assert!(result.success, "report must succeed: {}", result.content);
    let guard = state.read();
    let session = guard.session.get(&bot_id).expect("bot");
    assert_eq!(session.bot_reports().len(), 1);
    assert_eq!(session.bot_reports()[0].body, "the build was green");
    assert_eq!(session.bot_reports()[0].run, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn report_does_not_wake_the_parent() {
    // Given a bot with a live parent, on a bus with a recorder for
    // the enqueue that would wake it.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the report tool runs.
    let result = conclude_execute(call("conclude", r#"{"body":"a finding"}"#), tool_ctx).await;

    // Then nothing is enqueued anywhere — report is terminal.
    assert!(result.success);
    // The recorder must have stopped changing before "nothing was
    // enqueued" is an observation rather than a race with a publish that
    // is still in flight.
    let enqueued = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        enqueued.is_empty(),
        "report must not wake any session, got: {enqueued:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn conclude_persists_the_session() {
    // Given a bot on a bus with a recorder for the persist command.
    let harness = TestHarness::new().await;
    let persists = harness
        .spawn_recorder::<jinn_session_store_msg::PersistSession>()
        .await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id.clone(), &dir.path).await;

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
    assert_eq!(persisted[0].session_id, bot_id);
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_enqueues_an_intersession_message_into_the_parent() {
    // Given a bot with a live parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, parent_id) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id.clone(), &dir.path).await;

    // When the notify tool runs.
    let result = notify_parent_execute(
        call("notify_parent", r#"{"message":"look at this"}"#),
        tool_ctx,
    )
    .await;

    // Then the parent receives an inter-session enqueue addressed
    // source=bot / target=parent, the same variant a tell_session
    // plain delivery produces.
    assert!(result.success, "notify must succeed: {}", result.content);
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].session_id, parent_id);
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        ..
    } = &wakes[0].entry.kind
    else {
        panic!("the wake must be an Intersession entry (tell_session variant)");
    };
    assert_eq!(source_session, &bot_id, "the bot is the source");
    assert_eq!(target_session, &parent_id, "the parent is addressed");
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_composes_the_shared_attribution_prefix() {
    // Given a bot with a live parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id.clone(), &dir.path).await;

    // When the notify tool runs.
    let result = notify_parent_execute(
        call("notify_parent", r#"{"message":"look at this"}"#),
        tool_ctx,
    )
    .await;

    // Then the delivered text is the attributed format, byte-exact — the
    // one composition site tell_session uses.
    assert!(result.success, "{}", result.content);
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &wakes[0].entry.kind
    else {
        panic!("the wake must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!("[Message from session {bot_id}]: look at this")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_wake_is_included_in_the_parents_llm_context() {
    // Given a bot with a live parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the notify tool runs.
    let result =
        notify_parent_execute(call("notify_parent", r#"{"message":"wake up"}"#), tool_ctx).await;

    // Then the delivered entry is in LLM context by default — the model
    // reads the notification like a user message.
    assert!(result.success, "{}", result.content);
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert!(wakes[0].entry.is_in_context());
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_leaves_an_untitled_parent_untitled() {
    // Given a bot whose parent has no title.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, parent_id) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the notify tool runs.
    let result = notify_parent_execute(
        call("notify_parent", r#"{"message":"hello parent"}"#),
        tool_ctx,
    )
    .await;

    // Then the notification succeeded and the parent still has no title —
    // title minting stays keyed to genuine user entries, and a stamped
    // empty string would block every future mint.
    assert!(result.success, "{}", result.content);
    let wakes = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(wakes.len(), 1);
    let guard = state.read();
    let parent = guard.session.get(&parent_id).expect("parent");
    assert_eq!(parent.title(), None);
}

#[rstest::rstest]
#[tokio::test]
async fn notify_parent_marks_the_parent_interacted() {
    // Given a bot with a ParentCompleted trigger and a fresh parent.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, bot_id, parent_id) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    {
        let mut guard = state.write();
        guard
            .session
            .get_mut(&bot_id)
            .expect("bot")
            .set_bot_trigger(BotTrigger::ParentCompleted);
    }
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

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
async fn notify_parent_fails_for_a_non_bot() {
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
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id, &dir.path).await;

    // When the notify tool runs.
    let result =
        notify_parent_execute(call("notify_parent", r#"{"message":"hi"}"#), tool_ctx).await;

    // Then the call fails and nothing is enqueued.
    assert!(!result.success);
    // The recorder must have stopped changing before "nothing was
    // enqueued" is an observation rather than a race with a publish that
    // is still in flight.
    let enqueued = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(enqueued.is_empty(), "a failed notify must enqueue nothing");
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
    // Given a bot and a call whose arguments lack `body`.
    let harness = TestHarness::new().await;
    let (state, bot_id, _parent) = bot_fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the report tool runs.
    let result = conclude_execute(call("conclude", r#"{}"#), tool_ctx).await;

    // Then the call fails with the missing slot named.
    assert!(!result.success);
    assert!(result.content.contains("body"));
}

#[rstest::rstest]
fn every_declared_parameter_is_one_the_executor_reads() {
    // Given the conclude tool's schema.

    // When its declared properties are read.
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

    // When the definition is read.
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
