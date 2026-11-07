// Tool-level tests for the `spawn_subagent` tool's spawn and await behavior.
//
// These tests exercise `execute()` directly against a real bus (via
// `TestHarness`) and the same shared `State` wiring the tool orchestrator
// provides in production. No session actor is spawned: the tests stand in for
// the child session's own actor by driving
// phase transitions and pushing entries, which is exactly what the actor
// does in production on stream events.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use marked_path::{Absolute, MarkedPathBuf};
use std::time::Duration;

use crate::spawn_subagent::execute;
use crate::tool_types::ToolContext;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::SessionId;
use jinn_core_types::model_selection::{AlloyStrategy, ModelSelection};
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::app_paths::AppPaths;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::provider_resolution::registry_model_name_resolver;
use jinn_kernel::common::state::State;
use jinn_kernel::protocol::{ChatEntry, ChatEntryKind};
use jinn_kernel::{ProviderRegistry, ProvidersConfig};
use jinn_session_init_msg::{ContextFilesLoaded, PromptTemplatesLoaded};
use jinn_session_lifecycle_msg::SessionCreated;
use jinn_session_msg::SessionPhaseChanged;
use jinn_slices::PhaseView;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_tools_msg::{PhaseInput, TaskStatus};
use std::collections::BTreeSet;

/// How long tests wait for the child run to reach its outcome.
const AWAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a tool call with the given JSON arguments.
fn spawn_subagent_call(arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_task_1".into(),
        name: jinn_tools_msg::SPAWN_SUBAGENT_TOOL_NAME.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Builds a tool context wired to the harness bus, shared state, and runtime
/// services — the production wiring minus the MCP coordinator — rooted at
/// `paths_root`, a directory the calling test owns for its whole run. With
/// `seed_registry`, also seeds the provider registry and wires the
/// registry-backed model-name resolver, the production wiring for the
/// `model` argument.
async fn spawn_subagent_ctx_with(
    harness: &TestHarness,
    state: &State,
    session_id: SessionId,
    paths_root: &MarkedPathBuf<Absolute>,
    seed_registry: bool,
) -> ToolContext {
    let services = harness.services().await;
    if seed_registry {
        seed_provider_registry(&services);
    }
    let model_name_resolver =
        seed_registry.then(|| registry_model_name_resolver(&services.provider_registry));
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
        task_spawns: Some(services.task_spawns.clone()),
        session_store: Some(services.session_store.clone()),
        trouper_system: Some(harness.system().clone()),
        model_name_resolver,
    }
}

/// [`spawn_subagent_ctx_with`] without the model-name resolver — the
/// bare-fixture wiring, rooted at `paths_root`.
async fn spawn_subagent_ctx(
    harness: &TestHarness,
    state: &State,
    session_id: SessionId,
    paths_root: &MarkedPathBuf<Absolute>,
) -> ToolContext {
    spawn_subagent_ctx_with(harness, state, session_id, paths_root, false).await
}

/// [`spawn_subagent_ctx`] carrying the registry-backed resolver over a seeded
/// provider registry — the production wiring for the `model` argument.
async fn spawn_subagent_ctx_with_resolver(
    harness: &TestHarness,
    state: &State,
    session_id: SessionId,
    paths_root: &MarkedPathBuf<Absolute>,
) -> ToolContext {
    spawn_subagent_ctx_with(harness, state, session_id, paths_root, true).await
}

/// The parent fixture's distinctive cwd and project paths, derived from
/// `root` — a directory the calling test owns.
fn parent_paths(
    root: &MarkedPathBuf<Absolute>,
) -> (MarkedPathBuf<Absolute>, MarkedPathBuf<Absolute>) {
    (
        root.join("parent-cwd")
            .expect("temp root joins a bare name"),
        root.join_all(["home", "user", "projects", "jinn"])
            .expect("temp root joins a bare name"),
    )
}

/// Seeds a parent session with a distinctive model, cwd, persona, disabled
/// tool, and MCP servers — the cwd and project paths derived from `root`, a
/// directory the calling test owns; returns the state and the parent id.
fn parent_fixture(root: &MarkedPathBuf<Absolute>) -> (State, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let parent_id = SessionId::new();
    state.write().session.get_or_create(&parent_id);
    let (cwd, _project) = parent_paths(root);
    {
        let mut w = state.write();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.set_model(jinn_core_types::model_selection::ModelSelection::Single(
            "test-provider/test-model".to_owned(),
        ));
        parent.set_cwd(cwd);
        parent.set_title("Parent".to_owned());
        parent.profile_mut().tool_filter =
            Some(jinn_core_types::NameFilter::deny(["write".to_owned()]));
        parent.set_enabled_mcp_servers(BTreeSet::from(["stub".to_owned()]));
    }
    (state, parent_id)
}

/// Drives the seeded child to a finished assistant turn: Sending → Streaming
/// → Idle with the given final text — the phase sequence a real child run
/// produces. Publishes the same `SessionPhaseChanged` events the session
/// actor emits, so a subscribed listener sees the run.
async fn finish_child_like_session_actor(
    bus: &jinn_kernel::common::services::bus_service::BusService,
    state: &State,
    child_id: &SessionId,
    text: &str,
) {
    let old = {
        let mut w = state.write();
        let child = w.session.get_mut(child_id).expect("child seeded");
        let old = child.phase();
        child.begin_sending();
        child.begin_streaming();
        child.push_entry(ChatEntry::assistant(text));
        // The old finish is now the two actor halves: the session actor's
        // entry fold, then the phase actor's machine edge.
        child.finalize_entries_for_finish(false, jiff::Timestamp::now());
        child.settle_to_idle();
        old
    };
    bus.publish(SessionPhaseChanged {
        session_id: child_id.clone(),
        old_phase: old,
        new_phase: PhaseView::Idle,
        at: jiff::Timestamp::now(),
    })
    .await;
}

/// Drives the seeded child to a cancelled end: Streaming → Idle with the
/// `Error("Cancelled")` entry the streaming handler pushes on cancel.
/// Publishes the force-published `Idle→Idle` event the cancel path emits.
async fn cancel_child_like_user(
    bus: &jinn_kernel::common::services::bus_service::BusService,
    state: &State,
    child_id: &SessionId,
) {
    {
        let mut w = state.write();
        let child = w.session.get_mut(child_id).expect("child seeded");
        child.begin_sending();
        child.begin_streaming();
        // The old cancel is now the production sequence: the session actor's
        // entry fold, then the `Error("Cancelled")` entry the streaming
        // handler pushes, then the phase actor's machine edge. Finalization
        // must precede the error entry so it lands last in history.
        child.finalize_entries_for_cancel(jiff::Timestamp::now());
        child.push_entry(ChatEntry::error("Cancelled"));
        child.settle_to_idle();
    }
    bus.publish(SessionPhaseChanged {
        session_id: child_id.clone(),
        old_phase: PhaseView::Idle,
        new_phase: PhaseView::Idle,
        at: jiff::Timestamp::now(),
    })
    .await;
}

/// The parent fixture's enabled MCP servers — the child inherits this set, so
/// it is also the settle gate's expectation set.
fn parent_servers() -> BTreeSet<String> {
    BTreeSet::from(["stub".to_owned()])
}

/// A providers config naming two providers (`ollama`, `openrouter`) and one
/// alias (`fast` → `ollama/llama3`), written as the TOML a user would write —
/// `ProvidersConfig` deserializes it and `ProviderRegistry::from_config`
/// validates it, alias target included.
fn two_providers_and_alias_toml() -> String {
    r#"
[providers.ollama]
backend = "ollama"
models = ["llama3"]
base_url = "http://localhost:11434"
requires_key = false

[providers.openrouter]
backend = "openrouter"
models = ["anthropic/claude-sonnet-4"]
requires_key = true

[[aliases]]
name = "fast"
target = "ollama/llama3"
"#
    .to_owned()
}

/// Parses the fixture TOML into a validated registry.
fn registry_from_toml(toml_text: &str) -> ProviderRegistry {
    let config: ProvidersConfig =
        toml::from_str(toml_text).expect("fixture TOML parses into ProvidersConfig");
    ProviderRegistry::from_config(config).expect("fixture config validates")
}

/// Swaps the fixture registry into the harness services' provider registry.
fn seed_provider_registry(services: &jinn_kernel::common::services::Services) {
    services
        .provider_registry
        .replace(registry_from_toml(&two_providers_and_alias_toml()));
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
    bus: &jinn_kernel::common::services::bus_service::BusService,
    child_id: &SessionId,
    servers: &BTreeSet<String>,
) {
    bus.publish(ContextFilesLoaded {
        session_id: child_id.clone(),
        files: vec![],
        error: None,
    })
    .await;
    bus.publish(jinn_skills_msg::SkillsLoaded {
        session_id: child_id.clone(),
        skills: vec![],
        error: None,
    })
    .await;
    bus.publish(PromptTemplatesLoaded {
        session_id: child_id.clone(),
        templates: vec![],
        error: None,
    })
    .await;
    for server in servers {
        bus.publish(jinn_mcp_msg::McpServerStatus {
            session_id: child_id.clone(),
            server: server.clone(),
            status: jinn_mcp_msg::McpConnectionStatus::Running,
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// Spawn: linkage, inheritance, publication
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_spawns_child_linked_and_inheriting() {
    // Given a parent session with distinctive config and recorders on the bus.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let expected_cwd = parent_paths(&dir.path).0;
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call with a description.
    let call = spawn_subagent_call(r#"{"prompt": "Do the thing.", "description": "Do the thing"}"#);
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
    assert_eq!(child.cwd(), &expected_cwd);
    assert_eq!(child.persona_name(), "coding-assistant");
    assert!(!child.is_tool_enabled("write"));
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
async fn spawn_subagent_child_has_spawn_subagent_tool_suppressed() {
    // Given a parent session whose disabled set does not include the spawn_subagent
    // tool (it is enabled: this session could spawn).
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    {
        let snapshot = state.read();
        let parent = snapshot.session.get(&parent_id).expect("parent seeded");
        assert!(
            parent.is_tool_enabled(jinn_tools_msg::SPAWN_SUBAGENT_TOOL_NAME),
            "fixture parent must have spawn_subagent enabled"
        );
    }
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's tool filter withholds spawn_subagent — stamped at spawn.
    let snapshot = state.read();
    let child = snapshot
        .session
        .get(&child_id)
        .expect("child present in state");
    assert!(
        !child.is_tool_enabled(jinn_tools_msg::SPAWN_SUBAGENT_TOOL_NAME),
        "spawned child must start with the spawn_subagent tool suppressed, got: {:?}",
        child.tool_filter()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_child_inherits_parent_project() {
    // Given a parent session stamped with a project.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let expected_project = parent_paths(&dir.path).1;
    let (state, parent_id) = parent_fixture(&dir.path);
    {
        let mut w = state.write();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.set_project(Some(expected_project.clone()));
    }
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
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
    assert_eq!(child.project(), Some(&expected_project));
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_child_task_list_starts_empty() {
    // Given a parent session with a seeded task list.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    {
        let mut w = state.write();
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
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's task list is empty — the parent's plan does not leak.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert!(child.task_list().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_child_task_list_does_not_propagate_to_parent() {
    // Given a parent session and a spawned child.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    let servers = parent_servers();
    settle_child_discovery(&harness.bus(), &child_id, &servers).await;

    // When the child records its own todo work.
    {
        let mut w = state.write();
        let child = w.session.get_mut(&child_id).expect("child present");
        child.task_list_mut().set_from_inputs(&[PhaseInput {
            description: "Subagent work".to_owned(),
            tasks: vec![("Do the thing".to_owned(), TaskStatus::Pending)],
        }]);
    }
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the parent's list is untouched.
    let snapshot = state.read();
    let parent = snapshot.session.get(&parent_id).expect("parent present");
    assert!(parent.task_list().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_publishes_session_created_then_enqueue_user_message() {
    // Given a parent session and recorders on the bus.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let enqueue_rec = harness.spawn_recorder::<EnqueueUserMessage>().await;

    // When executing a spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
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
async fn spawn_subagent_stamps_spawned_child_session_on_parent_tool_call() {
    // Given a parent session whose history holds the tool call being executed.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    {
        let mut w = state.write();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.push_entry(ChatEntry::tool_call(
            "tc_task_1",
            jinn_tools_msg::SPAWN_SUBAGENT_TOOL_NAME,
            r#"{"prompt": "Explore."}"#,
        ));
    }
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing the spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
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
            |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc_task_1"),
        )
        .expect("stamped tool call entry");
    let ChatEntryKind::ToolCall { child_session, .. } = &entry.kind else {
        panic!("expected ToolCall kind");
    };
    assert_eq!(child_session, &Some(child_id));
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_stamp_leaves_unrelated_tool_call_entries_untouched() {
    // Given a parent session holding the executed call and an unrelated call.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    {
        let mut w = state.write();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.push_entry(ChatEntry::tool_call(
            "tc_task_1",
            jinn_tools_msg::SPAWN_SUBAGENT_TOOL_NAME,
            r#"{"prompt": "Explore."}"#,
        ));
        parent.push_entry(ChatEntry::tool_call(
            "tc_other",
            "read",
            r#"{"path": "a.rs"}"#,
        ));
    }
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing the spawn_subagent call and finishing the child.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
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
        .find(|entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc_other"))
        .expect("unrelated tool call entry");
    let ChatEntryKind::ToolCall { child_session, .. } = &entry.kind else {
        panic!("expected ToolCall kind");
    };
    assert_eq!(child_session, &None);
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_rejects_an_empty_prompt_with_a_legible_result() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id, &dir.path).await;

    // When executing a spawn_subagent call with a blank prompt.
    let result = execute(spawn_subagent_call(r#"{"prompt": "   "}"#), ctx).await;

    // Then the tool fails without spawning any session.
    let (success, content) = result_parts(&result);
    assert!(!success);
    assert!(content.contains("prompt"), "got: {content}");
    assert_eq!(result.tool_call_id.as_str(), "tc_task_1");
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_title_falls_back_to_the_prompt_first_line() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call without a description.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Search the tree for the answer."}"#),
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
// Model argument: validation, alias resolution, inheritance
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_model_unknown_name_fails_without_spawning() {
    // Given a parent session with the production resolver wiring and
    // recorders on the bus.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let baseline_sessions = state.read().session.session_count();
    let ctx = spawn_subagent_ctx_with_resolver(&harness, &state, parent_id, &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let enqueue_rec = harness.spawn_recorder::<EnqueueUserMessage>().await;

    // When executing a spawn_subagent call whose model names neither an expanded id
    // nor an alias.
    let result = execute(
        spawn_subagent_call(r#"{"prompt": "Ghost run.", "model": "no-such-model"}"#),
        ctx,
    )
    .await;

    // Then the call fails, naming the offending value and the accepted forms.
    let (success, content) = result_parts(&result);
    assert!(!success);
    assert!(content.contains("no-such-model"), "got: {content}");
    assert!(content.contains("alias"), "got: {content}");

    // And nothing was spawned: no session inserted, no events published.
    assert_eq!(
        state.read().session.session_count(),
        baseline_sessions,
        "a rejected model must not add a session"
    );
    let created = await_recorded(&created_rec, 0, Duration::from_millis(300)).await;
    assert!(created.is_empty(), "SessionCreated must not publish");
    let enqueued = await_recorded(&enqueue_rec, 0, Duration::from_millis(300)).await;
    assert!(enqueued.is_empty(), "EnqueueUserMessage must not publish");
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_model_alias_spawns_child_on_alias_target() {
    // Given a parent session with the production resolver wiring over a
    // registry that configures `fast` → `ollama/llama3`.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx =
        spawn_subagent_ctx_with_resolver(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call with the alias name.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Run fast.", "model": "fast"}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Fast done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's model is the alias's target expanded id.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(
        child.profile().model,
        ModelSelection::Single("ollama/llama3".to_owned()),
        "alias must resolve to its target on the child"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_model_expanded_id_spawns_child_on_that_id() {
    // Given a parent session with the production resolver wiring.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx =
        spawn_subagent_ctx_with_resolver(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call with an expanded provider/model id.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(
            r#"{"prompt": "Run big.", "model": "openrouter/anthropic/claude-sonnet-4"}"#,
        ),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Big done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child's model is exactly that id.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(
        child.profile().model,
        ModelSelection::Single("openrouter/anthropic/claude-sonnet-4".to_owned()),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_without_model_inherits_parent_whole_model_selection() {
    // Given a parent session running an alloy.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let alloy = ModelSelection::Alloy {
        models: vec!["a/one".to_owned(), "b/two".to_owned()],
        strategy: AlloyStrategy::RoundRobin { index: 0 },
    };
    {
        let mut w = state.write();
        let parent = w.session.get_mut(&parent_id).expect("parent seeded");
        parent.set_model(alloy.clone());
    }
    let ctx =
        spawn_subagent_ctx_with_resolver(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call with no model argument.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the child carries the parent's whole selection, alloy included.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(
        child.profile().model,
        alloy,
        "inheritance must preserve the alloy, not flatten it"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_blank_model_argument_inherits_parent_model() {
    // Given a parent session on a single model with the resolver wired.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx =
        spawn_subagent_ctx_with_resolver(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call whose model argument is blank.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore.", "model": "   "}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the blank argument is treated as absent: the parent's model.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(
        child.profile().model,
        ModelSelection::Single("test-provider/test-model".to_owned()),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_without_resolver_takes_explicit_model_name_as_final() {
    // Given a bare test context carrying no resolver — the fixture wiring,
    // which production never produces.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When executing a spawn_subagent call with an explicit model argument.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Unverified.", "model": "some/model"}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    finish_child_like_session_actor(&harness.bus(), &state, &child_id, "Done.").await;
    let result = pending.await.expect("task join");
    assert!(result.success, "expected success; got: {}", result.content);

    // Then the name is taken as final: no registry was wired to answer, so
    // validation was skipped and the child carries the name verbatim.
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child present");
    assert_eq!(
        child.profile().model,
        ModelSelection::Single("some/model".to_owned()),
    );
}

// ---------------------------------------------------------------------------
// Await + forward: completion, cancel, timeout, concurrency, abort
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_result_is_child_last_entry() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When the child finishes with an assistant entry.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Summarize."}"#),
        ctx,
    ));
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
async fn spawn_subagent_child_cancel_forwards_error_entry() {
    // Given a parent session.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    // When the user cancels the child (Error("Cancelled") + Idle).
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Long research."}"#),
        ctx,
    ));
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
async fn spawn_subagent_timeout_cancels_child_and_fails() {
    // Given a parent session and a recorder for cancel commands.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let cancel_rec = harness.spawn_recorder::<CancelTurn>().await;

    // When the child never finishes and the budget expires.
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Never finishes.", "max_duration_secs": 1}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    // Settle first, then strand the child in Streaming — a hung provider.
    // This makes the deadline under test the phase deadline, not the settle
    // budget.
    settle_child_discovery(&harness.bus(), &created[0].session_id, &parent_servers()).await;
    {
        let mut w = state.write();
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
async fn concurrent_spawn_subagent_calls_resolve_independently() {
    // Given a parent session and two in-flight spawn_subagent calls.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx_a = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let ctx_b = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;

    let pending_a = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Task A."}"#),
        ctx_a,
    ));
    let pending_b = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Task B."}"#),
        ctx_b,
    ));
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
    // Given an in-flight spawn_subagent call awaiting its child.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    // The registry inside the ctx is the one to observe — `spawn_subagent_ctx` clones
    // it out of the harness services, so they share the Arc'd inner map.
    let registry = ctx.task_spawns.clone().expect("ctx carries the registry");
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Still going."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    // Settle, then drive the child into Streaming so it is mid-run when the
    // parent goes.
    settle_child_discovery(&harness.bus(), &child_id, &parent_servers()).await;
    {
        let mut w = state.write();
        let child = w.session.get_mut(&child_id).expect("child");
        child.begin_sending();
        child.begin_streaming();
    }
    assert!(registry.has_in_flight(&parent_id));

    // When the parent's tool-call future is aborted (batch cancelled).
    //
    // RATIONALE: this exercises the registry layer only. The *cascade* is
    // the confirmed-cancel gesture (ESC ESC, `try_handle_cancel_stream_prompt`),
    // which walks `children_of` and stops every descendant; a batch abort
    // here is the dispatcher's own cleanup, not a user cancel, so the child
    // legitimately keeps running — its result still answers the question the
    // parent's *next* turn will ask.
    pending.abort();
    // The aborted future must have dropped its registry guard. The guard
    // is released when the future is dropped, which happens on the next
    // scheduler pass — a fixed window could pass before it was released.
    jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(5), || async {
        !registry.has_in_flight(&parent_id)
    })
    .await;

    // Then the pair is unregistered and the child keeps running untouched.
    assert!(
        !registry.has_in_flight(&parent_id),
        "abort must unregister the in-flight pair"
    );
    // And `children_of` reads empty: presence *means* in-flight, so the
    // cascade's live query can never stop a child whose call already ended.
    assert!(
        registry.children_of(&parent_id).is_empty(),
        "children_of must read empty once the guard drops"
    );
    let snapshot = state.read();
    let child = snapshot.session.get(&child_id).expect("child survives");
    assert_eq!(child.phase(), PhaseView::Streaming);
}

// ---------------------------------------------------------------------------
// Failure context: missing parent session
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_fails_when_parent_session_is_missing() {
    // Given a context naming a session that was never seeded.
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let state = State::new(AppState::default_with_scope_focus());
    let ctx = spawn_subagent_ctx(&harness, &state, SessionId::new(), &dir.path).await;

    // When executing a spawn_subagent call.
    let result = execute(spawn_subagent_call(r#"{"prompt": "Ghost."}"#), ctx).await;

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
    let _listener = crate::subagent_phase_listener_actor::SubagentPhaseListenerActor::spawn(
        crate::subagent_phase_listener_actor::SubagentPhaseListenerDeps {
            system: harness.system().clone(),
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
            old_phase: PhaseView::Idle,
            new_phase: PhaseView::Idle,
            at: jiff::Timestamp::now(),
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

/// Spawns the settle listener on the harness bus with a `Duration` budget,
/// and waits until its subscriptions are live before returning.
///
/// The listener's `spawn` returns only after its four subscriptions are
/// registered, so awaiting that point — rather than sleeping a fixed beat
/// — is what makes the test's later publish guaranteed to reach the
/// listener. Returns the join handle for the settlement wait.
async fn spawned_settle_wait(
    harness: &TestHarness,
    child_id: &SessionId,
    servers: &BTreeSet<String>,
    budget: Duration,
) -> tokio::task::JoinHandle<()> {
    let system = harness.system().clone();
    let bus = harness.bus();
    let child_id = child_id.clone();
    let servers = servers.clone();
    let (subscribed, settled) = crate::spawn_subagent::spawn_discovery_settle_listener(
        &system, &bus, &child_id, &servers, budget,
    )
    .await;
    // The listener is subscribed by now; the test may publish.
    subscribed.await;
    tokio::spawn(settled)
}

/// An in-flight `spawn_subagent` call whose child session has been created, with
/// recorders for `SessionCreated` and `EnqueueUserMessage`. Holds the temp dir
/// the fixture's paths derive from, so the tree outlives the in-flight call.
struct InFlightSubagentCall {
    harness: TestHarness,
    _dir: jinn_testutil::JinnTempDir,
    state: State,
    child_id: SessionId,
    enqueue_rec: jinn_testutil::bus_harness::Recorder<EnqueueUserMessage>,
    pending: tokio::task::JoinHandle<jinn_core_types::tool_types::ToolResult>,
}

/// Starts a `spawn_subagent` call, settles the child's context/skills/prompt discovery,
/// and waits for the child's `SessionCreated`. Discovery is settled here so
/// every caller starts from a gate that has been opened at least once; the MCP
/// leg stays pending, so the gate still holds until a server reports status.
async fn in_flight_subagent_call() -> InFlightSubagentCall {
    let harness = TestHarness::new().await;
    let dir = jinn_testutil::tempdir();
    let (state, parent_id) = parent_fixture(&dir.path);
    let ctx = spawn_subagent_ctx(&harness, &state, parent_id.clone(), &dir.path).await;
    let created_rec = harness.spawn_recorder::<SessionCreated>().await;
    let enqueue_rec = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let pending = tokio::spawn(execute(
        spawn_subagent_call(r#"{"prompt": "Explore."}"#),
        ctx,
    ));
    let created = await_recorded(&created_rec, 1, AWAIT_TIMEOUT).await;
    let child_id = created[0].session_id.clone();
    settle_child_discovery(&harness.bus(), &child_id, &BTreeSet::new()).await;
    InFlightSubagentCall {
        harness,
        _dir: dir,
        state,
        child_id,
        enqueue_rec,
        pending,
    }
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_holds_first_dispatch_while_the_mcp_leg_is_pending() {
    // Given an in-flight spawn_subagent call whose child session has been created and
    // whose discovery has settled with no MCP servers reporting a status.
    let task = in_flight_subagent_call().await;

    // When the enqueue is observed without any server reaching a status.
    // Then the enqueue has not been published: the gate holds for "stub".
    let held = await_recorded(&task.enqueue_rec, 0, Duration::from_millis(300)).await;
    assert!(
        held.is_empty(),
        "enqueue must not precede MCP settlement; got {}",
        held.len()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn spawn_subagent_dispatches_the_child_once_the_mcp_leg_settles() {
    // Given an in-flight spawn_subagent call still gated on its pending MCP leg.
    let task = in_flight_subagent_call().await;

    // When the server reaches its terminal status and the child finishes.
    task.harness
        .publish(jinn_mcp_msg::McpServerStatus {
            session_id: task.child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_mcp_msg::McpConnectionStatus::Running,
        })
        .await;
    finish_child_like_session_actor(
        &task.harness.bus(),
        &task.state,
        &task.child_id,
        "Found it.",
    )
    .await;
    let result = task.pending.await.expect("task join");

    // Then the enqueue was published for the child and the result forwarded.
    let enqueued = await_recorded(&task.enqueue_rec, 1, AWAIT_TIMEOUT).await;
    assert_eq!(enqueued[0].session_id, task.child_id);
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
        .publish(ContextFilesLoaded {
            session_id: child_id.clone(),
            files: vec![],
            error: Some("scan failed".to_owned()),
        })
        .await;
    harness
        .publish(jinn_skills_msg::SkillsLoaded {
            session_id: child_id.clone(),
            skills: vec![],
            error: Some("scan failed".to_owned()),
        })
        .await;
    harness
        .publish(PromptTemplatesLoaded {
            session_id: child_id.clone(),
            templates: vec![],
            error: Some("scan failed".to_owned()),
        })
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
        .publish(jinn_mcp_msg::McpServerStatus {
            session_id: child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_mcp_msg::McpConnectionStatus::Dead,
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
        .publish(jinn_mcp_msg::McpServerStatus {
            session_id: child_id.clone(),
            server: "stub".to_owned(),
            status: jinn_mcp_msg::McpConnectionStatus::Starting,
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
    crate::spawn_subagent::await_discovery_settlement(
        &harness.system().clone(),
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
    // awaiting spawn_subagent future aborted).
    let harness = TestHarness::new().await;
    let child_id = SessionId::new();
    let servers = BTreeSet::from(["stub".to_owned()]);
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel::<()>();
    drop(settled_rx);
    let listener = crate::subagent_settle_listener_actor::SubagentSettleListenerActor::spawn(
        crate::subagent_settle_listener_actor::SubagentSettleListenerDeps {
            system: harness.system().clone(),
            bus: harness.bus(),
            child_id: child_id.clone(),
            expected_servers: servers,
            settled: settled_tx,
        },
    )
    .await;

    // When a matching discovery event arrives (the abort check fires).
    harness
        .publish(jinn_skills_msg::SkillsLoaded {
            session_id: child_id.clone(),
            skills: vec![],
            error: None,
        })
        .await;

    // Then the listener stops instead of signalling a dead channel: poll
    // the system export until the listener's slot is gone.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut stopped = false;
    while tokio::time::Instant::now() < deadline {
        let export = harness.system().export().await;
        let alive = export.actors.iter().any(|a| a.path == listener);
        if !alive {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(stopped, "listener must notice the closed channel and stop");
}
