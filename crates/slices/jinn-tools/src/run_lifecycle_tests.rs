//! Tests for the `run_lifecycle` tool: hostile-argument quoting,
//! named→positional ordering, unknown-script refusal, setup coordinate
//! stamping + persistence, teardown replay-from-stored (options ignored),
//! state gates, and the two script sources — jinn.toml by name, a
//! workflow file by path — never combined. The quoting assertions call
//! the same render path the tool calls, against the lifecycle crate's
//! already-tested implementation.

#![allow(clippy::expect_used, reason = "test code")]

use marked_path::{Absolute, MarkedPathBuf};
use std::collections::BTreeMap;
use std::time::Duration;

use jinn_common::app_paths::AppPaths;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_lifecycle_msg::command_template::CommandTemplate;
use jinn_session_lifecycle_msg::{RunSessionSetup, RunSessionTeardown};
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::PersistSession;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

use crate::run_lifecycle::{execute, render_command, wait_budget_message};
use crate::tool_types::ToolContext;

/// Builds a tool call with the given JSON arguments.
fn call(arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_lc_1".into(),
        name: "run_lifecycle".to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// A config document carrying one shell lifecycle script with named args.
const SCRIPT_DOC: &str = r#"
[[session_lifecycle.script]]
name = "worktree"
description = "fork a worktree off a base branch"
setup_command = "mkdir <dir> && echo ./<dir> --branch <base>"
teardown_command = "fossil close && rm -rf <dir>"
"#;

/// Builds a tool context wired to the harness, with the script document.
///
/// Both paths must outlive the returned context — each test owns the
/// [`jinn_testutil::JinnTempDir`] they are derived from (a temp dir created
/// inside a helper would delete its tree before the test body ran).
async fn ctx_with(
    harness: &TestHarness,
    state: &State,
    session_id: SessionId,
    app_paths_root: &MarkedPathBuf<Absolute>,
    cwd: &MarkedPathBuf<Absolute>,
) -> ToolContext {
    ToolContext {
        turn: crate::tool_types::test_turn(),
        cwd: cwd.clone(),
        config: jinn_config::testutil::config_layer(SCRIPT_DOC),
        shells: Default::default(),
        timeout: None,
        state: Some(state.clone()),
        session_id: Some(session_id),
        app_paths: AppPaths::new_in(&app_paths_root.as_marked_path()),
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

/// Seeds one plain session; returns (state, session_id).
fn fixture() -> (State, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let session = ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    (state, id)
}

/// Seeds one session whose lifecycle already ran setup: the worktree
/// coordinates stamped at setup time (args in raw-template order) and the
/// state advanced past `nothing_ran` — the state a teardown replay meets.
fn setup_ran_fixture() -> (State, SessionId) {
    let (state, id) = fixture();
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("session");
        session.set_lifecycle_name(Some("worktree".to_owned()));
        session.set_lifecycle_args(jinn_session_state::fields::LifecycleArgs::from_pairs([(
            "dir", "wt-a",
        )]));
        session.set_lifecycle_source(None);
        session.advance_lifecycle_after_setup();
    }
    (state, id)
}

#[rstest::rstest]
#[test]
fn hostile_argument_values_cannot_restructure_the_command() {
    // Given values carrying spaces, quotes, dollar signs, and separators.
    let args = BTreeMap::from([
        ("dir".to_owned(), "feat x; rm -rf $HOME".to_owned()),
        ("base".to_owned(), "it's $(pwd) `back`".to_owned()),
    ]);

    // When rendering the script's setup command.
    let rendered =
        render_command("mkdir <dir> && echo ./<dir> --branch <base>", &args).expect("renders");

    // Then every hostile value landed inside single quotes: the command's
    // structure (`&&`, words) is exactly the script author's, and no value
    // contributed a separator, expansion, or quote of its own.
    assert!(rendered.contains("'feat x; rm -rf $HOME'"), "{rendered}");
    assert!(rendered.contains("'it'\\''s $(pwd) `back`'"), "{rendered}");
    assert_eq!(
        rendered.matches("&&").count(),
        1,
        "the only && is the script's own: {rendered}"
    );
}

#[rstest::rstest]
#[test]
fn a_value_containing_a_placeholder_shape_is_not_re_substituted() {
    // Given a value that looks like a placeholder.
    let args = BTreeMap::from([
        ("dir".to_owned(), "<base>".to_owned()),
        ("base".to_owned(), "trunk".to_owned()),
    ]);

    // When rendering.
    let rendered = render_command("echo <dir> <base>", &args).expect("renders");

    // Then the placeholder-shaped value was quoted literally, not swapped
    // for the other argument: render substitutes in one pass.
    assert!(rendered.contains("'<base>'"), "{rendered}");
    assert!(rendered.contains("trunk"), "{rendered}");
}

#[rstest::rstest]
#[test]
fn missing_argument_fails_naming_the_placeholder() {
    // Given args missing one of the script's placeholders.
    let args = BTreeMap::from([("base".to_owned(), "trunk".to_owned())]);

    // When rendering.
    let result = render_command("mkdir <dir> --branch <base>", &args);

    // Then it fails naming <dir>.
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("<dir>"));
}

#[rstest::rstest]
#[test]
fn pairing_stamps_names_in_template_parameter_order() {
    // Given args supplied in an order different from the template's.
    let args = BTreeMap::from([
        ("base".to_owned(), "trunk".to_owned()),
        ("dir".to_owned(), "wt".to_owned()),
    ]);

    // When pairing against the template (dir first, base second).
    let template = CommandTemplate::parse("mkdir <dir> --branch <base>");
    let paired = crate::run_lifecycle::pair_args(&template, &args);

    // Then the pairs carry the name with each value, in template order.
    assert_eq!(
        paired.as_pairs(),
        &[
            ("dir".to_owned(), "wt".to_owned()),
            ("base".to_owned(), "trunk".to_owned()),
        ]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn setup_stamps_coordinates_and_persists_before_the_run_command() {
    // Given a session and a harness recording setup commands and persists.
    // The temp dir outlives the setup block: the tool's context points
    // into it, and the tool runs after the block closes.
    let dir = jinn_testutil::tempdir();
    let (harness, setups, persists, state, id, tool_ctx, adopted_cwd) = {
        let harness = TestHarness::new().await;
        let setups = harness.spawn_recorder::<RunSessionSetup>().await;
        let persists = harness.spawn_recorder::<PersistSession>().await;
        let (state, id) = fixture();
        let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;
        let adopted_cwd = dir
            .path
            .join("inherited-project")
            .expect("temp root joins a bare name");
        (harness, setups, persists, state, id, tool_ctx, adopted_cwd)
    };

    // When the tool runs setup — racing it against our own completion
    // publish, since no lifecycle actor exists in this harness to answer.
    let tool = tokio::spawn(execute(
        call(&format!(
            r#"{{"script":"worktree","action":"setup","session_id":"{id}","args":{{"dir":"wt-a","base":"trunk"}}}}"#
        )),
        tool_ctx,
    ));
    // Give the listener a moment to subscribe, then answer as the actor
    // would.
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    let published = await_recorded::<RunSessionSetup>(&setups, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionSetupCompleted {
            session_id: id.clone(),
            cwd: adopted_cwd.clone(),
            error: None,
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the tool succeeded, the session carries the script name, the
    // args in raw-template parameter order, and the source — stamped by
    // the time the call completed — and a persist was published so the
    // coordinates survive a restart.
    assert!(result.success, "{}", result.content);
    let guard = state.read();
    let session = guard.session.get(&id).expect("session");
    assert_eq!(session.lifecycle_name(), Some("worktree"));
    assert_eq!(
        session.lifecycle_args().as_pairs(),
        &[
            ("dir".to_owned(), "wt-a".to_owned()),
            ("base".to_owned(), "trunk".to_owned()),
        ]
    );
    assert_eq!(session.lifecycle_source(), None);
    drop(guard);
    let persisted = await_recorded::<PersistSession>(&persists, 1, Duration::from_secs(2)).await;
    assert!(
        persisted.iter().any(|m| m.session_id == id),
        "the stamped coordinates must be persisted"
    );

    // And the published command carries the rendered, quoted command with
    // ordered args.
    let recorded = published.clone();
    assert_eq!(recorded[0].session_id, id);
    // Safe values pass through unquoted (shell_quote quotes only specials).
    assert!(
        recorded[0].command.contains("mkdir wt-a"),
        "{}",
        recorded[0].command
    );
    assert!(
        recorded[0].command.contains("--branch trunk"),
        "{}",
        recorded[0].command
    );
    assert_eq!(
        recorded[0].args,
        vec!["wt-a".to_owned(), "trunk".to_owned()]
    );
    // And the tool reported the adopted cwd.
    assert!(
        result.content.contains(&adopted_cwd.to_string()),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_ignores_supplied_options_and_replays_the_stored_coordinates() {
    // Given a session whose lifecycle already ran setup, with stored
    // coordinates, and a harness recording teardown commands.
    // The temp dir outlives the setup block: the tool's context points
    // into it, and the tool runs after the block closes.
    let dir = jinn_testutil::tempdir();
    let (harness, teardowns, state, id, tool_ctx) = {
        let harness = TestHarness::new().await;
        let teardowns = harness.spawn_recorder::<RunSessionTeardown>().await;
        let (state, id) = setup_ran_fixture();
        let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;
        (harness, teardowns, state, id, tool_ctx)
    };

    // When the tool runs teardown with a WRONG script, a WRONG path, and
    // WRONG args — none of which the call's stored coordinates agree with.
    // The subdirectories are never created, so the path still does not exist.
    let gone_path = dir
        .path
        .join("gone/workflow/lifecycle.toml")
        .expect("temp root joins a bare name");
    let tool = tokio::spawn(execute(
        call(
            &serde_json::json!({
                "script": "stage",
                "path": gone_path.to_string(),
                "action": "teardown",
                "session_id": id,
                "args": { "dir": "WRONG", "base": "WRONGER" },
            })
            .to_string(),
        ),
        tool_ctx,
    ));
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    let published =
        await_recorded::<RunSessionTeardown>(&teardowns, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionTeardownFinished {
            session_id: id.clone(),
            error: None,
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the call succeeded: every supplied option was ignored, the
    // stored coordinates are intact, and the published command is the
    // replay rendered from the stored args — the worktree teardown with
    // the stored "wt-a", not the wrong "WRONG".
    assert!(result.success, "{}", result.content);
    let guard = state.read();
    let session = guard.session.get(&id).expect("session");
    assert_eq!(session.lifecycle_name(), Some("worktree"));
    assert_eq!(
        session.lifecycle_args().as_pairs(),
        &[("dir".to_owned(), "wt-a".to_owned())]
    );
    assert_eq!(session.lifecycle_source(), None);
    drop(guard);
    let recorded = published.clone();
    assert_eq!(recorded[0].session_id, id);
    assert!(
        recorded[0].command.contains("rm -rf 'wt-a'")
            || recorded[0].command.contains("rm -rf wt-a"),
        "the replay must render from the STORED args: {}",
        recorded[0].command
    );
    assert!(
        !recorded[0].command.contains("WRONG"),
        "a supplied option leaked into the replay: {}",
        recorded[0].command
    );
}

/// Seeds one session whose lifecycle state is `SetupRan` but which
/// recorded no lifecycle name — the "nothing recorded at teardown" shape
/// (advanced past setup, coordinates never stamped).
fn setup_ran_without_record_fixture() -> (State, SessionId) {
    let (state, id) = fixture();
    state
        .write()
        .session
        .get_mut(&id)
        .expect("session")
        .advance_lifecycle_after_setup();
    (state, id)
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_for_a_session_with_nothing_recorded_fails_fast() {
    // Given a harness with a session that ran setup but recorded no
    // lifecycle — and no listener answer, so a hang would consume the
    // whole wait budget.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let teardowns = harness.spawn_recorder::<RunSessionTeardown>().await;
    let (state, id) = setup_ran_without_record_fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs teardown for that session.
    let started = std::time::Instant::now();
    let result = execute(
        call(&format!(r#"{{"action":"teardown","session_id":"{id}"}}"#)),
        tool_ctx,
    )
    .await;

    // Then it fails fast naming the session, and publishes nothing.
    assert!(!result.success);
    assert!(
        result.content.contains("no lifecycle script recorded"),
        "{}",
        result.content
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the refusal must not spend the wait budget"
    );
    assert!(teardowns.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_before_setup_fails_naming_the_state() {
    // Given a session whose lifecycle state is still nothing_ran.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs teardown for that session.
    let result = execute(
        call(&format!(r#"{{"action":"teardown","session_id":"{id}"}}"#)),
        tool_ctx,
    )
    .await;

    // Then it fails naming the state gate, not the missing record — the
    // state is the nearer refusal.
    assert!(!result.success);
    assert!(
        result.content.contains("teardown requires setup_ran"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn setup_on_a_session_that_already_ran_setup_fails_naming_the_state() {
    // Given a session whose lifecycle already ran setup.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let setups = harness.spawn_recorder::<RunSessionSetup>().await;
    let (state, id) = setup_ran_fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs setup for that session.
    let result = execute(
        call(&format!(
            r#"{{"script":"worktree","action":"setup","session_id":"{id}","args":{{"dir":"wt-b","base":"trunk"}}}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then it fails naming the state gate, and nothing was stamped or
    // published.
    assert!(!result.success);
    assert!(
        result.content.contains("setup requires nothing_ran"),
        "{}",
        result.content
    );
    let guard = state.read();
    let session = guard.session.get(&id).expect("session");
    assert_eq!(
        session.lifecycle_args().as_pairs(),
        &[("dir".to_owned(), "wt-a".to_owned())]
    );
    drop(guard);
    assert!(setups.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn setup_without_the_script_argument_refuses() {
    // Given a harness with a plain session and the schema no longer
    // requiring script at the schema level.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs setup with no script argument.
    let result = execute(
        call(&format!(
            r#"{{"action":"setup","session_id":"{id}","args":{{}}}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then it refuses naming the requirement, before any state write.
    assert!(!result.success);
    assert!(
        result
            .content
            .contains("setup requires the script argument"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[test]
fn the_wait_budget_message_says_the_command_may_still_be_running() {
    // Given an action and script name.
    // When building the wait-budget message.
    let message = wait_budget_message("setup", "worktree");

    // Then it names the pair and states honestly that the command may
    // still be running — the budget bounds the wait, not the script.
    assert!(message.contains("setup"), "{message}");
    assert!(message.contains("worktree"), "{message}");
    assert!(message.contains("may still be running"), "{message}");
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_script_fails_naming_the_known_ones() {
    // Given a state with the worktree script configured.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs an unknown script name.
    let result = execute(
        call(&format!(
            r#"{{"script":"nope","action":"setup","session_id":"{id}","args":{{}}}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then it fails naming the miss and the known scripts.
    assert!(!result.success);
    assert!(result.content.contains("unknown lifecycle script"));
    assert!(result.content.contains("worktree"));
}

#[rstest::rstest]
#[tokio::test]
async fn missing_placeholder_argument_fails_before_any_publish() {
    // Given a harness recording setup commands.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let setups = harness.spawn_recorder::<RunSessionSetup>().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs with only one of the two placeholders supplied.
    let result = execute(
        call(&format!(
            r#"{{"script":"worktree","action":"setup","session_id":"{id}","args":{{"dir":"wt"}}}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then it fails naming <base> and publishes nothing.
    assert!(!result.success);
    assert!(result.content.contains("<base>"));
    // The tool's own result is the positive signal that it failed before
    // publishing. The recorder must have stopped changing before "nothing
    // was published" is a real observation rather than a race.
    jinn_testutil::wait::await_settled(&setups, Duration::from_millis(200), Duration::from_secs(5))
        .await;
    assert!(setups.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn a_failed_script_reports_the_failure_as_a_failed_result() {
    // Given a session whose script will report an error.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let setups = harness.spawn_recorder::<RunSessionSetup>().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs and the completion carries an error.
    let tool = tokio::spawn(execute(
        call(&format!(
            r#"{{"script":"worktree","action":"setup","session_id":"{id}","args":{{"dir":"wt","base":"trunk"}}}}"#
        )),
        tool_ctx,
    ));
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    await_recorded::<RunSessionSetup>(&setups, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionSetupCompleted {
            session_id: id.clone(),
            cwd: dir.path.clone(),
            error: Some("exit code 1".to_owned()),
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the tool result fails and carries the script's error.
    assert!(!result.success);
    assert!(result.content.contains("exit code 1"), "{}", result.content);
}

// ── Path source ──────────────────────────────────────────────────────
//
// The second script source: a workflow-local lifecycle TOML read by
// exact path, with the name selecting the entry inside it. The two
// sources never combine — a call carrying a path never reads jinn.toml,
// and one carrying neither source is refused. Each test owns its temp
// dir; `workflow_file` only derives the file's path.

/// The document shape a workflow's `lifecycle.toml` ships: the bare
/// `[[session_lifecycle.script]]` section.
const WORKFLOW_DOC: &str = r#"
[[session_lifecycle.script]]
name = "stage"
description = "fork a stage worktree"
setup_command = "mkdir -p <dir> && echo ./<dir>"
teardown_command = "echo closing <dir>"

[[session_lifecycle.script]]
name = "purge"
description = "delete the trash"
setup_command = "rm -rf .trash"
teardown_command = "true"
"#;

/// Writes a workflow lifecycle file into a temp dir; returns the temp dir
/// (held for the test's lifetime) and the file's path.
fn workflow_file(doc: &str) -> (jinn_testutil::JinnTempDir, MarkedPathBuf<Absolute>) {
    let dir = jinn_testutil::tempdir();
    let path = dir
        .path
        .join("lifecycle.toml")
        .expect("temp root joins a bare name");
    std::fs::write(&path, doc).expect("write workflow lifecycle file");
    (dir, path)
}

#[rstest::rstest]
#[tokio::test]
async fn path_source_resolves_the_named_entry_and_publishes_the_same_shape() {
    // Given a workflow lifecycle file with two entries and a config layer
    // carrying a DIFFERENT script of the same name — proof the path
    // bypasses jinn.toml entirely.
    let (_dir, path) = workflow_file(WORKFLOW_DOC);
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let setups = harness.spawn_recorder::<RunSessionSetup>().await;
    let (state, id) = fixture();
    let mut tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;
    tool_ctx.config = jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"stage\"\n\
         setup_command = \"echo from-user-config\"\n",
    );

    // When the tool runs setup by path, answered by the completion event.
    let tool = tokio::spawn(execute(
        call(
            &serde_json::json!({
                "script": "stage",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": { "dir": "wt" }
            })
            .to_string(),
        ),
        tool_ctx,
    ));
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    let published = await_recorded::<RunSessionSetup>(&setups, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionSetupCompleted {
            session_id: id.clone(),
            cwd: dir.path.join("wt").expect("temp root joins a bare name"),
            error: None,
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the command rendered from the FILE's entry, not the config's.
    assert!(result.success, "{}", result.content);
    let recorded = published.clone();
    assert!(
        recorded[0].command.contains("mkdir -p wt"),
        "{}",
        recorded[0].command
    );
    assert!(
        !recorded[0].command.contains("from-user-config"),
        "the path source must bypass jinn.toml: {}",
        recorded[0].command
    );
    assert_eq!(recorded[0].args, vec!["wt".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn a_relative_path_resolves_against_the_session_cwd() {
    // Given a workflow file inside the session's cwd, and a separate temp
    // root for the context paths (this test owns both TempDirs).
    let dir = jinn_testutil::tempdir();
    let ctx_dir = jinn_testutil::tempdir();
    let rel = "workflows/autocoder/lifecycle.toml";
    std::fs::create_dir_all(
        dir.path
            .join("workflows/autocoder")
            .expect("temp root joins a bare name"),
    )
    .expect("mkdir");
    std::fs::write(
        dir.path.join(rel).expect("temp root joins a bare name"),
        WORKFLOW_DOC,
    )
    .expect("write");
    let harness = TestHarness::new().await;
    let setups = harness.spawn_recorder::<RunSessionSetup>().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &ctx_dir.path, &dir.path).await;

    // When the tool is called with the relative path, answered by the
    // completion event (the tool always awaits one — an unreadable file
    // would have failed fast, before this publish could matter).
    let tool = tokio::spawn(execute(
        call(
            &serde_json::json!({
                "script": "purge",
                "path": rel,
                "action": "setup",
                "session_id": id,
                "args": {},
            })
            .to_string(),
        ),
        tool_ctx,
    ));
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    let published = await_recorded::<RunSessionSetup>(&setups, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionSetupCompleted {
            session_id: id.clone(),
            cwd: dir.path.clone(),
            error: None,
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the file was found through the cwd: the call succeeded and the
    // published command is the purge entry rendered from the file.
    assert!(result.success, "{}", result.content);
    let command = published
        .first()
        .expect("the setup command was published")
        .command
        .clone();
    assert!(command.contains("rm -rf .trash"), "{command}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_unreadable_path_fails_naming_the_file() {
    // Given a path that does not exist (the subdirectories are never
    // created, so the joined path stays missing).
    let dir = jinn_testutil::tempdir();
    let missing_path = dir
        .path
        .join("no/such/lifecycle.toml")
        .expect("temp root joins a bare name");
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs with that path.
    let result = execute(
        call(
            &serde_json::json!({
                "script": "stage",
                "path": missing_path.to_string(),
                "action": "setup",
                "session_id": id,
                "args": {},
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then it fails naming the file.
    assert!(!result.success);
    assert!(
        result.content.contains(missing_path.to_string().as_str()),
        "{}",
        result.content
    );
    assert!(result.content.contains("cannot read"), "{}", result.content);
}

#[rstest::rstest]
#[tokio::test]
async fn an_unparseable_path_file_fails_naming_the_file() {
    // Given a file that is not a lifecycle document.
    let (_dir, path) = workflow_file("not = [a lifecycle");
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs with that path.
    let result = execute(
        call(
            &serde_json::json!({
                "script": "stage",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": {}
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then it fails naming the file and the parse reason.
    assert!(!result.success);
    assert!(
        result.content.contains("lifecycle.toml"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("not a lifecycle document"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_name_missing_from_the_path_file_fails_naming_the_file_and_entries() {
    // Given the shipped-shaped workflow file.
    let (_dir, path) = workflow_file(WORKFLOW_DOC);
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs with a script name the file does not define —
    // and the user's jinn.toml DOES define (the config carries "stage").
    let result = execute(
        call(
            &serde_json::json!({
                "script": "nope",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": {}
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then it fails naming the file and the entries the file defines —
    // never falling through to the config, where "nope" is also unknown
    // but the error would name the wrong source.
    assert!(!result.success);
    assert!(
        result.content.contains("lifecycle.toml"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("no lifecycle script named"),
        "{}",
        result.content
    );
    assert!(result.content.contains("purge"), "{}", result.content);
}

#[rstest::rstest]
#[tokio::test]
async fn an_entry_without_the_requested_half_fails() {
    // Given a workflow file whose entry has no setup.
    let (_dir, path) = workflow_file(
        "[[session_lifecycle.script]]\nname = \"solo\"\n\
         teardown_command = \"echo bye\"\n",
    );
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs the entry's setup.
    let result = execute(
        call(
            &serde_json::json!({
                "script": "solo",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": {}
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then it fails naming the entry and the missing half.
    assert!(!result.success);
    assert!(result.content.contains("solo"), "{}", result.content);
    assert!(
        result.content.contains("no setup command"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_builtin_entry_is_refused_through_the_path_source_too() {
    // Given a workflow file whose entry is a builtin command.
    let (_dir, path) = workflow_file(
        "[[session_lifecycle.script]]\nname = \"built\"\n\
         setup_command = { builtin = \"some-handler\" }\n",
    );
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs that entry.
    let result = execute(
        call(
            &serde_json::json!({
                "script": "built",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": {}
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then the refusal is the same shape the config source produces.
    assert!(!result.success);
    assert!(result.content.contains("builtin"), "{}", result.content);
}

#[rstest::rstest]
#[tokio::test]
async fn path_with_blank_script_refuses_naming_the_requirement() {
    // Given a workflow file and a call whose script is the empty string —
    // which satisfies the schema's `required` array.
    let (_dir, path) = workflow_file(WORKFLOW_DOC);
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs with that blank name and the path present.
    let result = execute(
        call(
            &serde_json::json!({
                "script": "",
                "path": path.to_string(),
                "action": "setup",
                "session_id": id.to_string(),
                "args": {}
            })
            .to_string(),
        ),
        tool_ctx,
    )
    .await;

    // Then it refuses with the explicit requirement, never a first-entry
    // guess and never the unknown-script error.
    assert!(!result.success);
    assert!(
        result.content.contains("script is required with path"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn neither_source_refuses() {
    // Given a call with no path and an unknown script name.
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let (state, id) = fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs.
    let result = execute(
        call(&format!(
            r#"{{"script":"missing","action":"setup","session_id":"{id}","args":{{}}}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then it fails naming the miss and the known scripts — the config
    // source's refusal, unchanged.
    assert!(!result.success);
    assert!(
        result.content.contains("unknown lifecycle script"),
        "{}",
        result.content
    );
    assert!(result.content.contains("worktree"), "{}", result.content);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_replays_even_when_the_call_carries_a_path() {
    // Given a session whose lifecycle already ran setup from the user's
    // config, and a workflow file whose stage entry exists but is NOT the
    // session's lifecycle.
    let (_dir, path) = workflow_file(WORKFLOW_DOC);
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let teardowns = harness.spawn_recorder::<RunSessionTeardown>().await;
    let (state, id) = setup_ran_fixture();
    let tool_ctx = ctx_with(&harness, &state, id.clone(), &dir.path, &dir.path).await;

    // When the tool runs teardown carrying that path and the file's entry
    // name, answered by the finish event.
    let tool = tokio::spawn(execute(
        call(
            &serde_json::json!({
                "script": "stage",
                "path": path.to_string(),
                "action": "teardown",
                "session_id": id.to_string(),
                "args": { "dir": "wt" }
            })
            .to_string(),
        ),
        tool_ctx,
    ));
    // The tool spawns its completion listener before it publishes the
    // run command, so the command's arrival on the bus is the signal that
    // the listener is subscribed. A fixed sleep raced that subscription.
    let published =
        await_recorded::<RunSessionTeardown>(&teardowns, 1, Duration::from_secs(5)).await;
    harness
        .publish(jinn_session_lifecycle_msg::SessionTeardownFinished {
            session_id: id.clone(),
            error: None,
        })
        .await;
    let result = tool.await.expect("tool task");

    // Then the call succeeded and the replay is the session's OWN recorded
    // lifecycle — the worktree teardown from jinn.toml with the stored
    // "wt-a" — not the file's stage entry. The path and name were ignored;
    // the stored coordinates are untouched.
    assert!(result.success, "{}", result.content);
    let guard = state.read();
    let session = guard.session.get(&id).expect("session");
    assert_eq!(session.lifecycle_name(), Some("worktree"));
    assert_eq!(session.lifecycle_source(), None);
    drop(guard);
    let recorded = published.clone();
    assert!(
        recorded[0].command.contains("rm -rf") && !recorded[0].command.contains("closing"),
        "the replay must come from the recorded lifecycle, not the call's file: {}",
        recorded[0].command
    );
}
