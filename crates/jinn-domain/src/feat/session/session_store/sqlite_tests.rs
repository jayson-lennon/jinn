#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::session_store::SessionStore;
use crate::feat::session::session_store::sqlite::SqliteSessionStore;
use crate::protocol::ToolResultStatus;
use crate::protocol::{ChatEntry, ChatEntryKind, EntryTiming, SessionId};
use tempfile::TempDir;

/// Creates a minimal `ChatSessionState` for testing.
fn make_session(id: &SessionId, title: &str) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    session.set_session_id(id.clone());
    session.set_title(title.to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session
}

async fn make_store() -> (TempDir, SqliteSessionStore) {
    let dir = TempDir::new().expect("temp dir");
    let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
    (dir, store)
}

#[rstest::rstest]
#[tokio::test]
async fn save_creates_summary() {
    // Given a SqliteSessionStore in a temp directory.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let session = make_session(&session_id, "Test Session");

    // When saving and loading summaries.
    store.save(&session).await.expect("save");
    let summaries = store.load_summaries().await.expect("load_summaries");

    // Then one summary is returned.
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].session_id, session_id);
    assert_eq!(summaries[0].title, "Test Session");
}

#[rstest::rstest]
#[tokio::test]
async fn load_session_restores_data() {
    // Given a SqliteSessionStore in a temp directory.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let session = make_session(&session_id, "Test Session");

    // When saving and loading the session.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load_session")
        .expect("should have a session");

    // Then the session data matches.
    assert_eq!(loaded.session_id(), &session_id);
    assert_eq!(loaded.title(), Some("Test Session"));
    assert_eq!(loaded.history().len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn degraded_token_expanded_survives_save_and_reload() {
    // Given a session with a resolved degraded user entry (marker set, expanded literal).
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let token = "@/nonexistent/whatever";
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    let mut entry = ChatEntry::user(format!("describe {token}"));
    // Simulate the post-resolution state: outcome set, expanded still containing the literal.
    if let crate::protocol::ChatEntryKind::User { outcome, .. } = &mut entry.kind {
        outcome
            .degraded
            .push(crate::protocol::ResolvedToken {
                raw: "/nonexistent/whatever".to_owned(),
                abs: std::path::PathBuf::from("/nonexistent/whatever"),
            });
    }
    session.push_entry(entry);

    // When saving and reloading the session.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load_session")
        .expect("should have a session");

    // Then the AI-facing expanded text keeps the literal token (no file:// revert).
    let expanded = loaded
        .history()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            ChatEntryKind::User { expanded, .. } => Some(expanded.clone()),
            _ => None,
        })
        .expect("user entry");
    assert!(
        expanded.contains(token),
        "reloaded expanded must keep literal token: {expanded}"
    );
    assert!(
        !expanded.contains("file://"),
        "reloaded expanded must not contain file://: {expanded}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn attachment_outcome_survives_save_and_reload() {
    // Given a resolved user entry carrying BOTH an attached and a degraded token.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    let mut entry = ChatEntry::user("see @real.png and @whatever");
    if let ChatEntryKind::User { outcome, .. } = &mut entry.kind {
        outcome
            .attached
            .push(crate::protocol::ResolvedToken {
                raw: "real.png".to_owned(),
                abs: std::path::PathBuf::from("/abs/real.png"),
            });
        outcome
            .degraded
            .push(crate::protocol::ResolvedToken {
                raw: "whatever".to_owned(),
                abs: std::path::PathBuf::from("/abs/whatever"),
            });
    }
    session.push_entry(entry);

    // When saving and reloading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load_session")
        .expect("should have a session");

    // Then the per-token outcome marker round-trips through SQLite — both the
    // attached and degraded tokens survive, so the render can color them after
    // reload (closing the masked-marker gap).
    let outcome = loaded
        .history()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            ChatEntryKind::User { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("user entry");
    assert_eq!(outcome.attached.len(), 1);
    assert_eq!(outcome.attached[0].raw, "real.png");
    assert_eq!(outcome.degraded.len(), 1);
    assert_eq!(outcome.degraded[0].raw, "whatever");
}

#[rstest::rstest]
#[tokio::test]
async fn summaries_returns_correct_count() {
    // Given a store with 2 sessions.
    let (_dir, store) = make_store().await;
    let id_a = SessionId::new();
    let id_b = SessionId::new();

    store.save(&make_session(&id_a, "A")).await.expect("save A");
    store.save(&make_session(&id_b, "B")).await.expect("save B");

    // When loading summaries.
    let summaries = store.load_summaries().await.expect("load_summaries");

    // Then 2 summaries are returned.
    assert_eq!(summaries.len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn save_updates_existing_session() {
    // Given a store with a saved session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_session(&session_id, "v1"))
        .await
        .expect("save v1");

    // When saving again with updated title.
    let mut updated = make_session(&session_id, "v2");
    updated.push_entry(ChatEntry::assistant("world"));
    store.save(&updated).await.expect("save v2");

    // Then the summary reflects v2.
    let summaries = store.load_summaries().await.expect("load_summaries");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].title, "v2");

    // And the loaded session has both entries.
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load_session")
        .expect("should exist");
    assert_eq!(loaded.history().len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn load_session_returns_none_for_unknown_id() {
    // Given an empty store.
    let (_dir, store) = make_store().await;

    // When loading a nonexistent session.
    let result = store
        .load_session(&SessionId::new())
        .await
        .expect("load_session");

    // Then None is returned.
    assert!(result.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn load_summaries_returns_empty_when_no_sessions() {
    // Given a fresh store.
    let (_dir, store) = make_store().await;

    // When loading summaries.
    let summaries = store.load_summaries().await.expect("load_summaries");

    // Then an empty vec is returned.
    assert!(summaries.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn save_creates_directory() {
    // Given a SqliteSessionStore pointed at a non-existent directory.
    let dir = TempDir::new().expect("temp dir");
    let nested = dir.path().join("does").join("not").join("exist");
    let store = SqliteSessionStore::new_in(&nested).await.expect("store");
    let session = make_session(&SessionId::new(), "Mkdir Test");

    // When saving.
    store.save(&session).await.expect("save");

    // Then the directory is created.
    assert!(nested.exists());
}

#[rstest::rstest]
#[tokio::test]
async fn delete_removes_session() {
    // Given a store with a saved session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_session(&session_id, "To Delete"))
        .await
        .expect("save");

    // When deleting.
    store.delete(&session_id).await.expect("delete");

    // Then the session is gone.
    let result = store.load_session(&session_id).await.expect("load_session");
    assert!(result.is_none());

    // And summaries are empty.
    let summaries = store.load_summaries().await.expect("load_summaries");
    assert!(summaries.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn delete_is_noop_for_unknown_id() {
    // Given a store.
    let (_dir, store) = make_store().await;

    // When deleting a nonexistent session.
    store.delete(&SessionId::new()).await.expect("delete");

    // Then no error occurs.
}

#[rstest::rstest]
#[tokio::test]
async fn fork_creates_new_session_with_entries_up_to_ordinal() {
    // Given a store with a session that has 3 entries.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Original".to_owned());
    source.push_entry(ChatEntry::user("first"));
    source.push_entry(ChatEntry::assistant("second"));
    source.push_entry(ChatEntry::user("third"));
    store.save(&source).await.expect("save source");

    // When forking at ordinal 1 (includes entries 0 and 1).
    let forked_id = store.fork(&source_id, 1).await.expect("fork");

    // Then the forked session has 2 entries.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(forked.history().len(), 2);

    // And the entries match the first two of the source.
    match &forked.history()[0].kind {
        ChatEntryKind::User { display, .. } => assert_eq!(display, "first"),
        other => panic!("expected User, got {other:?}"),
    }
    match &forked.history()[1].kind {
        ChatEntryKind::Assistant(t) => assert_eq!(t, "second"),
        other => panic!("expected Assistant, got {other:?}"),
    }

    // And the forked session has the source as parent.
    assert_eq!(forked.parent_session(), &Some(source_id.clone()));
}

#[rstest::rstest]
#[tokio::test]
async fn fork_does_not_modify_source() {
    // Given a store with a session that has 3 entries.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Original".to_owned());
    source.push_entry(ChatEntry::user("a"));
    source.push_entry(ChatEntry::assistant("b"));
    source.push_entry(ChatEntry::user("c"));
    store.save(&source).await.expect("save source");

    // When forking at ordinal 1.
    store.fork(&source_id, 1).await.expect("fork");

    // Then the source session is unchanged.
    let reloaded = store
        .load_session(&source_id)
        .await
        .expect("load source")
        .expect("should exist");
    assert_eq!(reloaded.history().len(), 3);
    assert_eq!(reloaded.title(), Some("Original"));
}

#[rstest::rstest]
#[tokio::test]
async fn fork_shares_entry_data_not_junction_rows() {
    // Given a store with a saved session.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Source".to_owned());
    source.push_entry(ChatEntry::user("shared entry"));
    store.save(&source).await.expect("save source");

    // When forking at ordinal 0.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then both sessions reference the same entry (same entry_id).
    let source = store
        .load_session(&source_id)
        .await
        .expect("load source")
        .expect("should exist");
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");

    assert_eq!(source.history()[0].id, forked.history()[0].id);
}

#[rstest::rstest]
#[tokio::test]
async fn fork_returns_error_for_unknown_source() {
    // Given a store.
    let (_dir, store) = make_store().await;

    // When forking from a nonexistent source.
    let result = store.fork(&SessionId::new(), 0).await;

    // Then an error is returned.
    assert!(result.is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn all_entry_kinds_round_trip() {
    // Given a session with every entry kind.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("All Kinds".to_owned());

    session.push_entry(ChatEntry::user("user msg"));
    session.push_entry(ChatEntry::system("system msg"));
    session.push_entry(ChatEntry::error("error msg"));
    session.push_entry(ChatEntry::assistant("assistant msg"));
    session.push_entry(ChatEntry::actor("bash", "actor msg"));
    session.push_entry(ChatEntry::thinking("thinking text"));
    session.push_entry(ChatEntry::tool_call("call_1", "bash", "{\"cmd\": true}"));
    session.push_entry(ChatEntry::tool_result(
        "call_1",
        "bash",
        "ok",
        ToolResultStatus::Success,
    ));

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then all entry kinds are preserved.
    assert_eq!(loaded.history().len(), 8);
    assert!(
        matches!(&loaded.history()[0].kind, ChatEntryKind::User { display, .. } if display == "user msg")
    );
    assert!(matches!(&loaded.history()[1].kind, ChatEntryKind::System(t) if t == "system msg"));
    assert!(matches!(&loaded.history()[2].kind, ChatEntryKind::Error(t) if t == "error msg"));
    assert!(
        matches!(&loaded.history()[3].kind, ChatEntryKind::Assistant(t) if t == "assistant msg")
    );
    assert!(
        matches!(&loaded.history()[4].kind, ChatEntryKind::Actor { source, text } if source == "bash" && text == "actor msg")
    );
    assert!(
        matches!(&loaded.history()[5].kind, ChatEntryKind::Thinking(t) if t == "thinking text")
    );
    assert!(
        matches!(&loaded.history()[6].kind, ChatEntryKind::ToolCall { id, name, arguments, .. } if id == "call_1" && name == "bash" && arguments == "{\"cmd\": true}")
    );
    assert!(
        matches!(&loaded.history()[7].kind, ChatEntryKind::ToolResult { id, name, content, status, .. } if id == "call_1" && name == "bash" && content == "ok" && *status == ToolResultStatus::Success)
    );
}

#[rstest::rstest]
#[tokio::test]
async fn pin_position_round_trips() {
    // Given a session with pinned entries.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Pins".to_owned());

    session.push_entry(ChatEntry::user("pinned top").with_pin(crate::protocol::PinPosition::Top));
    session.push_entry(
        ChatEntry::assistant("pinned bottom").with_pin(crate::protocol::PinPosition::Bottom),
    );
    session.push_entry(
        ChatEntry::user("pinned relative").with_pin(crate::protocol::PinPosition::Relative),
    );
    session.push_entry(ChatEntry::user("unpinned"));

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then pin positions are preserved.
    assert_eq!(
        loaded.history()[0].pin_position,
        Some(crate::protocol::PinPosition::Top)
    );
    assert_eq!(
        loaded.history()[1].pin_position,
        Some(crate::protocol::PinPosition::Bottom)
    );
    assert_eq!(
        loaded.history()[2].pin_position,
        Some(crate::protocol::PinPosition::Relative)
    );
    assert_eq!(loaded.history()[3].pin_position, None);
}

#[rstest::rstest]
#[tokio::test]
async fn token_ledger_round_trips() {
    // Given a session with token records.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Tokens".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.push_token_record(crate::feat::session::token_stats::TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 100,
        tokens_received: 50,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the token ledger is preserved.
    assert_eq!(loaded.token_ledger().len(), 1);
    assert_eq!(loaded.token_ledger()[0].tokens_sent, 100);
    assert_eq!(loaded.token_ledger()[0].tokens_received, 50);
}

#[rstest::rstest]
#[tokio::test]
async fn token_ledger_round_trips_prompt_and_cached_tokens() {
    // Given a session with token records carrying provider-reported prompt
    // and cached token counts, plus one record with both as None.
    use crate::feat::session::token_stats::TokenRecord;
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Cache tokens".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.push_token_record(TokenRecord {
        model_used: Some("openrouter/auto".to_owned()),
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 50,
        cost: Some(0.01),
        prompt_tokens: Some(1000),
        cached_tokens: Some(400),
    });
    session.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 200,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the provider-reported counts round-trip exactly.
    let ledger = loaded.token_ledger();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0].prompt_tokens, Some(1000));
    assert_eq!(ledger[0].cached_tokens, Some(400));
    // And None round-trips as None.
    assert_eq!(ledger[1].prompt_tokens, None);
    assert_eq!(ledger[1].cached_tokens, None);
}

#[rstest::rstest]
#[tokio::test]
async fn delete_cleans_up_orphaned_entries() {
    // Given two sessions sharing entries via fork.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Source".to_owned());
    source.push_entry(ChatEntry::user("shared"));
    store.save(&source).await.expect("save source");

    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // When deleting the forked session.
    store.delete(&forked_id).await.expect("delete forked");

    // Then the source session still has its entry.
    let source = store
        .load_session(&source_id)
        .await
        .expect("load source")
        .expect("should exist");
    assert_eq!(source.history().len(), 1);

    // When also deleting the source.
    store.delete(&source_id).await.expect("delete source");

    // Then the entry is fully cleaned up (verified by saving the same
    // entry ID again - should work since it was deleted).
    let summaries = store.load_summaries().await.expect("load_summaries");
    assert!(summaries.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn cwd_round_trips_through_save_and_load() {
    // Given a store with a session that has a custom cwd.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("CWD Test".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.set_cwd(std::path::PathBuf::from("/tmp/my-project"));

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the cwd is preserved.
    assert_eq!(loaded.cwd(), std::path::Path::new("/tmp/my-project"));
}

#[rstest::rstest]
#[tokio::test]
async fn fork_inherits_cwd_from_source() {
    // Given a store with a session that has a custom cwd.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Original".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    source.set_cwd(std::path::PathBuf::from("/home/user/project"));
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then the forked session inherits the source cwd.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(forked.cwd(), std::path::Path::new("/home/user/project"));
}

#[rstest::rstest]
#[tokio::test]
async fn fork_strips_suppressed_task_tool() {
    // Given a store with a source session whose disabled tools include task.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Subagent".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    {
        let profile = source.profile_mut();
        profile
            .disabled_tools
            .insert(jinn_tools_msg::TASK_TOOL_NAME.to_owned());
    }
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then the forked session has the task tool enabled again.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert!(
        !forked
            .profile()
            .disabled_tools
            .contains(jinn_tools_msg::TASK_TOOL_NAME),
        "a fork must not inherit the task suppression stamp, got: {:?}",
        forked.profile().disabled_tools
    );
}

#[rstest::rstest]
#[tokio::test]
async fn fork_preserves_other_disabled_tools() {
    // Given a store with a source session disabling write (not task).
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Manual disable".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    source.set_disabled_tools(std::collections::HashSet::from(["write".to_owned()]));
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then the forked session still has write disabled — the strip is
    // targeted at task, not a wipe of the disabled set.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert!(
        forked.profile().disabled_tools.contains("write"),
        "fork must preserve non-task disabled tools, got: {:?}",
        forked.profile().disabled_tools
    );
    // And it does not gain a task entry of its own.
    assert!(
        !forked
            .profile()
            .disabled_tools
            .contains(jinn_tools_msg::TASK_TOOL_NAME),
        "fork must not gain a task disable, got: {:?}",
        forked.profile().disabled_tools
    );
}

#[rstest::rstest]
#[tokio::test]
async fn project_round_trips_through_save_and_load() {
    // Given a store with a session that has a project stamp.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Project Test".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the project is preserved.
    assert_eq!(
        loaded.project(),
        Some(std::path::Path::new("/home/user/projects/jinn")),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn project_defaults_to_none_for_legacy_rows() {
    // Given a store with a session saved with no project (the pre-project
    // creation shape).
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let session = make_session(&session_id, "Legacy Session");

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the project defaults to None (blank column).
    assert_eq!(loaded.project(), None);
}

#[rstest::rstest]
#[tokio::test]
async fn fork_inherits_project_from_source() {
    // Given a store with a session that has a project stamp.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Original".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    source.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then the forked session inherits the source project.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(
        forked.project(),
        Some(std::path::Path::new("/home/user/projects/jinn")),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn set_cwd_does_not_alter_project() {
    // Given a session with a project stamp.
    let mut session = ChatSessionState::new();
    session.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));

    // When changing the session's cwd.
    session.set_cwd(std::path::PathBuf::from("/somewhere/else/entirely"));

    // Then the project stamp is unchanged.
    assert_eq!(
        session.project(),
        Some(std::path::Path::new("/home/user/projects/jinn")),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn summary_from_store_carries_project() {
    // Given a store with a session that has a project stamp.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Summary Project".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));
    store.save(&session).await.expect("save");

    // When loading summaries.
    let summaries = store.load_summaries().await.expect("load_summaries");

    // Then the summary carries the project from the metadata blob.
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].project,
        Some(std::path::PathBuf::from("/home/user/projects/jinn")),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn save_updates_cwd_on_existing_session() {
    // Given a store with a saved session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("CWD Update".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.set_cwd(std::path::PathBuf::from("/old/path"));
    store.save(&session).await.expect("save v1");

    // When saving with an updated cwd.
    session.set_cwd(std::path::PathBuf::from("/new/path"));
    store.save(&session).await.expect("save v2");

    // Then the loaded session has the new cwd.
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");
    assert_eq!(loaded.cwd(), std::path::Path::new("/new/path"));
}

#[rstest::rstest]
#[tokio::test]
async fn ignored_field_round_trips() {
    // Given a session with ignored entries.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Ignored".to_owned());

    session.push_entry(ChatEntry::user("normal"));
    session.push_entry(ChatEntry::assistant("response"));
    session.push_entry(ChatEntry::user("ignored message"));
    session.push_entry(ChatEntry::assistant("ignored response"));

    // When marking entries 2,3 as ignored.
    session.mark_entries_ignored(&[2, 3]);

    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then ignored flags are preserved after round-trip.
    assert!(
        !loaded.history()[0].ignored(),
        "entry 0 should not be ignored"
    );
    assert!(
        !loaded.history()[1].ignored(),
        "entry 1 should not be ignored"
    );
    assert!(loaded.history()[2].ignored(), "entry 2 should be ignored");
    assert!(loaded.history()[3].ignored(), "entry 3 should be ignored");
}

#[rstest::rstest]
#[tokio::test]
async fn lifecycle_metadata_round_trips() {
    // Given a store with a session that has lifecycle metadata.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = make_session(&session_id, "Lifecycle Session");
    session.set_lifecycle_name(Some("fossil branch".to_owned()));
    session.set_lifecycle_args(vec!["my-branch".to_owned(), "--private".to_owned()]);

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then lifecycle metadata is preserved.
    assert_eq!(loaded.lifecycle_name(), Some("fossil branch"));
    assert_eq!(
        loaded.lifecycle_args(),
        &["my-branch".to_owned(), "--private".to_owned()]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn session_without_lifecycle_loads_as_none() {
    // Given a store with a session that has no lifecycle metadata.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let session = make_session(&session_id, "Plain Session");

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then lifecycle fields are None/empty.
    assert_eq!(loaded.lifecycle_name(), None);
    assert!(loaded.lifecycle_args().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn fork_inherits_lifecycle_metadata() {
    // Given a store with a session that has lifecycle metadata.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Source".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    source.set_lifecycle_name(Some("fossil branch".to_owned()));
    source.set_lifecycle_args(vec!["dev".to_owned()]);
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");

    // Then the forked session inherits lifecycle metadata.
    assert_eq!(forked.lifecycle_name(), Some("fossil branch"));
    assert_eq!(forked.lifecycle_args(), &["dev".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn lifecycle_script_state_setup_ran_round_trips() {
    // Given a session with SetupRan.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Lifecycle State".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.advance_lifecycle_after_setup();

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then lifecycle_script_state is SetupRan.
    assert_eq!(
        loaded.lifecycle_script_state(),
        crate::feat::session::chat_session::LifecycleScriptState::SetupRan
    );
}

#[rstest::rstest]
#[tokio::test]
async fn lifecycle_script_state_nothing_ran_round_trips() {
    // Given a session with NothingRan (default).
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let session = make_session(&session_id, "Default State");

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then lifecycle_script_state is NothingRan.
    assert_eq!(
        loaded.lifecycle_script_state(),
        crate::feat::session::chat_session::LifecycleScriptState::NothingRan
    );
}

#[rstest::rstest]
#[tokio::test]
async fn fork_inherits_lifecycle_script_state() {
    // Given a store with a session that has SetupRan.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.set_title("Source".to_owned());
    source.push_entry(ChatEntry::user("hello"));
    source.advance_lifecycle_after_setup();
    store.save(&source).await.expect("save source");

    // When forking.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");

    // Then the forked session inherits SetupRan.
    assert_eq!(
        forked.lifecycle_script_state(),
        crate::feat::session::chat_session::LifecycleScriptState::SetupRan
    );
}

#[rstest::rstest]
#[tokio::test]
async fn non_persistent_session_is_not_written() {
    // Given a transient (persist=false) session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Transient".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.core.persist = false;

    // When saving.
    store
        .save(&session)
        .await
        .expect("save should be a no-op, not an error");

    // Then no row exists for this session.
    let loaded = store.load_session(&session_id).await.expect("load query");
    assert!(
        loaded.is_none(),
        "persist=false session must not be written"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persistent_session_is_written() {
    // Given a persistent (persist=true) session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Persistent".to_owned());
    session.push_entry(ChatEntry::user("hello"));
    session.core.persist = true;

    // When saving.
    store.save(&session).await.expect("save");

    // Then the row exists.
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load query")
        .expect("should exist");
    assert_eq!(loaded.session_id(), &session_id);
    // And the persist flag round-trips through SQLite.
    assert!(loaded.core.persist, "persist must survive save/load");
}

#[rstest::rstest]
#[tokio::test]
async fn streamed_timing_roundtrips_through_db() {
    // Given a persisted session with an entry that has Streamed timing.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = make_session(&session_id, "Timing test");
    session.core.persist = true;

    let dispatched = jiff::Timestamp::now();
    let mut timing = EntryTiming::streamed(dispatched);
    timing.set_first_token();
    timing.finish();

    let mut entry = ChatEntry::user("hello");
    entry.timing = timing;
    session.push_entry(entry);

    // When saving and loading.
    store.save(&session).await.expect("save");

    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the Streamed timing is preserved with all timestamps.
    let loaded_entry = &loaded.history()[1];
    match &loaded_entry.timing {
        EntryTiming::Streamed {
            dispatched_at,
            first_token_at,
            finished_at,
        } => {
            assert_eq!(
                *dispatched_at, dispatched,
                "dispatched_at should round-trip"
            );
            assert!(first_token_at.is_some(), "first_token_at should be Some");
            assert!(finished_at.is_some(), "finished_at should be Some");
        }
        other => panic!("expected Streamed timing, got {other:?}"),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn fork_ordinal_persists_across_save_and_load() {
    // Given a store with a session that has 3 entries.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.push_entry(ChatEntry::user("first"));
    source.push_entry(ChatEntry::assistant("second"));
    source.push_entry(ChatEntry::user("third"));
    store.save(&source).await.expect("save source");

    // When forking at ordinal 1.
    let forked_id = store.fork(&source_id, 1).await.expect("fork");

    // Then the forked session has fork_ordinal = Some(1) after loading.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(forked.fork_ordinal(), Some(1));
}

#[rstest::rstest]
#[tokio::test]
async fn fork_blocking_sets_fork_ordinal() {
    // Given a store with a session that has 5 entries.
    let (_dir, store) = make_store().await;
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new();
    source.set_session_id(source_id.clone());
    source.push_entry(ChatEntry::user("a"));
    source.push_entry(ChatEntry::assistant("b"));
    source.push_entry(ChatEntry::user("c"));
    source.push_entry(ChatEntry::assistant("d"));
    source.push_entry(ChatEntry::user("e"));
    store.save(&source).await.expect("save source");

    // When forking at ordinal 4 (all entries inherited).
    let forked_id = store.fork(&source_id, 4).await.expect("fork");

    // Then the forked session has fork_ordinal = Some(4).
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(forked.fork_ordinal(), Some(4));

    // And the root session has fork_ordinal = None.
    let root = store
        .load_session(&source_id)
        .await
        .expect("load root")
        .expect("should exist");
    assert_eq!(root.fork_ordinal(), None);
}

use crate::feat::session::session_store::migrator::seed_at_version;
use jinn_core_types::model_selection::ModelSelection;
use rusqlite::params;

/// A metadata blob in the 0.65 shape: `profile.model` is a bare string.
///
/// Constructed by hand (not via `PersistableCore`) so it carries the legacy
/// serialization that v19 must repair.
const LEGACY_065_BLOB: &str = "{\"session_id\":\"10000000-0000-0000-0000-000000000065\",\"title\":\"Legacy 065\",\"profile\":{\"strategy\":\"sliding_window\",\"model\":\"ollama/llama3\",\"persona_name\":\"coding-assistant\",\"token_budget\":150000,\"sliding_window_size\":5},\"cwd\":\".\",\"parent_session\":null,\"blobs\":{},\"lifecycle_name\":null,\"lifecycle_args\":[],\"lifecycle_script_state\":\"nothing_ran\",\"session_state\":\"Loaded\",\"created_at\":\"2024-01-01T00:00:00Z\",\"updated_at\":\"2024-01-01T00:00:00Z\",\"is_automated\":false,\"persist\":true}";

/// A 0.66-shape metadata blob for the automated row (`is_automated: true`
/// as written by the removed workflow feature).
fn legacy_automated_blob() -> String {
    LEGACY_065_BLOB
        .replace(
            "\"10000000-0000-0000-0000-000000000065\"",
            "\"10000000-0000-0000-0000-000000000099\"",
        )
        .replace("\"Legacy 065\"", "\"Automated Legacy\"")
        .replace("\"is_automated\":false", "\"is_automated\":true")
}

/// A row recorded by the removed workflow feature with `is_automated = 1`.
/// After the automation removal, the flag no longer exists in code: the row
/// must load as a perfectly ordinary session.
#[rstest::rstest]
#[tokio::test]
async fn legacy_automated_row_loads_as_normal_session() {
    // Given a database holding a session row flagged is_automated = 1.
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    seed_at_version(db_path.to_string_lossy().as_ref(), 18, |conn| {
        conn.execute(
            "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, \
             lifecycle_script_state, is_automated, persist, metadata) \
             VALUES ('10000000-0000-0000-0000-000000000099', 'Automated Legacy', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
             '{\"model\":{\"single\":\"ollama/llama3\"}}', '{}', 'nothing_ran', 1, 1, ?)",
            params![legacy_automated_blob()],
        ).map(|_| ())
    })
    .await;

    // When loading the session through the store.
    let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
    let loaded = store
        .load_session(&SessionId::from(
            "10000000-0000-0000-0000-000000000099".to_owned(),
        ))
        .await
        .expect("load_session")
        .expect("session should exist");

    // Then it loads as an ordinary session: not a child, in the Loaded state.
    assert_eq!(
        loaded.parent_session(),
        &None,
        "flag row has no parent link"
    );
    assert_eq!(
        loaded.session_state(),
        crate::feat::session::chat_session::SessionState::Loaded,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn legacy_065_blob_loads_after_v19() {
    // Given a 0.65 database (recorded at v18) with a legacy-shape metadata blob.
    // v19 has not yet run, so profile.model is still a bare string.
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    seed_at_version(db_path.to_string_lossy().as_ref(), 18, |conn| {
        conn.execute(
            "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, \
             lifecycle_script_state, is_automated, persist, metadata) \
             VALUES ('10000000-0000-0000-0000-000000000065', 'Legacy 065', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
             '{\"model\":\"ollama/llama3\"}', '{}', 'nothing_ran', 0, 0, ?)",
            params![LEGACY_065_BLOB],
        ).map(|_| ())
    })
    .await;

    // When loading the session through the store.
    // (The store re-runs migrations on open; v19 repairs the blob, v20 drops zombies.)
    let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
    let loaded = store
        .load_session(&SessionId::from(
            "10000000-0000-0000-0000-000000000065".to_owned(),
        ))
        .await
        .expect("load_session")
        .expect("session should exist");

    // Then the profile model is restored as Single(...) - not dropped or errored.
    assert_eq!(
        loaded.model_selection(),
        &ModelSelection::Single("ollama/llama3".to_owned()),
        "0.65 bare-string model must load as Single after v19"
    );

    // And other profile fields round-trip correctly.
    assert_eq!(loaded.persona_name(), "coding-assistant");
}

/// A metadata blob in the 0.66 shape: `profile.model` is already `{"single": ...}`.
const CURRENT_066_BLOB: &str = "{\"session_id\":\"10000000-0000-0000-0000-000000000066\",\"title\":\"Current 066\",\"profile\":{\"strategy\":\"sliding_window\",\"model\":{\"single\":\"ollama/llama3\"},\"persona_name\":\"coding-assistant\",\"token_budget\":150000,\"sliding_window_size\":5},\"cwd\":\".\",\"parent_session\":null,\"blobs\":{},\"lifecycle_name\":null,\"lifecycle_args\":[],\"lifecycle_script_state\":\"nothing_ran\",\"session_state\":\"Loaded\",\"created_at\":\"2024-01-01T00:00:00Z\",\"updated_at\":\"2024-01-01T00:00:00Z\",\"is_automated\":false,\"persist\":true}";

#[rstest::rstest]
#[tokio::test]
async fn current_066_blob_loads_unchanged() {
    // Given a fresh database with a 0.66-shape blob inserted directly.
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    seed_at_version(db_path.to_string_lossy().as_ref(), 18, |conn| {
        conn.execute(
            "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, \
             lifecycle_script_state, is_automated, persist, metadata) \
             VALUES ('10000000-0000-0000-0000-000000000066', 'Current 066', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
             '{\"model\":{\"single\":\"ollama/llama3\"}}', '{}', 'nothing_ran', 0, 0, ?)",
            params![CURRENT_066_BLOB],
        ).map(|_| ())
    })
    .await;

    // When loading the session through the store.
    let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
    let loaded = store
        .load_session(&SessionId::from(
            "10000000-0000-0000-0000-000000000066".to_owned(),
        ))
        .await
        .expect("load_session")
        .expect("session should exist");

    // Then the profile model loads correctly - no regression on current sessions.
    assert_eq!(
        loaded.model_selection(),
        &ModelSelection::Single("ollama/llama3".to_owned())
    );
    assert_eq!(loaded.persona_name(), "coding-assistant");
}

/// A pre-v8 session (no metadata blob) must still load after v20 backfills its
/// metadata from the zombie columns. The legacy column-read path is gone (v20
/// dropped those columns), so loading succeeds via the backfilled blob.
#[rstest::rstest]
#[tokio::test]
async fn legacy_pre_v8_row_loads_after_v20_backfill() {
    // Given a fresh database at v18 with a session row whose metadata is NULL
    // (pre-v8 shape). The profile column carries the post-v17 form.
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sessions.db");
    seed_at_version(db_path.to_string_lossy().as_ref(), 18, |conn| {
        conn.execute(
            "INSERT INTO sessions (id, title, updated_at, created_at, cwd, profile, blobs, \
             lifecycle_script_state, is_automated, persist) \
             VALUES ('10000000-0000-0000-0000-000000000008', 'Legacy Pre-V8', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '.', \
             '{\"model\":{\"single\":\"ollama/llama3\"},\"persona_name\":\"coding-assistant\"}', '{}', 'nothing_ran', 0, 0)",
            params![],
        ).map(|_| ())
    })
    .await;

    // When loading the session through the store (which runs v19 + v20 on open).
    let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
    let loaded = store
        .load_session(&SessionId::from(
            "10000000-0000-0000-0000-000000000008".to_owned(),
        ))
        .await
        .expect("load_session")
        .expect("session should exist");

    // Then v20 backfilled the metadata from the profile column, and the blob
    // path restores model + persona.
    assert_eq!(
        loaded.model_selection(),
        &ModelSelection::Single("ollama/llama3".to_owned()),
        "pre-v8 row must load its model after v20 backfill"
    );
    assert_eq!(
        loaded.persona_name(),
        "coding-assistant",
        "pre-v8 row must load persona after v20 backfill"
    );
}
/// After v20, the `sessions` table has exactly the 9 authoritative columns —
/// the zombie columns (`profile`, `blobs`, `cwd`, `lifecycle_script_state`,
/// `lifecycle_args`) are gone. A saved session's real data lives entirely
/// in the `metadata` blob.
#[rstest::rstest]
#[tokio::test]
async fn sessions_table_has_exactly_nine_columns() {
    // Given a saved session.
    let (dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = make_session(&session_id, "Schema Check");
    session.set_model(ModelSelection::Single("anthropic/claude-opus-4".to_owned()));
    store.save(&session).await.expect("save");

    // When listing the sessions table columns.
    let db_path = dir.path().join("sessions.db");
    let pool = daow::Pool::open(db_path.to_string_lossy().as_ref()).expect("open");
    let cols: Vec<ColumnRow> = pool
        .query_all::<ColumnRow>("PRAGMA table_info(sessions)", vec![])
        .await
        .expect("table_info");

    // Then exactly the 9 authoritative columns exist (no zombies).
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "id",
            "title",
            "updated_at",
            "created_at",
            "parent_session",
            "archived",
            "metadata",
            "is_automated",
            "persist"
        ],
        "sessions table must have exactly the 9 authoritative columns post-v20"
    );

    // And metadata carries the real model.
    let row: Option<MetadataRow> = pool
        .query_one(
            "SELECT metadata FROM sessions WHERE id = ?",
            vec![Box::new(session_id.to_string())],
        )
        .await
        .expect("select metadata");
    let metadata = row
        .expect("row exists")
        .metadata
        .expect("metadata non-null");
    assert!(
        metadata.contains("\"anthropic/claude-opus-4\""),
        "metadata blob should contain the real model: {metadata}"
    );
}

#[derive(Debug)]
struct ColumnRow {
    name: String,
}
impl daow::FromRow for ColumnRow {
    fn from_row(row: &daow::Row) -> daow::Result<Self> {
        Ok(Self {
            name: row.get("name")?,
        })
    }
}

#[derive(Debug)]
struct MetadataRow {
    metadata: Option<String>,
}
impl daow::FromRow for MetadataRow {
    fn from_row(row: &daow::Row) -> daow::Result<Self> {
        Ok(Self {
            metadata: row.get("metadata")?,
        })
    }
}

#[rstest::rstest]
#[tokio::test]
async fn user_entry_with_image_attachment_roundtrips_through_sqlite() {
    // Given a session with a user entry referencing a PNG on disk via @path.
    let (dir, store) = make_store().await;
    let session_id = SessionId::new();
    let png_path = dir.path().join("img.png");
    std::fs::write(&png_path, TINY_PNG).expect("write png");
    let display = format!("describe this @{}", png_path.to_string_lossy());
    let expanded = format!("describe this (file://{})", png_path.to_string_lossy());
    let mut entry = ChatEntry::user_expanded(display, expanded);
    // Populate the attachment directly — this test verifies SQLite persistence
    // of attachments, not the @path expansion pipeline (which resolves in the
    // session actor).
    if let ChatEntryKind::User { attachments, .. } = &mut entry.kind {
        attachments.push(jinn_provider::Attachment::image(
            "image/png".to_owned(),
            TINY_PNG.to_vec(),
        ));
    }
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Image Session".to_owned());
    session.push_entry(entry);

    // When saving and reloading the session.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load_session")
        .expect("session should exist");

    // Then the reloaded user entry carries the image attachment read from disk.
    let entry = &loaded.history()[0];
    let ChatEntryKind::User {
        attachments,
        expanded,
        ..
    } = &entry.kind
    else {
        panic!("expected a User entry");
    };
    assert_eq!(attachments.len(), 1);
    assert!(attachments[0].is_image());
    assert_eq!(attachments[0].data(), TINY_PNG);
    assert!(
        expanded.contains(&format!("(file://{})", png_path.to_string_lossy())),
        "expanded text should contain the file:// URI: {expanded}"
    );
}

/// Minimal valid PNG (1×1) magic bytes for attachment roundtrip tests.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89,
];

#[rstest::rstest]
#[tokio::test]
async fn legacy_blob_without_origin_loads_as_user() {
    // Given a subagent-origin session serialized to the persisted blob shape.
    let parent_id = SessionId::new();
    let mut session = ChatSessionState::new_child(&parent_id, true);
    let session_id = SessionId::new();
    session.set_session_id(session_id.clone());
    let blob = serde_json::to_string(
        &crate::feat::session::session_store::sqlite::PersistableCore::from(&session.core),
    )
    .expect("serialize");

    // When stripping the `origin` key (simulating a blob written before the
    // field existed) and deserializing back.
    let mut value: serde_json::Value = serde_json::from_str(&blob).expect("parse");
    value
        .as_object_mut()
        .expect("blob is an object")
        .remove("origin");
    let stripped = serde_json::to_string(&value).expect("re-serialize");
    let persistable: crate::feat::session::session_store::sqlite::PersistableCore =
        serde_json::from_str(&stripped).expect("deserialize legacy blob");
    let core = crate::feat::session::chat_session::SessionCore::from(persistable);

    // Then the legacy blob loads as User.
    assert_eq!(
        core.origin,
        crate::feat::session::chat_session::SessionOrigin::User
    );
}

#[rstest::rstest]
#[tokio::test]
async fn legacy_blob_without_project_defaults_to_none() {
    // Given a project-stamped session serialized to the persisted blob shape.
    let mut session = ChatSessionState::new();
    session.set_project(Some(std::path::PathBuf::from("/home/user/projects/jinn")));
    let blob = serde_json::to_string(
        &crate::feat::session::session_store::sqlite::PersistableCore::from(&session.core),
    )
    .expect("serialize");

    // When stripping the `project` key (simulating a blob written before the
    // field existed) and deserializing back.
    let mut value: serde_json::Value = serde_json::from_str(&blob).expect("parse");
    value
        .as_object_mut()
        .expect("blob is an object")
        .remove("project");
    let stripped = serde_json::to_string(&value).expect("re-serialize");
    let persistable: crate::feat::session::session_store::sqlite::PersistableCore =
        serde_json::from_str(&stripped).expect("deserialize legacy blob");
    let core = crate::feat::session::chat_session::SessionCore::from(persistable);

    // Then the legacy blob loads with no project (blank column).
    assert_eq!(core.project, None);
}

#[rstest::rstest]
#[tokio::test]
async fn subagent_origin_roundtrips_through_store() {
    // Given a store with a task-tool child session.
    let (_dir, store) = make_store().await;
    let parent_id = SessionId::new();
    let session_id = SessionId::new();
    let mut child = ChatSessionState::new_child(&parent_id, true);
    child.set_session_id(session_id.clone());
    child.set_title("subagent".to_owned());
    child.push_entry(ChatEntry::user("hello"));
    store.save(&child).await.expect("save");

    // When loading it back.
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then the subagent origin survives persistence.
    assert_eq!(
        loaded.origin(),
        crate::feat::session::chat_session::SessionOrigin::Subagent
    );
}

#[rstest::rstest]
#[tokio::test]
async fn forked_session_persists_fork_origin() {
    // Given a store with a subagent-origin session that has 2 entries.
    let (_dir, store) = make_store().await;
    let parent_id = SessionId::new();
    let source_id = SessionId::new();
    let mut source = ChatSessionState::new_child(&parent_id, true);
    source.set_session_id(source_id.clone());
    source.set_title("subagent".to_owned());
    source.push_entry(ChatEntry::user("first"));
    source.push_entry(ChatEntry::assistant("second"));
    store.save(&source).await.expect("save source");

    // When forking it at ordinal 0.
    let forked_id = store.fork(&source_id, 0).await.expect("fork");

    // Then the fork loads with Fork origin — even though its source was a
    // subagent.
    let forked = store
        .load_session(&forked_id)
        .await
        .expect("load forked")
        .expect("should exist");
    assert_eq!(
        forked.origin(),
        crate::feat::session::chat_session::SessionOrigin::Fork
    );
    // And the fork still carries the parent link.
    assert_eq!(forked.parent_session(), &Some(source_id.clone()));
}

#[rstest::rstest]
#[tokio::test]
async fn set_archived_many_round_trips_subset() {
    // Given a store with three saved sessions.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    let b = SessionId::new();
    let c = SessionId::new();
    for (id, title) in [(&a, "a"), (&b, "b"), (&c, "c")] {
        store.save(&make_session(id, title)).await.expect("save");
    }

    // When archiving a subset (a and c) in one call.
    store
        .set_archived_many(&[a.clone(), c.clone()], true)
        .await
        .expect("set_archived_many");

    // Then exactly a and c are archived.
    let summaries = store.load_summaries().await.expect("summaries");
    let is_archived = |id: &SessionId| {
        summaries
            .iter()
            .find(|s| s.session_id == *id)
            .expect("summary")
            .session_state
            == crate::feat::session::chat_session::SessionState::Archived
    };
    assert!(is_archived(&a));
    assert!(!is_archived(&b));
    assert!(is_archived(&c));
}

#[rstest::rstest]
#[tokio::test]
async fn set_archived_many_with_empty_slice_is_noop() {
    // Given a store with one saved session.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    store.save(&make_session(&a, "a")).await.expect("save");

    // When calling set_archived_many with an empty slice.
    store
        .set_archived_many(&[], true)
        .await
        .expect("set_archived_many");

    // Then the session is untouched (still loaded).
    let summaries = store.load_summaries().await.expect("summaries");
    assert_eq!(
        summaries[0].session_state,
        crate::feat::session::chat_session::SessionState::Loaded
    );
}

#[rstest::rstest]
#[tokio::test]
async fn set_archived_many_false_un_archives() {
    // Given a store with two sessions archived via set_archived_many.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    let b = SessionId::new();
    store.save(&make_session(&a, "a")).await.expect("save");
    store.save(&make_session(&b, "b")).await.expect("save");
    store
        .set_archived_many(&[a.clone(), b.clone()], true)
        .await
        .expect("archive");

    // When un-archiving them with archived = false.
    store
        .set_archived_many(&[a.clone(), b.clone()], false)
        .await
        .expect("un-archive");

    // Then both sessions are loaded again.
    let summaries = store.load_summaries().await.expect("summaries");
    assert!(
        summaries.iter().all(|s| {
            s.session_state == crate::feat::session::chat_session::SessionState::Loaded
        })
    );
}

#[rstest::rstest]
#[tokio::test]
async fn set_archived_many_ignores_unknown_ids() {
    // Given a store with one saved session.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    store.save(&make_session(&a, "a")).await.expect("save");

    // When archiving a slice containing the session and an unknown ID.
    store
        .set_archived_many(&[a.clone(), SessionId::new()], true)
        .await
        .expect("set_archived_many");

    // Then the call succeeds and the known session is archived.
    let summaries = store.load_summaries().await.expect("summaries");
    assert_eq!(
        summaries[0].session_state,
        crate::feat::session::chat_session::SessionState::Archived
    );
}

#[rstest::rstest]
#[tokio::test]
async fn token_count_round_trips_through_store() {
    // Given a session with entries carrying mixed token counts.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("Token Counts".to_owned());

    session.push_entry(ChatEntry::user("counted"));
    session.push_entry(ChatEntry::assistant("not counted yet"));

    let mut counted = ChatEntry::user("pre-computed");
    counted.token_count = Some(1234);
    session.push_entry(counted);

    // When saving and loading.
    store.save(&session).await.expect("save");
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");

    // Then persisted counts are restored and missing counts stay None.
    assert_eq!(loaded.history().len(), 3);
    assert_eq!(loaded.history()[0].token_count, None);
    assert_eq!(loaded.history()[1].token_count, None);
    assert_eq!(loaded.history()[2].token_count, Some(1234));
}

#[rstest::rstest]
#[tokio::test]
async fn saving_entry_with_uncomputed_count_preserves_persisted_count() {
    // Given a shared entry whose count another session already persisted.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    let shared = ChatEntry::user("shared entry");
    let shared_id = shared.id.clone();
    session.push_entry(shared);
    store.save(&session).await.expect("first save");

    // (Seed a persisted count directly, as if another session's save had.)
    store
        .pool()
        .execute(
            "UPDATE entries SET token_count = 77 WHERE id = ?",
            vec![Box::new(shared_id.to_string())],
        )
        .await
        .expect("seed persisted count");

    // When saving a second session sharing the same entry, but with the
    // count not yet computed in memory.
    let other_id = SessionId::new();
    let mut other = ChatSessionState::new();
    other.set_session_id(other_id.clone());
    other.push_entry({
        let mut clone = ChatEntry::user("shared entry");
        clone.id = shared_id.clone();
        clone
    });
    store.save(&other).await.expect("second save");

    // Then the previously persisted count is not clobbered by the NULL.
    let loaded = store
        .load_session(&session_id)
        .await
        .expect("load")
        .expect("should exist");
    assert_eq!(loaded.history()[0].token_count, Some(77));
}

// ── FTS search index (schema v26) ────────────────────────────────────────

use crate::feat::session_search::SearchableRole;

/// A session with a user entry and an assistant entry, both mentioning the
/// needle word used across the search tests.
fn make_two_entry_session(id: &SessionId, title: &str) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    session.set_session_id(id.clone());
    session.set_title(title.to_owned());
    session.push_entry(ChatEntry::user("the zephyr needle sails home"));
    session.push_entry(ChatEntry::assistant("and the zephyr needle docks"));
    session
}

/// Drains the dirty set using the store primitives — the same chunked,
/// resumable loop the search-index actor runs, with a chunk size large
/// enough to finish each test session in one pass. Returns the number of
/// chunks run.
///
/// Store tests only exercise the store layer; batch isolation and progress
/// reporting belong to the actor and are covered there.
async fn drain(
    store: &SqliteSessionStore,
) -> Result<usize, error_stack::Report<crate::feat::session::session_store::SessionStoreError>> {
    // Oversized chunks: store tests exercise single-shot rebuilds; chunked
    // resume semantics belong to the actor tests. Repeats while partial
    // chunks remain, counting sessions that reached their final chunk.
    let mut reindexed = 0;
    for _ in 0..100 {
        let ids = store.dirty_session_ids().await?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            if store.reindex_session_chunk(id, 10_000).await? {
                reindexed += 1;
            }
        }
    }
    Ok(reindexed)
}

#[rstest::rstest]
#[tokio::test]
async fn save_marks_session_dirty_and_reindex_indexes_it() {
    // Given a store with a saved session mentioning a needle word.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "sailing"))
        .await
        .expect("save");

    // When reindexing the dirty set and searching for the needle.
    let reindexed = drain(&store).await.expect("reindex");
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then the session was reindexed and its user+assistant entries match.
    assert_eq!(reindexed, 1);
    assert_eq!(outcome.total_matches, 2);
    assert_eq!(outcome.hits.len(), 2);
    // And the marker is cleared: a second drain finds nothing to do.
    let second = drain(&store).await.expect("reindex 2");
    assert_eq!(second, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn reindex_honors_default_field_visibility_via_roles() {
    // Given an indexed session with a tool_result entry.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = make_two_entry_session(&session_id, "tools");
    session.push_entry(ChatEntry::tool_result(
        "t1",
        "grep",
        "needle found in config.toml",
        ToolResultStatus::Success,
    ));
    store.save(&session).await.expect("save");
    drain(&store).await.expect("reindex");

    // When searching without a role filter.
    let all = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: vec![session_id.to_string()],
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");
    drain(&store).await.expect("reindex");
    let tool_only = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: vec![session_id.to_string()],
            roles: vec![SearchableRole::ToolResult],
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then the unfiltered search sees all three entries.
    assert!(all.total_matches >= 3);
    // And the role-filtered search sees only the tool_result.
    assert_eq!(tool_only.total_matches, 1);
    assert_eq!(tool_only.hits[0].role, "tool_result");
}

#[rstest::rstest]
#[tokio::test]
async fn reindex_clears_rows_of_deleted_sessions() {
    // Given a store with an indexed session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "doomed"))
        .await
        .expect("save");
    drain(&store).await.expect("reindex");

    // When deleting the session, marking it dirty, and reindexing.
    store.delete(&session_id).await.expect("delete");
    let reindexed = drain(&store).await.expect("reindex");

    // Then the deletion marked it dirty, and its rows are gone.
    assert_eq!(reindexed, 1);
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(outcome.total_matches, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn search_reports_per_session_rollup() {
    // Given two indexed sessions both matching the needle.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    let b = SessionId::new();
    store
        .save(&make_two_entry_session(&a, "alpha"))
        .await
        .expect("save a");
    store
        .save(&make_two_entry_session(&b, "beta"))
        .await
        .expect("save b");
    drain(&store).await.expect("reindex");

    // When searching without scope restriction.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 50,
        })
        .await
        .expect("search");

    // Then both sessions appear in the rollup with their counts.
    assert_eq!(outcome.total_matches, 4);
    let mut counted: Vec<(String, u64)> = outcome.per_session.clone();
    counted.sort();
    let mut expected: Vec<(String, u64)> = vec![(a.to_string(), 2), (b.to_string(), 2)];
    expected.sort();
    assert_eq!(counted, expected);
}

#[rstest::rstest]
#[tokio::test]
async fn search_limits_hits_but_reports_full_totals() {
    // Given two matching entries in one session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "capped"))
        .await
        .expect("save");
    drain(&store).await.expect("reindex");

    // When searching with a limit of 1.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 1,
        })
        .await
        .expect("search");

    // Then only one hit is returned but the total is complete.
    assert_eq!(outcome.hits.len(), 1);
    assert_eq!(outcome.total_matches, 2);
    assert_eq!(outcome.per_session, vec![(session_id.to_string(), 2)]);
}

#[rstest::rstest]
#[tokio::test]
async fn search_filters_by_session_ids() {
    // Given two indexed sessions.
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    let b = SessionId::new();
    store
        .save(&make_two_entry_session(&a, "alpha"))
        .await
        .expect("save a");
    store
        .save(&make_two_entry_session(&b, "beta"))
        .await
        .expect("save b");
    drain(&store).await.expect("reindex");

    // When searching restricted to session b.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: vec![b.to_string()],
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then only session b's entries match.
    assert_eq!(outcome.total_matches, 2);
    assert!(outcome.hits.iter().all(|h| h.session_id == b.to_string()));
}

#[rstest::rstest]
#[tokio::test]
async fn search_surfaces_fts_syntax_errors_verbatim() {
    // Given an indexed store.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "syntax"))
        .await
        .expect("save");
    drain(&store).await.expect("reindex");

    // When running a syntactically invalid MATCH query.
    let result = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle AND".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await;

    // Then the error carries the fts5 message text.
    let report = result.expect_err("invalid query must fail");
    let rendered = format!("{report:?}");
    assert!(
        rendered.contains("fts5"),
        "expected verbatim fts5 error, got: {rendered}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn search_dates_filter_on_entry_timestamps() {
    // Given an indexed session whose entries are all from now.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "dated"))
        .await
        .expect("save");
    drain(&store).await.expect("reindex");

    let until_long_ago = jiff::Timestamp::now() - jiff::Span::new().hours(25);

    // When searching with an `until` bound in the past.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: Some(until_long_ago),
            limit: 10,
        })
        .await
        .expect("search");

    // Then nothing matches.
    assert_eq!(outcome.total_matches, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn search_snippets_are_single_line_with_match_markers() {
    // Given an indexed session with a multi-line assistant entry.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("snippets".to_owned());
    session.push_entry(ChatEntry::assistant("first line\nneedle on its\nown line"));
    store.save(&session).await.expect("save");
    drain(&store).await.expect("reindex");

    // When searching for the needle.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then the snippet is a single line with <<>> markers around the match.
    assert_eq!(outcome.hits.len(), 1);
    let snippet = &outcome.hits[0].snippet;
    assert!(
        !snippet.contains('\n'),
        "snippet must be one line: {snippet:?}"
    );
    assert!(
        snippet.contains("<<needle>>"),
        "marker missing: {snippet:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn search_flags_ignored_entries_as_excluded() {
    // Given an indexed session where one entry is user-force-excluded.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("excluded".to_owned());
    session.push_entry(ChatEntry::user("in context needle"));
    session.push_entry(
        ChatEntry::user("dropped needle")
            .with_context_override(crate::protocol::ContextOverride::ForcedExclude),
    );
    store.save(&session).await.expect("save");
    drain(&store).await.expect("reindex");

    // When searching.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then both entries match but only the excluded one is flagged.
    assert_eq!(outcome.total_matches, 2);
    let mut flagged = 0;
    for hit in &outcome.hits {
        if hit.excluded {
            flagged += 1;
            assert!(hit.snippet.contains("dropped"), "wrong entry flagged");
        }
    }
    assert_eq!(flagged, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn search_does_not_flag_pinned_entries_as_excluded() {
    // Given an indexed session with a pinned (thus in-context) entry.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("pinned".to_owned());
    session
        .push_entry(ChatEntry::user("pinned needle").with_pin(crate::protocol::PinPosition::Top));
    store.save(&session).await.expect("save");
    drain(&store).await.expect("reindex");

    // When searching.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");

    // Then the pinned entry matches without the excluded flag.
    assert_eq!(outcome.total_matches, 1);
    assert!(!outcome.hits[0].excluded);
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_window_returns_entries_around_anchor() {
    // Given an indexed session with five entries.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("window".to_owned());
    for i in 0..5 {
        session.push_entry(ChatEntry::user(format!("entry {i}")));
    }
    store.save(&session).await.expect("save");

    // When fetching a window of 3 entries anchored at the middle entry.
    let anchor = session.history()[2].id.clone();
    let window = store
        .fetch_window(&session_id, &anchor, 3)
        .await
        .expect("fetch_window")
        .expect("session exists");

    // Then the window has 3 entries starting at the anchor's neighborhood,
    // with live ordinals and total count.
    assert_eq!(window.total_entries, 5);
    assert_eq!(window.entries.len(), 3);
    let ordinals: Vec<usize> = window.entries.iter().map(|e| e.ordinal).collect();
    assert_eq!(ordinals, vec![1, 2, 3]);
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_window_clamps_at_session_start() {
    // Given an indexed session with five entries.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("clamp".to_owned());
    for i in 0..5 {
        session.push_entry(ChatEntry::user(format!("entry {i}")));
    }
    store.save(&session).await.expect("save");

    // When fetching a window anchored at the first entry.
    let anchor = session.history()[0].id.clone();
    let window = store
        .fetch_window(&session_id, &anchor, 3)
        .await
        .expect("fetch_window")
        .expect("session exists");

    // Then the window is clamped to the start (no negative ordinals).
    let ordinals: Vec<usize> = window.entries.iter().map(|e| e.ordinal).collect();
    assert_eq!(ordinals, vec![0, 1, 2]);
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_window_errors_for_anchor_in_wrong_session() {
    // Given an indexed session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("solo".to_owned());
    session.push_entry(ChatEntry::user("entry here"));
    store.save(&session).await.expect("save");

    // When fetching a window anchored at an entry from another session.
    let stranger = ChatEntry::user("stranger");
    let result = store.fetch_window(&session_id, &stranger.id, 3).await;

    // Then the call fails with a legible error.
    assert!(result.is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_window_returns_none_for_missing_session() {
    // Given a store with no such session.
    let (_dir, store) = make_store().await;

    // When fetching by an unknown session id.
    let anchor = ChatEntry::user("x");
    let window = store
        .fetch_window(&SessionId::new(), &anchor.id, 3)
        .await
        .expect("fetch_window");

    // Then no window is returned.
    assert!(window.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_tail_returns_last_entries() {
    // Given an indexed session with five entries.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("tail".to_owned());
    for i in 0..5 {
        session.push_entry(ChatEntry::user(format!("entry {i}")));
    }
    store.save(&session).await.expect("save");

    // When fetching the tail with a limit of 2.
    let window = store
        .fetch_tail(&session_id, 2)
        .await
        .expect("fetch_tail")
        .expect("session exists");

    // Then the last two entries are returned with live ordinals.
    assert_eq!(window.total_entries, 5);
    let ordinals: Vec<usize> = window.entries.iter().map(|e| e.ordinal).collect();
    assert_eq!(ordinals, vec![3, 4]);
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_tail_marks_excluded_entries() {
    // Given a session whose last entry is user-force-excluded.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(session_id.clone());
    session.set_title("flagged".to_owned());
    session.push_entry(ChatEntry::user("kept"));
    session.push_entry(
        ChatEntry::assistant("dropped")
            .with_context_override(crate::protocol::ContextOverride::ForcedExclude),
    );
    store.save(&session).await.expect("save");

    // When fetching the tail.
    let window = store
        .fetch_tail(&session_id, 10)
        .await
        .expect("fetch_tail")
        .expect("session exists");

    // Then the excluded flag is true only for the dropped entry.
    assert_eq!(window.entries.len(), 2);
    assert!(!window.entries[0].excluded);
    assert!(window.entries[1].excluded);
}

#[rstest::rstest]
#[tokio::test]
async fn dirty_session_ids_returns_saved_sessions() {
    // Given a store with two saved sessions (saves seed dirty markers).
    let (_dir, store) = make_store().await;
    let a = SessionId::new();
    let b = SessionId::new();
    store
        .save(&make_two_entry_session(&a, "a"))
        .await
        .expect("save");
    store
        .save(&make_two_entry_session(&b, "b"))
        .await
        .expect("save");

    // When reading the dirty ids.
    let mut ids = store.dirty_session_ids().await.expect("dirty ids");
    let mut expected = vec![a, b];
    ids.sort();
    expected.sort();

    // Then both saved sessions are pending.
    assert_eq!(ids, expected);
}

#[rstest::rstest]
#[tokio::test]
async fn pending_dirty_count_tracks_markers() {
    // Given a store with one saved session.
    let (_dir, store) = make_store().await;
    let id = SessionId::new();
    store
        .save(&make_two_entry_session(&id, "counted"))
        .await
        .expect("save");

    // When reading the pending count before and after a drain.
    let before = store.pending_dirty_count().await.expect("count");
    drain(&store).await.expect("drain");
    let after = store.pending_dirty_count().await.expect("count");

    // Then the save left one pending session and the drain cleared it.
    assert_eq!(before, 1);
    assert_eq!(after, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn dirty_session_ids_skips_unparseable_marker() {
    // Given a store whose dirty table holds a corrupt (unparseable) id.
    let (_dir, store) = make_store().await;
    store
        .pool()
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO fts_dirty(session_id) VALUES ('not-a-uuid')",
                [],
            )
            .map_err(daow::Error::from)
        })
        .await
        .expect("insert corrupt marker");

    // When reading the dirty ids.
    let ids = store.dirty_session_ids().await.expect("dirty ids");

    // Then the corrupt marker is not returned (it could never be reindexed).
    assert!(ids.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn pending_dirty_count_includes_unparseable_marker() {
    // Given a store whose dirty table holds a corrupt (unparseable) id.
    let (_dir, store) = make_store().await;
    store
        .pool()
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO fts_dirty(session_id) VALUES ('not-a-uuid')",
                [],
            )
            .map_err(daow::Error::from)
        })
        .await
        .expect("insert corrupt marker");

    // When reading the pending count.
    let count = store.pending_dirty_count().await.expect("count");

    // Then the corrupt marker is counted — the queue size reflects reality so
    // the dashboard can surface the stuck row instead of hiding it.
    assert_eq!(count, 1);
}
#[rstest::rstest]
#[tokio::test]
async fn partial_chunk_persists_resume_point_and_next_chunk_finishes() {
    // Given a store with a 6-entry session and chunks of 2.
    let (_dir, store) = make_store().await;
    let id = SessionId::new();
    let mut session = ChatSessionState::new();
    session.set_session_id(id.clone());
    session.set_title("chunked".to_owned());
    for i in 0..6 {
        session.push_entry(ChatEntry::user(format!("entry {i} mentions needle")));
    }
    store.save(&session).await.expect("save");

    // When the first bounded chunk runs.
    let finished = store.reindex_session_chunk(&id, 2).await.expect("chunk 1");

    // Then the session is not finished, its marker stays, and the resume
    // point advanced to 2 — so search finds only the prefix.
    assert!(!finished);
    assert_eq!(store.pending_dirty_count().await.expect("count"), 1);
    let first = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(first.total_matches, 2, "only the indexed prefix is visible");

    // When the remaining chunks run (2+2+2 = three exactly-full chunks; the
    // final full chunk is followed by an empty one that reports completion).
    assert!(!store.reindex_session_chunk(&id, 2).await.expect("chunk 2"));
    assert!(!store.reindex_session_chunk(&id, 2).await.expect("chunk 3"));
    let finished = store.reindex_session_chunk(&id, 2).await.expect("chunk 4");

    // Then the rebuild completes, the marker clears, and all six entries
    // are searchable.
    assert!(finished);
    assert_eq!(store.pending_dirty_count().await.expect("count"), 0);
    let all = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: Vec::new(),
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(all.total_matches, 6);
}

#[rstest::rstest]
#[tokio::test]
async fn reindex_chunk_on_deleted_session_clears_marker() {
    // Given a dirty marker for a session with no rows (deleted since marked).
    let (_dir, store) = make_store().await;
    let id = SessionId::new();
    let marker = id.to_string();
    store
        .pool()
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO fts_dirty(session_id) VALUES (?)",
                rusqlite::params![marker],
            )
            .map_err(daow::Error::from)
        })
        .await
        .expect("mark dirty");

    // When a chunk runs for it.
    let finished = store.reindex_session_chunk(&id, 100).await.expect("chunk");

    // Then the rebuild is trivially finished and the marker cleared.
    assert!(finished);
    assert_eq!(store.pending_dirty_count().await.expect("count"), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn reindex_maps_every_fts_row_in_the_rowid_side_table() {
    // Given a store with an indexed two-entry session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "mapped"))
        .await
        .expect("save");
    drain(&store).await.expect("reindex");

    // When counting FTS rows and map rows for the session.
    let (fts_count, map_count) = fts_and_map_counts(store.pool(), &session_id.to_string()).await;

    // Then the map is exactly as large as the index it mirrors.
    assert!(fts_count > 0, "precondition: the session was indexed");
    assert_eq!(
        map_count, fts_count,
        "one fts_rowids row per session_fts row"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn reindexed_rebuild_replaces_rows_via_rowid_map() {
    // Given an indexed session.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    store
        .save(&make_two_entry_session(&session_id, "rebuild"))
        .await
        .expect("save");
    drain(&store).await.expect("initial reindex");
    let (initial_fts, initial_map) =
        fts_and_map_counts(store.pool(), &session_id.to_string()).await;

    // When the session is saved again (re-marked dirty: a rebuild-from-zero)
    // and reindexed.
    store
        .save(&make_two_entry_session(&session_id, "rebuild"))
        .await
        .expect("save again");
    drain(&store).await.expect("rebuild reindex");

    // Then no old row survives the rebuild — every pre-rebuild FTS rowid was
    // deleted, not re-inserted on top of — and the map is consistent.
    let (after_fts, after_map) = fts_and_map_counts(store.pool(), &session_id.to_string()).await;
    assert_eq!(after_fts, initial_fts, "same entry count after rebuild");
    assert_eq!(after_map, initial_map, "map tracks the rebuilt rows");
    let orphan_map_rows: i64 = {
        let sid = session_id.to_string();
        store
            .pool()
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM fts_rowids \
                     WHERE session_id = ? AND fts_rowid NOT IN \
                     (SELECT rowid FROM session_fts)",
                    rusqlite::params![sid],
                    |row| row.get(0),
                )
                .map_err(daow::Error::from)
            })
            .await
            .expect("orphan check")
    };
    assert_eq!(orphan_map_rows, 0, "no map row points at a dead FTS rowid");
    // And search still finds the session after the map-mediated rebuild.
    let outcome = store
        .search(crate::feat::session_search::SearchParams {
            query: "needle".to_owned(),
            session_ids: vec![session_id.to_string()],
            roles: Vec::new(),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .expect("search after rebuild");
    assert_eq!(outcome.total_matches, 2, "search works after rebuild");
}

#[rstest::rstest]
#[tokio::test]
async fn reindex_of_never_indexed_session_skips_delete_with_empty_map() {
    // Given a dirty marker for a session that has never been indexed
    // (so fts_rowids has no rows for it) and one saved entry.
    let (_dir, store) = make_store().await;
    let session_id = SessionId::new();
    let mut session = make_two_entry_session(&session_id, "fresh");
    // Save the session first so the marker exists with entries attached,
    // then clear its FTS presence to simulate a never-indexed session.
    store.save(&session).await.expect("save");
    drain(&store).await.expect("reindex");
    store
        .pool()
        .with_conn({
            let marker = session_id.to_string();
            move |conn| {
                conn.execute_batch(&format!(
                    "DELETE FROM session_fts WHERE session_id = '{marker}'; \
                     DELETE FROM fts_rowids WHERE session_id = '{marker}';"
                ))
                .map_err(daow::Error::from)
            }
        })
        .await
        .expect("strip index and map");
    // Re-mark the session dirty so a rebuild runs over the empty map.
    store
        .pool()
        .with_conn({
            let marker = session_id.to_string();
            move |conn| {
                conn.execute(
                    "INSERT INTO fts_dirty(session_id) VALUES (?)",
                    rusqlite::params![marker],
                )
                .map_err(daow::Error::from)
            }
        })
        .await
        .expect("re-mark dirty");
    session.set_session_id(session_id.clone());

    // When reindexing the session.
    let finished = store
        .reindex_session_chunk(&session_id, 10_000)
        .await
        .expect("chunk");

    // Then the rebuild succeeds (the empty-map delete was skipped, not
    // attempted as a full scan) and the session is fully indexed again.
    assert!(finished, "single oversized chunk finishes the session");
    let (fts_count, map_count) = fts_and_map_counts(store.pool(), &session_id.to_string()).await;
    assert_eq!(fts_count, 2, "both entries indexed");
    assert_eq!(map_count, 2, "map re-populated");
}

/// Returns the number of `session_fts` and `fts_rowids` rows for `session_id`.
async fn fts_and_map_counts(pool: &daow::Pool, session_id: &str) -> (i64, i64) {
    let sid = session_id.to_owned();
    pool.with_conn(move |conn| {
        let fts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_fts WHERE session_id = ?",
            rusqlite::params![sid],
            |row| row.get(0),
        )?;
        let map: i64 = conn.query_row(
            "SELECT COUNT(*) FROM fts_rowids WHERE session_id = ?",
            rusqlite::params![sid],
            |row| row.get(0),
        )?;
        Ok((fts, map))
    })
    .await
    .expect("fts/map counts")
}
