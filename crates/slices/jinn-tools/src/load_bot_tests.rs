//! Tests for the `load_bot` tool and the workflow-source save-block.

#![allow(clippy::expect_used, reason = "test code")]

use std::time::Duration;

use jinn_bot_msg::{BotBehavior, BotTrigger};
use jinn_common::app_paths::AppPaths;
use jinn_core_types::BotSourcePath;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::PersistSession;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use marked_path::{Absolute, MarkedPathBuf};

use crate::load_bot::{definition, execute, parse_bot_entry};
use crate::tool_types::ToolContext;

/// Builds a tool call with the given JSON arguments.
fn call(arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_load_1".into(),
        name: "load_bot".to_owned(),
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

/// Seeds one user session; returns (state, user_id).
fn fixture() -> (State, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let user = ChatSessionState::new();
    let user_id = user.session_id().clone();
    state.write().session.insert(user);
    (state, user_id)
}

/// Writes a bot file under a fresh prefixed temp dir; returns the
/// [`jinn_testutil::JinnTempDir`] (its Drop removes the tree) and the file's path.
fn bot_file(name: &str, body: &str) -> (jinn_testutil::JinnTempDir, String) {
    let dir = jinn_testutil::tempdir_with_prefix(&format!("jinn-test-load-bot-{name}-"));
    let path = dir
        .path
        .join("bot.toml")
        .expect("temp root joins a bare name");
    std::fs::write(&path, body).expect("write bot file");
    let path = path.to_string();
    (dir, path)
}

const BOT_BODY: &str = r#"
name = "implementer"
behavior = "preserve"
trigger = "manual"
prep_mode = false
seed_template = "You implement one stage of an autocoder run."
"#;

#[rstest::rstest]
#[tokio::test]
async fn load_attaches_a_bot_and_stamps_the_marker_with_the_file_path() {
    // Given a user session and a workflow-local bot file on disk.
    let harness = TestHarness::new().await;
    let persists = harness.spawn_recorder::<PersistSession>().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("stamp", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads the file.
    let result = execute(
        call(&serde_json::json!({ "path": path.clone() }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool succeeds and the bot exists as a child.
    assert!(result.success, "load must succeed: {}", result.content);
    let guard = state.read();
    let bot = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("a loaded bot must exist");
    assert_eq!(bot.parent_session().as_ref(), Some(&user_id));
    assert_eq!(bot.title(), Some("implementer"));
    // And it carries the source marker naming its source file.
    assert_eq!(
        bot.workflow_source(),
        Some(&BotSourcePath::new(path.clone()))
    );
    assert!(!bot.bot_is_prepping(), "file config applied");
    // And the creation published persists for parent and bot.
    drop(guard);
    let persisted = await_recorded::<PersistSession>(&persists, 2, Duration::from_secs(2)).await;
    assert_eq!(persisted.len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn inline_toml_load_attaches_a_bot_stamped_inline() {
    // Given a user session and a bot entry handed to the call as TOML text.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads the entry inline.
    let result = execute(
        call(&serde_json::json!({ "toml": BOT_BODY }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool succeeds and the bot exists as a child, stamped
    // with the inline marker.
    assert!(result.success, "load must succeed: {}", result.content);
    assert!(
        result.content.contains("inline"),
        "the result names the source: {}",
        result.content
    );
    let guard = state.read();
    let bot = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("a loaded bot must exist");
    assert_eq!(bot.parent_session().as_ref(), Some(&user_id));
    assert_eq!(bot.title(), Some("implementer"));
    assert_eq!(
        bot.workflow_source(),
        Some(&BotSourcePath::new("inline".to_owned())),
        "the inline load stamps `inline`"
    );
    assert!(!bot.bot_is_prepping(), "inline config applied");
}

#[rstest::rstest]
#[tokio::test]
async fn children_of_a_loaded_bot_do_not_inherit_the_marker() {
    // Given a bot loaded from a workflow-local file.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("inherit", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    let result = execute(
        call(&serde_json::json!({ "path": path }).to_string()),
        tool_ctx,
    )
    .await;
    assert!(result.success);

    // When a child is created the ordinary way from that bot
    // (`new_bot` is the creation path every spawner shares).
    let guard = state.read();
    let loaded = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("loaded bot");
    let child = ChatSessionState::new_bot(loaded, true);
    let child_id = child.session_id().clone();
    drop(guard);
    state.write().session.insert(child);

    // Then the child is an ordinary personal bot: no marker.
    let guard = state.read();
    let child = guard.session.get(&child_id).expect("child");
    assert_eq!(
        child.workflow_source(),
        None,
        "the marker is never inherited"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn children_of_an_inline_bot_do_not_inherit_the_marker() {
    // Given a bot loaded inline from the call's toml argument.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    let result = execute(
        call(&serde_json::json!({ "toml": BOT_BODY }).to_string()),
        tool_ctx,
    )
    .await;
    assert!(result.success);

    // When a child is created the ordinary way from that bot.
    let guard = state.read();
    let loaded = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("inline bot");
    let child = ChatSessionState::new_bot(loaded, true);
    let child_id = child.session_id().clone();
    drop(guard);
    state.write().session.insert(child);

    // Then the child is an ordinary personal bot: no marker.
    let guard = state.read();
    let child = guard.session.get(&child_id).expect("child");
    assert_eq!(
        child.workflow_source(),
        None,
        "the marker is never inherited"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn missing_file_fails_naming_the_path() {
    // Given a session and a path that does not exist. It is anchored at the
    // volume root, so it is absolute on every platform and the tool uses it
    // as-is instead of joining it onto the session cwd.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    let missing = jinn_testutil::volume_root()
        .join_all(["definitely", "not", "here.toml"])
        .expect("bare-name joins keep the base absolute");
    let raw_missing = missing.to_string();

    // When the tool loads the missing file.
    let payload = serde_json::json!({ "path": raw_missing }).to_string();
    let result = execute(call(&payload), tool_ctx).await;

    // Then the tool fails naming the path and nothing is created.
    assert!(!result.success);
    assert!(result.content.contains(&raw_missing));
    let guard = state.read();
    assert!(
        !guard.session.iter().any(|(_, s)| s.is_bot()),
        "a failed load must not create anything"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn malformed_bot_file_fails_with_the_reason() {
    // Given a file that is not a valid bot entry.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("bad", "this is not = toml = ==");
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads it.
    let result = execute(
        call(&serde_json::json!({ "path": path }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool fails explaining why.
    assert!(!result.success);
    assert!(result.content.contains("not a valid bot entry"));
}

#[rstest::rstest]
#[tokio::test]
async fn malformed_inline_toml_fails_with_the_parse_reason() {
    // Given inline text that is not a valid bot entry.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads it inline.
    let result = execute(
        call(&serde_json::json!({ "toml": "this is not = toml = =" }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool fails with the parse reason and no fabricated path.
    assert!(!result.success);
    assert!(
        result.content.contains("not a valid bot entry"),
        "the reason is surfaced: {}",
        result.content
    );
    assert!(
        !result.content.contains(".toml"),
        "no path is fabricated for inline text: {}",
        result.content
    );
    let guard = state.read();
    assert!(
        !guard.session.iter().any(|(_, s)| s.is_bot()),
        "a failed load must not create anything"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn load_bot_with_both_path_and_toml_fails_naming_both_keys() {
    // Given a session, a bot file on disk, and inline text.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("both", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool is called with both arguments.
    let result = execute(
        call(&serde_json::json!({ "path": path, "toml": BOT_BODY }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool fails naming both keys and nothing is created.
    assert!(!result.success);
    assert!(
        result.content.contains("`path`") && result.content.contains("`toml`"),
        "the refusal names both keys: {}",
        result.content
    );
    let guard = state.read();
    assert!(
        !guard.session.iter().any(|(_, s)| s.is_bot()),
        "a refused call must not create anything"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn load_bot_with_neither_path_nor_toml_fails_naming_both_keys() {
    // Given a session and a call with no load-source argument.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool is called with neither argument.
    let result = execute(
        call(&serde_json::json!({ "set_name": "x" }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the tool fails naming both keys and nothing is created.
    assert!(!result.success);
    assert!(
        result.content.contains("`path`") && result.content.contains("`toml`"),
        "the refusal names both keys: {}",
        result.content
    );
    let guard = state.read();
    assert!(
        !guard.session.iter().any(|(_, s)| s.is_bot()),
        "a refused call must not create anything"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn bot_caller_cannot_load_a_bot_from_a_file() {
    // Given a user session with a bot, calling from the bot.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let bot = {
        let guard = state.read();
        ChatSessionState::new_bot(guard.session.get(&user_id).expect("user"), true)
    };
    let bot_id = bot.session_id().clone();
    state.write().session.insert(bot);
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("nested", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the bot tries to load another bot.
    let result = execute(
        call(&serde_json::json!({ "path": path }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the load is refused: loaded bots hang off non-bot parents.
    assert!(!result.success);
    assert!(result.content.contains("non-bot"));
}

#[rstest::rstest]
#[tokio::test]
async fn bot_caller_cannot_load_a_bot_inline() {
    // Given a user session with a bot, calling from the bot.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let bot = {
        let guard = state.read();
        ChatSessionState::new_bot(guard.session.get(&user_id).expect("user"), true)
    };
    let bot_id = bot.session_id().clone();
    state.write().session.insert(bot);
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, bot_id, &dir.path).await;

    // When the bot tries to load another bot inline.
    let result = execute(
        call(&serde_json::json!({ "toml": BOT_BODY }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the load is refused: loaded bots hang off non-bot parents.
    assert!(!result.success);
    assert!(result.content.contains("non-bot"));
}

#[rstest::rstest]
#[test]
fn parse_accepts_the_bare_entry_shape() {
    // Given a bare bot entry body.
    // When parsing.
    let entry = parse_bot_entry(BOT_BODY).expect("parses");

    // Then the fields came across.
    assert_eq!(entry.name, "implementer");
    assert!(!entry.prep_mode);
}

#[rstest::rstest]
#[test]
fn parse_accepts_a_single_entry_wrapped_in_an_entry_array() {
    // Given a wrapped body with one entry.
    let body = format!("[[entry]]\n{}", BOT_BODY.trim_start_matches('\n'));

    // When parsing.
    let entry = parse_bot_entry(&body).expect("parses");

    // Then the single entry comes across.
    assert_eq!(entry.name, "implementer");
}

#[rstest::rstest]
#[test]
fn parse_refuses_a_wrapped_file_carrying_several_entries() {
    // Given a wrapped body with two entries.
    let one = BOT_BODY.trim_start_matches('\n');
    let body = format!("[[entry]]\n{one}\n[[entry]]\nname = \"second\"\n");

    // When parsing.
    let result = parse_bot_entry(&body);

    // Then it refuses with a reason naming the count.
    let error = result.expect_err("two entries must be refused");
    assert!(
        error.to_string().contains("2 entries"),
        "the reason names the count: {error}"
    );
}

#[rstest::rstest]
#[test]
fn parse_accepts_the_full_document_shape_jinn_toml_uses() {
    // Given a body in the shape jinn.toml itself uses — the array
    // nested under an `bot` table (`[[bot.entry]]`) — which is the
    // shape the tool's description and the shipped autocoder bot
    // files both promise.
    let one = BOT_BODY.trim_start_matches('\n');
    let body = format!("[[bot.entry]]\n{one}\n");

    // When parsing.
    let entry = parse_bot_entry(&body).expect("the jinn.toml shape parses");

    // Then the single entry comes across with its fields intact.
    assert_eq!(entry.name, "implementer");
}

#[rstest::rstest]
#[test]
fn parse_still_accepts_the_legacy_attendant_document_shape() {
    // Given a body carrying the pre-rename section key — the shape
    // older saved files and stale workflow copies may still use.
    let one = BOT_BODY.trim_start_matches('\n');
    let body = format!("[[attendant.entry]]\n{one}\n");

    // When parsing.
    let entry = parse_bot_entry(&body).expect("the legacy shape parses");

    // Then the single entry comes across.
    assert_eq!(entry.name, "implementer");
}

#[rstest::rstest]
#[tokio::test]
async fn personal_bots_still_save_and_loaded_bots_refuse() {
    // Given a personal bot (no marker) and a loaded bot, both live.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let personal = {
        let guard = state.read();
        ChatSessionState::new_bot(guard.session.get(&user_id).expect("user"), true)
    };
    let personal_id = personal.session_id().clone();
    state.write().session.insert(personal);
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("savepath", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads the bot.
    let result = execute(
        call(&serde_json::json!({ "path": path }).to_string()),
        tool_ctx,
    )
    .await;
    assert!(result.success);

    // Then the personal one has no marker and the loaded one does.
    let guard = state.read();
    let personal = guard.session.get(&personal_id).expect("personal");
    assert_eq!(personal.workflow_source(), None);
    let loaded = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot() && s.session_id() != &personal_id)
        .expect("loaded bot");
    assert!(loaded.workflow_source().is_some());
}

#[rstest::rstest]
#[test]
fn definition_names_the_load_source_arguments() {
    // Given the tool definition.
    let def = definition();

    // When its parameters are read.
    // Then it is named load_bot with neither load source required —
    // exactly-one-of is enforced at run time, not in the schema.
    assert_eq!(def.name, "load_bot");
    let required = def.parameters["required"].as_array().expect("required");
    assert!(required.is_empty(), "required must be empty: {required:?}");
    // And both load-source arguments are declared, along with set_name.
    let props = def.parameters["properties"].as_object().expect("props");
    for key in ["path", "toml", "set_name"] {
        assert!(props.contains_key(key), "{key} is a declared argument");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn set_name_relabels_the_instance_and_leaves_the_seed_name_alone() {
    // Given a user session and a seed file named "implementer".
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("setname", BOT_BODY);

    // When the tool loads it with set_name = "implementor-002".
    let result = execute(
        call(&serde_json::json!({ "path": path, "set_name": "implementor-002" }).to_string()),
        ctx(&harness, &state, user_id.clone(), &dir.path).await,
    )
    .await;

    // Then the tool succeeds and the bot's title is the new label.
    assert!(result.success, "load must succeed: {}", result.content);
    assert!(
        result.content.contains("implementor-002"),
        "the result names the applied label: {}",
        result.content
    );
    let guard = state.read();
    let bot = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("loaded bot");
    assert_eq!(
        bot.title(),
        Some("implementor-002"),
        "the instance carries the caller's label"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn loading_the_same_seed_twice_with_different_set_names_yields_two_named_bots() {
    // Given a user session and one shared seed file.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("twice", BOT_BODY);

    // When it is loaded twice under two different labels.
    for label in ["implementor-001", "implementor-002"] {
        let result = execute(
            call(&serde_json::json!({ "path": path.clone(), "set_name": label }).to_string()),
            ctx(&harness, &state, user_id.clone(), &dir.path).await,
        )
        .await;
        assert!(result.success, "load must succeed: {}", result.content);
    }

    // Then both bots exist, each under its own label.
    let guard = state.read();
    let mut titles: Vec<_> = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .filter(|s| s.is_bot())
        .map(|s| s.title().expect("titled").to_owned())
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["implementor-001", "implementor-002"]);
}

#[rstest::rstest]
#[tokio::test]
async fn without_set_name_the_seed_files_name_is_used() {
    // Given a user session and a seed file named "implementer".
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let (_file_dir, path) = bot_file("noname", BOT_BODY);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool loads it with no set_name argument.
    let result = execute(
        call(&serde_json::json!({ "path": path }).to_string()),
        tool_ctx,
    )
    .await;

    // Then the bot keeps the seed file's name.
    assert!(result.success, "load must succeed: {}", result.content);
    let guard = state.read();
    let bot = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("loaded bot");
    assert_eq!(bot.title(), Some("implementer"));
}

#[rstest::rstest]
#[test]
fn load_attendant_is_no_longer_a_registered_tool() {
    // Given the built-in tool registry.
    let tools = crate::registry::builtin_tools(30);
    let names: Vec<_> = tools.iter().map(|(def, _, _)| def.name.as_str()).collect();

    // When looking for the old tool name.
    // Then it is gone — the rename is a hard cutover, so stale
    // workflow files fail visibly instead of silently working.
    assert!(
        !names.contains(&"load_attendant"),
        "the pre-rename name must not resolve: {names:?}"
    );
    // And the new name is registered.
    assert!(names.contains(&"load_bot"), "load_bot must be present");
}

/// Extracts the ready-to-go worker TOML the description advertises: the
/// `[[bot.entry]]` block through the end of the pins array.
fn advertised_worker_seed(description: &str) -> String {
    let start = description
        .find("[[bot.entry]]\nname = \"worker\"")
        .expect("description carries the ready-to-go worker seed");
    let pins_start = description[start..]
        .find("pins = [")
        .expect("seed carries pins")
        + start;
    let close = description[pins_start..]
        .find("]\n")
        .map(|i| pins_start + i + 1)
        .expect("pins array closes");
    description[start..close].to_owned()
}

#[rstest::rstest]
#[test]
fn advertised_seed_parses_as_a_valid_bot_entry() {
    // Given the ready-to-go worker TOML the description embeds.
    let description = definition().description;
    let seed = advertised_worker_seed(&description);

    // When parsing it as a bot entry.
    let entry = parse_bot_entry(&seed).expect("the advertised seed must parse");

    // Then the worker comes across ready to run: preserve behavior,
    // manual trigger, prep mode off, and a seed template carrying both
    // dispatch inlets.
    assert_eq!(entry.name, "worker");
    assert_eq!(entry.behavior, BotBehavior::Preserve);
    assert_eq!(entry.trigger, BotTrigger::Manual);
    assert!(!entry.prep_mode, "a prep-mode seed refuses every dispatch");
    assert!(entry.seed_template.contains("<parent_id>"));
    assert!(entry.seed_template.contains("<prior_report>"));
}

#[rstest::rstest]
#[test]
fn advertised_seed_pin_carries_the_report_protocol() {
    // Given the pin the advertised worker seed carries.
    let description = definition().description;
    let seed = advertised_worker_seed(&description);
    let entry = parse_bot_entry(&seed).expect("the advertised seed must parse");
    let pin = entry.pins.first().expect("the seed carries one pin");

    // When reading the pin's rules in order.
    // Then the report protocol appears in dispatch order: conclude
    // first, then notify_parent with DONE or BLOCKED, then stop.
    let conclude = pin
        .text
        .find("Call `conclude`")
        .expect("pin names conclude");
    let notify = pin
        .text
        .find("notify_parent")
        .expect("pin names notify_parent");
    let stop = pin.text.find("STOP and wait").expect("pin stops the run");
    assert!(
        conclude < notify && notify < stop,
        "report order: {}",
        pin.text
    );
    // And DONE/BLOCKED are the named outcomes.
    assert!(pin.text.contains("DONE or BLOCKED"));
}

#[rstest::rstest]
#[tokio::test]
async fn bot_from_advertised_seed_accepts_dispatch() {
    // Given a user session and the advertised seed handed to the call.
    let harness = TestHarness::new().await;
    let (state, user_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let seed = advertised_worker_seed(&definition().description);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    let result = execute(
        call(&serde_json::json!({ "toml": seed }).to_string()),
        tool_ctx,
    )
    .await;

    // When the tool loads it.
    // Then the load succeeds and the bot is dispatch-capable: not in
    // prep mode, so a tell_session dispatch would run rather than refuse.
    assert!(result.success, "load must succeed: {}", result.content);
    let guard = state.read();
    let bot = guard
        .session
        .iter()
        .map(|(_, s)| s)
        .find(|s| s.is_bot())
        .expect("a loaded bot must exist");
    assert!(!bot.bot_is_prepping(), "dispatch would be refused");
}

#[rstest::rstest]
#[test]
fn load_bot_description_teaches_the_dispatch_step() {
    // Given the tool description.
    let description = definition().description;

    // When reading it as an orchestrator would.
    // Then it names tell_session as how work starts, forbids bash-sleep
    // waiting, and offers session_fetch for progress checks.
    assert!(
        description.contains("tell_session"),
        "the dispatch step must be named"
    );
    assert!(
        description.contains("NEVER wait") && description.contains("bash sleep"),
        "the no-sleep rule must be stated"
    );
    assert!(
        description.contains("session_fetch"),
        "the progress-check path must be named"
    );
    assert!(
        description.contains("BACKGROUND"),
        "the background model must be named"
    );
}
