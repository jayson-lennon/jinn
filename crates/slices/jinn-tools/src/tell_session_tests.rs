//! Tests for the `tell_session` tool — the four delivery paths and the
//! no-silent-drop invariant.

#![allow(clippy::expect_used, reason = "test code")]

use std::time::Duration;

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_common::app_paths::AppPaths;
use jinn_core_types::SessionId;
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::tool_types::ToolCall;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::PersistSession;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use marked_path::{Absolute, MarkedPathBuf};

use crate::tell_session::{definition, execute};
use crate::tool_types::ToolContext;

/// Builds a tool call with the given JSON arguments.
fn call(arguments: &str) -> ToolCall {
    ToolCall {
        id: "tc_msg_1".into(),
        name: "tell_session".to_owned(),
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

/// Seeds a user session and a bot with a parent and a seed template.
fn fixture() -> (State, SessionId, SessionId) {
    let state = State::new(AppState::default_with_scope_focus());
    let user = ChatSessionState::new();
    let user_id = user.session_id().clone();
    let bot = ChatSessionState::new_bot(&user, true);
    let bot_id = bot.session_id().clone();
    {
        let mut guard = state.write();
        guard.session.insert(user);
        guard.session.insert(bot);
    }
    (state, user_id, bot_id)
}

/// Sets the bot's seed template so seeded runs are recognizable.
fn set_template(state: &State, bot_id: &SessionId, template: &str) {
    state
        .write()
        .session
        .get_mut(bot_id)
        .expect("bot")
        .set_seed_template(template.to_owned());
}

/// Ends prep mode so the bot can run.
fn take_out_of_prep(state: &State, bot_id: &SessionId) {
    state
        .write()
        .session
        .get_mut(bot_id)
        .expect("bot")
        .set_bot_is_prepping(false);
}

/// Drives the session into the Streaming phase, like a real turn.
fn make_busy(state: &State, session_id: &SessionId) {
    state
        .write()
        .session
        .get_mut(session_id)
        .expect("session")
        .begin_streaming();
}

#[rstest::rstest]
#[tokio::test]
async fn idle_bot_starts_a_run_seeded_with_the_message() {
    // Given an idle bot with a seed template.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    set_template(&state, &bot_id, "Review the diff. <prior_report>");
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When a message is sent to the bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"focus on main.rs"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the tool succeeds and one seeded enqueue is published.
    assert!(result.success, "delivery must succeed: {}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].session_id, bot_id);
    // And the dispatched text carries the template seed and the message, as
    // an Intersession dispatch attributed to the calling session.
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        text,
        ..
    } = &recorded[0].entry.kind
    else {
        panic!("seeded run must dispatch as an intersession entry");
    };
    assert_eq!(target_session, &bot_id);
    assert_eq!(source_session, &user_id);
    assert!(text.contains("Review the diff."), "seed present");
    assert!(text.contains("focus on main.rs"), "message present");
    // And the target was marked interacted so its run persists.
    let guard = state.read();
    assert!(guard.session.get(&bot_id).expect("bot").has_interacted());
}

#[rstest::rstest]
#[tokio::test]
async fn idle_bot_with_empty_template_dispatches_the_message_alone() {
    // Given an idle bot whose template renders to nothing.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    set_template(&state, &bot_id, "");
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"just do the thing"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the dispatched entry is the message alone, still Intersession.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("seeded run must dispatch as an intersession entry");
    };
    assert_eq!(text, "just do the thing");
}

#[rstest::rstest]
#[tokio::test]
async fn busy_bot_queues_and_never_cancels() {
    // Given a busy bot (mid-stream) with a recorder for cancel commands.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let cancels = harness
        .spawn_recorder::<jinn_inference_msg::CancelTurn>()
        .await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    set_template(&state, &bot_id, "Review the diff.");
    make_busy(&state, &bot_id);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When a message is sent to the busy bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"also check tests"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the seeded enqueue is published and no cancel is.
    assert!(result.success, "{}", result.content);
    assert!(
        result.content.contains("queued behind its current turn"),
        "busy delivery must say so: {}",
        result.content
    );
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(recorded[0].session_id, bot_id);
    // The positive signal has arrived; now the negative one is an
    // observation. The cancel recorder must have stopped changing before
    // "no cancel was published" means anything.
    let cancelled = jinn_testutil::wait::await_settled_messages(
        &cancels,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        cancelled.is_empty(),
        "delivery must never cancel in-flight work, got: {cancelled:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_session_receives_a_context_carrying_entry_from_the_caller() {
    // Given a non-bot target with no title and a distinct calling
    // session.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let caller = ChatSessionState::new();
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent to it.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"orchestrator says hello"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the enqueue is an inter-session entry addressed to that session,
    // carrying the caller as the source and the attribution prefix baked
    // into the text.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(recorded[0].session_id, user_id);
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        text,
        ..
    } = &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(source_session, &caller_id, "the caller is the source");
    assert_eq!(target_session, &user_id, "the target is addressed");
    // And the text is the attributed format, byte-exact.
    assert_eq!(
        text.as_str(),
        format!("[Message from session {caller_id}]: orchestrator says hello")
    );
    // And the entry is in LLM context by default — the model reads it.
    assert!(recorded[0].entry.is_in_context());
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_carries_a_titled_caller_in_the_prefix() {
    // Given a calling session with a title.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title("Workflow Run 42".to_owned());
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(r#"{{"session_id":"{user_id}","message":"go"}}"#)),
        tool_ctx,
    )
    .await;

    // Then the text names the source id and its title.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!("[Message from session {caller_id} (Workflow Run 42)]: go")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_excludes_an_empty_caller_title() {
    // Given a calling session whose title is the empty string.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title(String::new());
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(r#"{{"session_id":"{user_id}","message":"go"}}"#)),
        tool_ctx,
    )
    .await;

    // Then the text carries no parenthesized title.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!("[Message from session {caller_id}]: go"),
        "an empty title must be omitted, not rendered as ()"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_excludes_a_relayed_caller_title() {
    // Given a calling session whose title already looks like an attribution
    // header (it received a relayed message that minted nothing — the title
    // was set to mimic the format).
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title("[Message from session dead-beef]: stage 1".to_owned());
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent onward.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"next stage"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the text omits the title rather than nesting one header in another.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!("[Message from session {caller_id}]: next stage"),
        "a [Message-prefixed title must not nest inside the prefix"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_truncates_a_long_caller_title_with_an_ellipsis() {
    // Given a calling session whose title is 40 ASCII graphemes.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title("t".repeat(40));
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(r#"{{"session_id":"{user_id}","message":"go"}}"#)),
        tool_ctx,
    )
    .await;

    // Then the title is clipped to 32 graphemes plus one ellipsis.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!(
            "[Message from session {caller_id} ({}…)]: go",
            "t".repeat(32)
        )
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_truncation_counts_graphemes_not_bytes() {
    // Given a title where 32 grapheme clusters span more than 32 bytes
    // (family emoji are single graphemes of 8 bytes each).
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title("👨‍👩‍👧‍👦".repeat(34));
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(r#"{{"session_id":"{user_id}","message":"go"}}"#)),
        tool_ctx,
    )
    .await;

    // Then exactly 32 clusters survive, each intact — not 32 bytes, which
    // would have split mid-emoji.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    let families = "👨‍👩‍👧‍👦".repeat(32);
    assert_eq!(
        text.as_str(),
        format!("[Message from session {caller_id} ({families}…)]: go")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn plain_delivery_keeps_a_boundary_length_title_whole() {
    // Given a calling session whose title is exactly 32 graphemes.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let mut caller = ChatSessionState::new();
    caller.set_title("y".repeat(32));
    let caller_id = caller.session_id().clone();
    state.write().session.insert(caller);
    let tool_ctx = ctx(&harness, &state, caller_id.clone(), &dir.path).await;

    // When a message is sent.
    let result = execute(
        call(&format!(r#"{{"session_id":"{user_id}","message":"go"}}"#)),
        tool_ctx,
    )
    .await;

    // Then the title is not truncated and carries no ellipsis.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("plain delivery must be an Intersession entry");
    };
    assert_eq!(
        text.as_str(),
        format!(
            "[Message from session {caller_id} ({})]: go",
            "y".repeat(32)
        )
    );
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_session_id_fails_naming_the_target() {
    // Given a state with no session bearing the target id.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    let missing = SessionId::new();

    // When a message is sent to the unknown id.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{missing}","message":"anyone there?"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the tool fails naming the target and publishes nothing.
    assert!(!result.success, "unknown target must fail, not vanish");
    assert!(result.content.contains(&missing.to_string()));
    // The recorder must have stopped changing before "nothing was
    // published" is an observation rather than a race with a publish that
    // is still in flight. A fixed sleep would pass on a slow host simply
    // because the publish had not been processed yet.
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        published.is_empty(),
        "a refused send must publish nothing, got: {published:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn malformed_session_id_fails_without_publishing() {
    // Given a call whose session_id is not a UUID.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the tool runs with the malformed id.
    let result = execute(
        call(r#"{"session_id":"not-a-uuid","message":"hi"}"#),
        tool_ctx,
    )
    .await;

    // Then it fails naming the argument and publishes nothing.
    assert!(!result.success);
    assert!(result.content.contains("not-a-uuid"));
    // The recorder must have stopped changing before "nothing was
    // published" is an observation rather than a race with a publish that
    // is still in flight.
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(published.is_empty(), "a refused send must publish nothing");
}

#[rstest::rstest]
#[tokio::test]
async fn prep_mode_bot_refuses_delivery() {
    // Given a bot still being composed (prep mode is the default).
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;
    assert!(
        state
            .read()
            .session
            .get(&bot_id)
            .expect("bot")
            .bot_is_prepping(),
        "fixture must start in prep mode"
    );

    // When a message is sent to the prepping bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"wake up"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then delivery is refused with the reason and nothing is published.
    assert!(!result.success);
    assert!(result.content.contains("prep mode"));
    // The recorder must have stopped changing before "nothing was
    // published" is an observation rather than a race with a publish that
    // is still in flight.
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(published.is_empty(), "a refused send must publish nothing");
}

#[rstest::rstest]
#[tokio::test]
async fn reset_behavior_bot_persists_the_exclusions() {
    // Given an idle bot in the default Reset behavior with history.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let persists = harness.spawn_recorder::<PersistSession>().await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    {
        let mut guard = state.write();
        let bot = guard.session.get_mut(&bot_id).expect("bot");
        bot.set_bot_is_prepping(false);
        bot.push_entry(ChatEntry::user("old context"));
    }
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When a message is delivered.
    let result = execute(
        call(&format!(r#"{{"session_id":"{bot_id}","message":"again"}}"#)),
        tool_ctx,
    )
    .await;

    // Then a PersistSession for the bot rides ahead of the enqueue.
    assert!(result.success, "{}", result.content);
    let persisted = await_recorded::<PersistSession>(&persists, 1, Duration::from_secs(2)).await;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].session_id, bot_id);
    // And the non-pinned history is force-excluded.
    let guard = state.read();
    let bot = guard.session.get(&bot_id).expect("bot");
    assert!(
        bot.history().iter().all(|e| e.is_pinned()
            || e.context_override == jinn_core_types::ContextOverride::ForcedExclude)
    );
    let _ = enqueues; // the enqueue itself is covered by the idle test
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_to_busy_plain_session_steers_without_enqueueing() {
    // Given a busy non-bot target and recorders for both transports.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    make_busy(&state, &user_id);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is sent to the busy target.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"stop and read the error","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then one steering fragment is published carrying the attribution
    // prefix, and no enqueue is.
    assert!(result.success, "{}", result.content);
    let steered = await_recorded::<jinn_chat_input_msg::SubmitSteeringMessage>(
        &steers,
        1,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(steered.len(), 1);
    assert_eq!(steered[0].session_id, user_id);
    assert_eq!(
        steered[0].text,
        format!("[Message from session {user_id}]: stop and read the error")
    );
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        published.is_empty(),
        "an interrupt must not enqueue, got: {published:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_to_busy_bot_steers_without_seeding_a_run() {
    // Given a busy bot with recorders for both transports.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    make_busy(&state, &bot_id);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is sent to the busy bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"reversed the sign","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the steering fragment is published and no seeded enqueue is.
    assert!(result.success, "{}", result.content);
    let steered = await_recorded::<jinn_chat_input_msg::SubmitSteeringMessage>(
        &steers,
        1,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(steered[0].session_id, bot_id);
    assert!(steered[0].text.contains("reversed the sign"));
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        published.is_empty(),
        "an interrupt must not seed a run, got: {published:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_to_idle_bot_falls_back_to_a_seeded_run() {
    // Given an idle bot with a seed template.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    set_template(&state, &bot_id, "Review the diff.");
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is sent to the idle bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"look at tests","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the ordinary seeded-run delivery happens, not a steering
    // fragment.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(recorded[0].session_id, bot_id);
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("fallback delivery must be the ordinary seeded run");
    };
    assert!(text.contains("Review the diff."), "seed present");
    let steered = jinn_testutil::wait::await_settled_messages(
        &steers,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        steered.is_empty(),
        "an interrupt to an idle bot must not steer, got: {steered:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_to_idle_plain_session_falls_back_to_a_user_turn() {
    // Given an idle non-bot target.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is sent to the idle target.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"when free","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the ordinary inter-session user turn is delivered, not a
    // steering fragment.
    assert!(result.success, "{}", result.content);
    let recorded = await_recorded::<EnqueueUserMessage>(&enqueues, 1, Duration::from_secs(2)).await;
    assert_eq!(recorded[0].session_id, user_id);
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } =
        &recorded[0].entry.kind
    else {
        panic!("fallback delivery must be the ordinary user turn");
    };
    assert_eq!(
        text.as_str(),
        format!("[Message from session {user_id}]: when free")
    );
    let steered = jinn_testutil::wait::await_settled_messages(
        &steers,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        steered.is_empty(),
        "an interrupt to an idle session must not steer, got: {steered:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_to_prep_mode_bot_is_refused() {
    // Given a bot still in prep mode.
    let harness = TestHarness::new().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is sent to the prepping bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"wake up","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the refusal carries the usual reason and nothing is published.
    assert!(!result.success);
    assert!(result.content.contains("prep mode"));
    let steered = jinn_testutil::wait::await_settled_messages(
        &steers,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(steered.is_empty(), "a refused send must publish nothing");
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_mode_fails_naming_the_valid_values() {
    // Given a live target and a call whose mode is a typo.
    let harness = TestHarness::new().await;
    let enqueues = harness.spawn_recorder::<EnqueueUserMessage>().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When the call runs with the unrecognized mode.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"go","mode":"interupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the tool fails naming both valid values and publishes nothing.
    assert!(!result.success, "a typo'd mode must fail, not default");
    assert!(result.content.contains("\"queue\""), "{}", result.content);
    assert!(
        result.content.contains("\"interrupt\""),
        "{}",
        result.content
    );
    let published = jinn_testutil::wait::await_settled_messages(
        &enqueues,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(published.is_empty(), "a failed mode must publish nothing");
    let steered = jinn_testutil::wait::await_settled_messages(
        &steers,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(steered.is_empty(), "a failed mode must publish nothing");
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_delivery_never_cancels_the_in_flight_turn() {
    // Given a busy bot with a recorder for cancel commands.
    let harness = TestHarness::new().await;
    let cancels = harness
        .spawn_recorder::<jinn_inference_msg::CancelTurn>()
        .await;
    let (state, user_id, bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    take_out_of_prep(&state, &bot_id);
    make_busy(&state, &bot_id);
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is delivered to the busy bot.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{bot_id}","message":"course correct","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the delivery succeeds and no cancel command is published.
    assert!(result.success, "{}", result.content);
    let cancelled = jinn_testutil::wait::await_settled_messages(
        &cancels,
        Duration::from_millis(100),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        cancelled.is_empty(),
        "an interrupt must never cancel in-flight work, got: {cancelled:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn interrupt_delivery_marks_the_target_interacted() {
    // Given a busy plain-session target that has not been interacted with.
    let harness = TestHarness::new().await;
    let steers = harness
        .spawn_recorder::<jinn_chat_input_msg::SubmitSteeringMessage>()
        .await;
    let (state, user_id, _bot_id) = fixture();
    let dir = jinn_testutil::tempdir();
    make_busy(&state, &user_id);
    assert!(
        !state
            .read()
            .session
            .get(&user_id)
            .expect("target")
            .has_interacted(),
        "fixture must start un-interacted"
    );
    let tool_ctx = ctx(&harness, &state, user_id.clone(), &dir.path).await;

    // When an interrupt is delivered to it.
    let result = execute(
        call(&format!(
            r#"{{"session_id":"{user_id}","message":"see this","mode":"interrupt"}}"#
        )),
        tool_ctx,
    )
    .await;

    // Then the target is marked interacted so its turn can persist.
    assert!(result.success, "{}", result.content);
    let _ = await_recorded::<jinn_chat_input_msg::SubmitSteeringMessage>(
        &steers,
        1,
        Duration::from_secs(2),
    )
    .await;
    let guard = state.read();
    assert!(
        guard
            .session
            .get(&user_id)
            .expect("target")
            .has_interacted(),
        "an interrupt must mark the target interacted"
    );
}

#[rstest::rstest]
#[test]
fn definition_requires_both_arguments_and_names_the_tool() {
    // Given the tool definition.
    let def = definition();

    // When its parameters are read.
    // Then it is named tell_session with both parameters required.
    assert_eq!(def.name, "tell_session");
    let required = def.parameters["required"].as_array().expect("required");
    assert_eq!(required.len(), 2);
}

#[rstest::rstest]
#[test]
fn definition_advertises_an_optional_mode_with_both_values() {
    // Given the tool definition.
    let def = definition();

    // When its mode property is read.
    let mode = &def.parameters["properties"]["mode"];
    let enum_values: Vec<&str> = mode["enum"]
        .as_array()
        .expect("mode enum")
        .iter()
        .map(|v| v.as_str().expect("enum entries are strings"))
        .collect();

    // Then it is an optional string enum of exactly queue and interrupt,
    // described as defaulting to queue.
    assert_eq!(mode["type"], "string");
    assert_eq!(enum_values, ["queue", "interrupt"]);
    assert!(
        !def.parameters["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|v| v == "mode"),
        "mode must not be required"
    );
    assert!(
        mode["description"]
            .as_str()
            .expect("mode description")
            .contains("(the default)"),
        "the description must say queue is the default"
    );
}
