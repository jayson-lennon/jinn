// Tool-level tests for the `task` tool's spawn and await behavior.
//
// These tests exercise `execute()` directly against a real bus (via
// `TestHarness`), a shared `State`, and a minted session cap — the same
// wiring the tool orchestrator provides in production. No session actor is
// spawned: the tests stand in for the child session's own actor by driving
// phase transitions and pushing entries, which is exactly what the actor
// does in production on stream events.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use std::time::Duration;

use crate::common::app_paths::AppPaths;
use crate::common::app_state::AppState;
use crate::common::bus::test_harness::{TestHarness, await_recorded};
use crate::common::state::State;
use crate::common::tcaps::mint::mint_session_cap;
use crate::feat::chat_input::protocol::command::EnqueueUserMessage;
use crate::feat::provider::protocol::command::CancelStream;
use crate::feat::session::chat_entry::{ChatEntry, ChatEntryKind};
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use crate::feat::session_lifecycle::protocol::event::SessionCreated;
use crate::feat::todo_list::{PhaseInput, TaskStatus};
use crate::feat::tools_actor::task::execute;
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolResult};
use crate::protocol::SessionId;
use std::collections::BTreeSet;

/// How long tests wait for the child run to reach its outcome.
const AWAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a tool call with the given JSON arguments.
fn task_call(arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_task_1".to_owned(),
        name: crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Builds a tool context wired to the harness bus, shared state, and a minted
/// session cap — the production wiring minus the MCP coordinator.
async fn task_ctx(harness: &TestHarness, state: &State, session_id: SessionId) -> ToolContext {
    let services = harness.services().await;
    ToolContext {
        cwd: std::path::PathBuf::from("/tmp"),
        command_policy: crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
        timeout: None,
        state: Some(state.clone()),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: Some(harness.bus()),
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: Some(mint_session_cap()),
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: Some(services.task_spawns.clone()),
        session_store: Some(services.session_store.clone()),
    }
}

/// Seeds a parent session with a distinctive model, cwd, persona, disabled
/// tool, and MCP servers; returns the state and the parent id.
fn parent_fixture() -> (State, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let parent_id = SessionId::new();
    state.write_test_no_cap().session.get_or_create(&parent_id);
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.set_model(
            crate::feat::session::model_selection::ModelSelection::Single(
                "test-provider/test-model".to_owned(),
            ),
        );
        parent.set_cwd(std::path::PathBuf::from("/tmp/parent-cwd"));
        parent.set_title("Parent".to_owned());
        parent
            .profile_mut()
            .disabled_tools
            .insert("write".to_owned());
        parent.set_enabled_mcp_servers(BTreeSet::from(["stub".to_owned()]));
    }
    (state, parent_id)
}

/// Drives the seeded child to a finished assistant turn: Sending → Streaming
/// → Idle with the given final text — the phase sequence a real child run
/// produces. Publishes the same `SessionPhaseChanged` events the session
/// actor emits, so a subscribed listener sees the run.
async fn finish_child_like_session_actor(
    bus: &crate::common::services::bus_service::BusService,
    state: &State,
    child_id: &SessionId,
    text: &str,
) {
    let old = {
        let mut w = state.write_test_no_cap();
        let child = w.session.get_mut(child_id).expect("child seeded");
        let old = child.phase();
        child.begin_sending();
        child.begin_streaming();
        child.push_entry(ChatEntry::assistant(text));
        child.finish_streaming(false, jiff::Timestamp::now());
        old
    };
    bus.publish(SessionPhaseChanged {
        session_id: child_id.clone(),
        old_phase: old,
        new_phase: PhaseKind::Idle,
    })
    .await;
}

/// Drives the seeded child to a cancelled end: Streaming → Idle with the
/// `Error("Cancelled")` entry the streaming handler pushes on cancel.
/// Publishes the force-published `Idle→Idle` event the cancel path emits.
async fn cancel_child_like_user(
    bus: &crate::common::services::bus_service::BusService,
    state: &State,
    child_id: &SessionId,
) {
    {
        let mut w = state.write_test_no_cap();
        let child = w.session.get_mut(child_id).expect("child seeded");
        child.begin_sending();
        child.begin_streaming();
        child.push_entry(ChatEntry::error("Cancelled"));
        child.finish_streaming(false, jiff::Timestamp::now());
    }
    bus.publish(SessionPhaseChanged {
        session_id: child_id.clone(),
        old_phase: PhaseKind::Idle,
        new_phase: PhaseKind::Idle,
    })
    .await;
}

/// The parent fixture's enabled MCP servers — the child inherits this set, so
/// it is also the settle gate's expectation set.
fn parent_servers() -> BTreeSet<String> {
    BTreeSet::from(["stub".to_owned()])
}

/// Extracts the (success, content) pair from a finished tool result.
fn result_parts(result: &ToolResult) -> (bool, &str) {
    (result.success, result.content.as_str())
}

/// Settles the child's discovery the way the scan actors and MCP actor do:
/// the three `*Loaded` events (clean) plus `Running` per server. A gate-reaching
/// test must call this after `SessionCreated` arrives, or the tool's settle
/// gate holds the enqueue for the full 15s budget.
async fn settle_child_discovery(
    bus: &crate::common::services::bus_service::BusService,
    child_id: &SessionId,
    servers: &BTreeSet<String>,
) {
    bus.publish(crate::feat::context::protocol::event::ContextFilesLoaded {
        session_id: child_id.clone(),
        files: vec![],
        error: None,
    })
    .await;
    bus.publish(crate::feat::skills::SkillsLoaded {
        session_id: child_id.clone(),
        skills: vec![],
        error: None,
    })
    .await;
    bus.publish(
        crate::feat::provider::protocol::event::PromptTemplatesLoaded {
            session_id: child_id.clone(),
            templates: vec![],
            error: None,
        },
    )
    .await;
    for server in servers {
        bus.publish(jinn_slices::McpServerStatus {
            session_id: child_id.clone(),
            server: server.clone(),
            status: jinn_slices::McpConnectionStatus::Running,
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// Spawn: linkage, inheritance, publication
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn task_spawns_child_linked_and_inheriting() {
    // Given a parent session with distinctive config and recorders on the bus.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a task call with a description.
    let call = task_call(r#"{"prompt": "Do the thing.", "description": "Do the thing"}"#);
    let pending = tokio::spawn(execute(call, ctx));

    // And the child finishing its run.
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "All done.").await;
    let result = pending.await.expect("task join");

    // Then the result is the child's final assistant text.
    assert!(result.success, "expected success; got: {}", result.content);
    assert_eq!(result.content, "All done.");

    // And the child is linked, titled, inheriting, persisted, and interacted.
    let snapshot = state.read();
    let child = snapshot
        .session
        .get(&child_id)
        .expect("child present in state");
    assert_eq!(child.parent_session().as_ref(), Some(&parent_id));
    assert_eq!(child.title(), Some("Do the thing"));
    assert_eq!(
        child.profile().model.to_string(),
        "test-provider/test-model"
    );
    assert_eq!(child.cwd(), std::path::Path::new("/tmp/parent-cwd"));
    assert_eq!(child.persona_name(), "coding-assistant");
    assert!(child.profile().disabled_tools.contains("write"));
    assert_eq!(
        child.enabled_mcp_servers(),
        &BTreeSet::from(["stub".to_owned()])
    );
    assert!(child.persist());
    assert!(child.has_interacted());
    // And no parent history leaked into the child: its history holds only
    // the simulated run's output.
    assert_eq!(child.history().len(), 1);
    assert!(matches!(
        &child.history()[0].kind,
        ChatEntryKind::Assistant(text) if text == "All done."
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn task_child_has_task_tool_suppressed() {
    // Given a parent session whose disabled set does not include the task
    // tool (it is enabled: this session could spawn).
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let snapshot = state.read();
        let parent = snapshot.session.get(&parent_id).expect("parent seeded");
        assert!(
            !parent
                .profile()
                .disabled_tools
                .contains(crate::feat::tools_actor::task::TASK_TOOL_NAME),
            "fixture parent must have task enabled"
        );
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's disabled tools include task — stamped at spawn.
    let snapshot = state.read();
    let child = snapshot
        .session
        .get(&child_id)
        .expect("child present in state");
    assert!(
        child
            .profile()
            .disabled_tools
            .contains(crate::feat::tools_actor::task::TASK_TOOL_NAME),
        "spawned child must start with the task tool suppressed, got: {:?}",
        child.profile().disabled_tools
    );
}

#[rstest::rstest]
#[tokio::test]
async fn task_child_inherits_parent_project() {
    // Given a parent session stamped with a project.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child inherits the parent's project association.
    let snapshot = state.read();
    let child = snapshot
        .session
        .get(&child_id)
        .expect("child present in state");
    assert_eq!(
        child.project(),
        Some(std::path::Path::new("/home/user/projects/jinn")),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn task_child_inherits_parent_task_list() {
    // Given a parent session with a seeded task list.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
        ]);
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's task list equals the parent's list at spawn time.
    let snapshot = state.read();
    let parent = snapshot.session.get(&parent_id).expect("parent present");
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(child.task_list(), parent.task_list());
}

#[rstest::rstest]
#[tokio::test]
async fn task_child_task_list_is_independent_after_spawn() {
    // Given a parent session with a seeded task list and a spawned child that
    // inherited it.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.task_list_mut().set_from_inputs(&[PhaseInput {
            description: "Research".to_owned(),
            tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
        }]);
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // When mutating the parent's list after spawn.
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent present");
        parent.task_list_mut().clear();
    }

    // Then the child's list still holds the spawn-time snapshot.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert!(!child.task_list().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn task_publishes_session_created_then_enqueue_user_message() {
    // Given a parent session and recorders on the bus.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let enqueue_rec = harness.spawn_recorder::<EnqueueUserMessage>().await;

    // When executing a task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &created[0].session_id, "Found it.")
        .await;
    let _ = pending.await;

    // Then both messages were published for the child.
    let enqueued = await_recorded(&enqueue_rec, 1, AWAIT_TIMEOUT).await;
    assert_eq!(created[0].session_id, enqueued[0].session_id);
    assert!(matches!(
        &enqueued[0].entry.kind,
        ChatEntryKind::User { display, .. } if display == "Explore."
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn task_stamps_spawned_child_session_on_parent_tool_call() {
    // Given a parent session whose history holds the tool call being executed.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.push_entry(ChatEntry::tool_call(
            "tc_task_1",
            crate::feat::tools_actor::task::TASK_TOOL_NAME,
            r#"{"prompt": "Explore."}"#,
        ));
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing the task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the parent's tool call entry carries the spawned child session.
    let snapshot = state.read();
    let parent = snapshot
        .session
        .get(&parent_id)
        .expect("parent present in state");
    let entry = parent
        .history()
        .iter()
        .find(
            |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc_task_1"),
        )
        .expect("stamped tool call entry");
    let ChatEntryKind::ToolCall { child_session, .. } = &entry.kind else {
        panic!("expected ToolCall kind");
    };
    assert_eq!(child_session, &Some(child_id));
}

#[rstest::rstest]
#[tokio::test]
async fn task_stamp_leaves_unrelated_tool_call_entries_untouched() {
    // Given a parent session holding the executed call and an unrelated call.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    {
        let mut w = state.write_test_no_cap();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.push_entry(ChatEntry::tool_call(
            "tc_task_1",
            crate::feat::tools_actor::task::TASK_TOOL_NAME,
            r#"{"prompt": "Explore."}"#,
        ));
        parent.push_entry(ChatEntry::tool_call(
            "tc_other",
            "read",
            r#"{"path": "a.rs"}"#,
        ));
    }
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing the task call and finishing the child.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &created[0].session_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the unrelated entry still has no child session link.
    let snapshot = state.read();
    let parent = snapshot
        .session
        .get(&parent_id)
        .expect("parent present in state");
    let entry = parent
        .history()
        .iter()
        .find(|entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc_other"))
        .expect("unrelated tool call entry");
    let ChatEntryKind::ToolCall { child_session, .. } = &entry.kind else {
        panic!("expected ToolCall kind");
    };
    assert_eq!(child_session, &None);
}

#[rstest::rstest]
#[tokio::test]
async fn task_rejects_an_empty_prompt_with_a_legible_result() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id).await;

    // When executing a task call with a blank prompt.
    let result = execute(task_call(r#"{"prompt": "   "}"#), ctx).await;

    // Then the tool fails without spawning any session.
    let (success, content) = result_parts(&result);
    assert!(!success);
    assert!(content.contains("prompt"), "got: {content}");
    assert_eq!(result.tool_call_id, "tc_task_1");
}

#[rstest::rstest]
#[tokio::test]
async fn task_title_falls_back_to_the_prompt_first_line() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a task call without a description.
    let pending = tokio::spawn(execute(
        task_call(r#"{"prompt": "Search the tree for the answer."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &created[0].session_id, "done").await;
    let _ = pending.await;

    // Then the child's title derives from the prompt's first line.
    let snapshot = state.read();
    let child = snapshot.session.get(&created[0].session_id).expect("child");
    assert_eq!(child.title(), Some("Search the tree for the answer."));
}

// ---------------------------------------------------------------------------
// Await + forward: completion, cancel, timeout, concurrency, abort
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn task_result_is_child_last_entry() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When the child finishes with an assistant entry.
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Summarize."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    finish_child_like_session_actor(
        &harness.bus(),
        &state,
        &created[0].session_id,
        "The summary.",
    )
    .await;
    let result = pending.await.expect("join");

    // Then the tool result is that entry's text, marked successful.
    assert!(result.success);
    assert_eq!(result.content, "The summary.");
}

#[rstest::rstest]
#[tokio::test]
async fn task_child_cancel_forwards_error_entry() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When the user cancels the child (Error("Cancelled") + Idle).
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Long research."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    cancel_child_like_user(&harness.bus(), &state, &created[0].session_id).await;
    let result = pending.await.expect("join");

    // Then the cancel is forwarded as a failure result.
    let (success, content) = result_parts(&result);
    assert!(!success, "cancel must surface as failure; got: {content}");
    assert_eq!(content, "Cancelled");
}

#[rstest::rstest]
#[tokio::test]
async fn task_timeout_cancels_child_and_fails() {
    // Given a parent session and a recorder for cancel commands.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let cancel_rec = harness.spawn_recorder::<CancelStream>().await;

    // When the child never finishes and the budget expires.
    let pending = tokio::spawn(execute(
        task_call(r#"{"prompt": "Never finishes.", "max_duration_secs": 1}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    // Settle first, then strand the child in Streaming — a hung provider.
    // This makes the deadline under test the phase deadline, not the settle
    // budget.
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    {
        let mut w = state.write_test_no_cap();
        let child = w.session.get_mut(&created[0].session_id).expect("child");
        child.begin_sending();
        child.begin_streaming();
    }
    let result = pending.await.expect("join");

    // Then the child was cancelled and the tool failed naming the budget.
    let cancels = await_recorded(&cancel_rec, 1, AWAIT_TIMEOUT).await;
    assert_eq!(cancels[0].session_id, created[0].session_id);
    let (success, content) = result_parts(&result);
    assert!(!success);
    assert!(
        content.contains("timed out") && content.contains("max_duration_secs"),
        "got: {content}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn concurrent_task_calls_resolve_independently() {
    // Given a parent session and two in-flight task calls.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx_a = task_ctx(&harness, &state, parent_id.clone()).await;
    let ctx_b = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    let pending_a = tokio::spawn(execute(task_call(r#"{"prompt": "Task A."}"#), ctx_a));
    let pending_b = tokio::spawn(execute(task_call(r#"{"prompt": "Task B."}"#), ctx_b));
    let created = await_recorded(&created_rec, 2, AWAIT_TIMEOUT).await;
    assert_ne!(created[0].session_id, created[1].session_id);

    // When both children finish.
    for (i, text) in ["A done.", "B done."].iter().enumerate() {
        settle_child_discovery(&harness.bus(), &created[i].session_id, &parent_servers()).await;
        finish_child_like_session_actor(&harness.bus(), &state, &created[i].session_id, text).await;
    }
    let result_a = pending_a.await.expect("join a");
    let result_b = pending_b.await.expect("join b");

    // Then each result carries its own child's answer.
    assert_eq!(result_a.content, "A done.");
    assert_eq!(result_b.content, "B done.");
}

#[rstest::rstest]
#[tokio::test]
async fn parent_cancel_leaves_child_running_and_unregisters_pair() {
    // Given an in-flight task call awaiting its child.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    // The registry inside the ctx is the one to observe — `task_ctx` clones
    // it out of the harness services, so they share the Arc'd inner map.
    let registry = ctx.task_spawns.clone().expect("ctx carries the registry");
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Still going."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    // Settle, then drive the child into Streaming so it is mid-run when the
    // parent goes.
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    {
        let mut w = state.write_test_no_cap();
        let child = w.session.get_mut(&child_id).expect("child");
        child.begin_sending();
        child.begin_streaming();
    }
    assert!(registry.has_in_flight(&parent_id));

    // When the parent's tool-call future is aborted (batch cancelled).
    pending.abort();
    // Give the aborted future a moment to drop its registry guard.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Then the pair is unregistered and the child keeps running untouched.
    assert!(
        !registry.has_in_flight(&parent_id),
        "abort must unregister the in-flight pair"
    );
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child survives");
    assert_eq!(child.phase(), PhaseKind::Streaming);
}

// ---------------------------------------------------------------------------
// Failure context: missing parent session
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn task_fails_when_parent_session_is_missing() {
    // Given a context naming a session that was never seeded.
    let harness = TestHarness::new().await;
    let state = State::new(AppState::default_with_scope_focus());
    let ctx = task_ctx(&harness, &state, SessionId::new()).await;

    // When executing a task call.
    let result = execute(task_call(r#"{"prompt": "Ghost."}"#), ctx).await;

    // Then the tool fails naming the missing parent.
    let (success, content) = result_parts(&result);
    assert!(!success);
    assert!(content.contains("parent"), "got: {content}");
}

// ---------------------------------------------------------------------------
// Phase listener: Idle→Idle force-publish (cancel path) is not missed
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn listener_signals_on_idle_to_idle_transition() {
    // Given a listener subscribed for a child and a completion channel.
    let harness = TestHarness::new().await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let child_id = SessionId::new();
    let _listener = harness
        .spawn_actor::<crate::feat::tools_actor::task_phase_listener_actor::TaskPhaseListenerActor>(
            crate::feat::tools_actor::task_phase_listener_actor::TaskPhaseListenerDeps {
                bus: harness.bus(),
                child_id: child_id.clone(),
                completion: tx,
            },
        )
        .await;

    // When a force-published Idle→Idle transition arrives (the cancel path).
    harness
        .publish(SessionPhaseChanged {
            session_id: child_id.clone(),
            old_phase: PhaseKind::Idle,
            new_phase: PhaseKind::Idle,
        })
        .await;

    // Then the completion signal fires despite old == new.
    tokio::time::timeout(AWAIT_TIMEOUT, rx)
        .await
        .expect("listener must signal on Idle→Idle")
        .expect("sender alive");
    // The actor stopped itself after signaling; nothing to tear down here.
}

// ---------------------------------------------------------------------------
// Settle gate: the first dispatch waits for discovery events
// ---------------------------------------------------------------------------

/// Spawns `await_discovery_settlement` on the harness bus with a `Duration`
/// budget, sleeping a beat first so the listener's subscriptions register
/// before the test publishes events. Returns the join handle.
async fn spawned_settle_wait(
    harness: &TestHarness,
    child_id: &SessionId,
    servers: &BTreeSet<String>,
    budget: Duration,
) -> tokio::task::JoinHandle<()> {
    let bus = harness.bus();
    let child_id = child_id.clone();
    let servers = servers.clone();
    let wait = tokio::spawn(async move {
        crate::feat::tools_actor::task::await_discovery_settlement(
            &bus, &child_id, &servers, budget,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    wait
}

#[rstest::rstest]
#[tokio::test]
async fn task_gates_first_dispatch_on_settlement() {
    // Given a parent session with an enabled MCP server, an enqueue recorder,
    // and an in-flight task call whose SessionCreated has arrived.
    let harness = TestHarness::new().await;
    let (state, parent_id) = parent_fixture();
    let ctx = task_ctx(&harness, &state, parent_id.clone()).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let enqueue_rec = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let pending = tokio::spawn(execute(task_call(r#"{"prompt": "Explore."}"#), ctx));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();

    // When only the three scan events arrive (the MCP leg is still pending).
    let no_servers = BTreeSet::new();
    settle_child_discovery(&harness.bus(), &child_id, &no_servers).await;

    // Then the enqueue has not been published: the gate holds for "stub".
    let held = await_recorded(&enqueue_rec, 0, Duration::from_millis(300)).await;
    assert!(
        held.is_empty(),
        "enqueue must not precede MCP settlement; got {}",
        held.len()
    );

    // When the server reaches its terminal status and the child finishes.
    harness
        .publish(jinn_slices::McpServerStatus {
            session_id: child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_slices::McpConnectionStatus::Running,
        })
        .await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Found it.").await;
    let result = pending.await.expect("task join");

    // Then the enqueue was published for the child and the result forwarded.
    let enqueued = await_recorded(&enqueue_rec, 1, AWAIT_TIMEOUT).await;
    assert_eq!(enqueued[0].session_id, child_id);
    assert!(result.success, "got: {}", result.content);
}

#[rstest::rstest]
#[tokio::test]
async fn settle_waiter_completes_on_quorum() {
    // Given a settle wait for a child with one expected server.
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let wait = spawned_settle_wait(&harness, &child_id, &servers, AWAIT_TIMEOUT).await;

    // When all discovery events arrive for the child.
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;

    // Then the wait returns promptly — quorum, not budget.
    tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("settle wait must complete on quorum, not at budget")
        .expect("join");
}

#[rstest::rstest]
#[tokio::test]
async fn settle_waiter_counts_error_events_as_settled() {
    // Given a settle wait for a child with no MCP expectations.
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let no_servers = BTreeSet::new();
    let wait = spawned_settle_wait(&harness, &child_id, &no_servers, AWAIT_TIMEOUT).await;

    // When the three scan events arrive carrying errors (failed scans).
    harness
        .publish(crate::feat::context::protocol::event::ContextFilesLoaded {
            session_id: child_id.clone(),
            files: vec![],
            error: Some("scan failed".to_owned()),
        })
        .await;
    harness
        .publish(crate::feat::skills::SkillsLoaded {
            session_id: child_id.clone(),
            skills: vec![],
            error: Some("scan failed".to_owned()),
        })
        .await;
    harness
        .publish(
            crate::feat::provider::protocol::event::PromptTemplatesLoaded {
                session_id: child_id.clone(),
                templates: vec![],
                error: Some("scan failed".to_owned()),
            },
        )
        .await;

    // Then the gate opens: a resolved scan settles, error or not.
    tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("error-path scan events must settle the gate")
        .expect("join");
}

#[rstest::rstest]
#[tokio::test]
async fn settle_waiter_dead_mcp_status_settles() {
    // Given a settle wait expecting one server.
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let wait = spawned_settle_wait(&harness, &child_id, &servers, AWAIT_TIMEOUT).await;

    // When the scan events arrive and the server reports Dead (never came up).
    let no_servers = BTreeSet::new();
    settle_child_discovery(&harness.bus(), &child_id, &no_servers).await;
    harness
        .publish(jinn_slices::McpServerStatus {
            session_id: child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_slices::McpConnectionStatus::Dead,
        })
        .await;

    // Then the gate opens: Dead is terminal.
    tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("Dead must settle the gate")
        .expect("join");
}

#[rstest::rstest]
#[tokio::test]
async fn settle_waiter_ignores_starting_status() {
    // Given a settle wait expecting one server with a tiny budget.
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let tiny = Duration::from_millis(400);
    // The budget starts at spawn; elapsed measures from there so a
    // premature return (Starting settling the gate at ~50ms) is detectable.
    let started = tokio::time::Instant::now();
    let wait = spawned_settle_wait(&harness, &child_id, &servers, tiny).await;

    // When the scan events arrive but the server only ever reports Starting.
    let no_servers = BTreeSet::new();
    settle_child_discovery(&harness.bus(), &child_id, &no_servers).await;
    harness
        .publish(jinn_slices::McpServerStatus {
            session_id: child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_slices::McpConnectionStatus::Starting,
        })
        .await;

    // Then the wait ends at the budget, not on the Starting status.
    tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("join")
        .expect("join");
    assert!(
        started.elapsed() >= tiny,
        "Starting must not settle the gate; returned after only {:?}",
        started.elapsed()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn settle_waiter_budget_expiry_proceeds() {
    // Given a settle wait whose expected server never reports in.
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let tiny = Duration::from_millis(300);
    let started = tokio::time::Instant::now();

    // When the budget expires with no events at all.
    crate::feat::tools_actor::task::await_discovery_settlement(
        &harness.bus(),
        &child_id,
        &servers,
        tiny,
    )
    .await;
    let elapsed = started.elapsed();

    // Then the call proceeded at the budget without error.
    assert!(
        elapsed >= tiny,
        "must wait out the budget; elapsed {elapsed:?}"
    );
    assert!(
        elapsed < AWAIT_TIMEOUT,
        "must not wait past the budget; elapsed {elapsed:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn listener_stops_on_channel_close() {
    // Given a settle listener whose receiver was already consumed (the
    // awaiting task future aborted).
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel::<()>();
    drop(settled_rx);
    let listener = harness
        .spawn_actor::<crate::feat::tools_actor::task_settle_listener_actor::TaskSettleListenerActor>(
            crate::feat::tools_actor::task_settle_listener_actor::TaskSettleListenerDeps {
                bus: harness.bus(),
                child_id: child_id.clone(),
                expected_servers: servers,
                settled: settled_tx,
            },
        )
        .await;

    // When a matching discovery event arrives (the abort check fires).
    harness
        .publish(crate::feat::skills::SkillsLoaded {
            session_id: child_id.clone(),
            skills: vec![],
            error: None,
        })
        .await;

    // Then the listener stops instead of signalling a dead channel.
    tokio::time::timeout(Duration::from_secs(2), listener.wait_for_shutdown())
        .await
        .expect("listener must notice the closed channel and stop");
}
