//! Tests for the `session_fetch` tool.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::Mutex;

use error_stack::Report;

use crate::session_fetch::{definition, execute};
use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::feat::session::chat_session::ChatSessionState;
use jinn_domain::feat::session::session_store::{
    SessionStore, SessionStoreError, SessionStoreService,
};
use jinn_domain::feat::session_search::{TranscriptEntry, TranscriptWindow};
use jinn_domain::protocol::{ChatEntry, ChatEntryId, SessionId};

/// A stub store serving one canned transcript window, recording the last
/// read it was asked for.
#[derive(Debug, Default)]
struct StubStore {
    window: Mutex<Option<TranscriptWindow>>,
    last_read: Mutex<Option<String>>,
}

impl StubStore {
    fn with_window(window: TranscriptWindow) -> Self {
        Self {
            window: Mutex::new(Some(window)),
            last_read: Mutex::new(None),
        }
    }

    fn last_read(&self) -> Option<String> {
        self.last_read.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SessionStore for StubStore {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn save(&self, _session: &ChatSessionState) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_domain::feat::session::session_summary::SessionSummary>,
        Report<SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<ChatSessionState>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn delete(&self, _session_id: &SessionId) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &SessionId,
        _at_ordinal: usize,
    ) -> Result<SessionId, Report<SessionStoreError>> {
        Ok(SessionId::new())
    }

    async fn set_archived(
        &self,
        _session_id: &SessionId,
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[SessionId],
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<
        Vec<jinn_domain::feat::session::session_summary::SessionSummary>,
        Report<SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn shutdown(&self) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &SessionId,
        _max_entries: usize,
    ) -> Result<bool, Report<SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>> {
        Ok(0)
    }

    async fn search(
        &self,
        _params: jinn_domain::feat::session_search::SearchParams,
    ) -> Result<jinn_domain::feat::session_search::SearchOutcome, Report<SessionStoreError>> {
        Ok(jinn_domain::feat::session_search::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &SessionId,
        _anchor: &ChatEntryId,
        context: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        *self.last_read.lock().unwrap() = Some(format!("window:{context}"));
        Ok(self.window.lock().unwrap().clone())
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        limit: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        *self.last_read.lock().unwrap() = Some(format!("tail:{limit}"));
        Ok(self.window.lock().unwrap().clone())
    }
}

fn entry(ordinal: usize, kind: ChatEntry) -> TranscriptEntry {
    TranscriptEntry {
        ordinal,
        entry: kind,
        excluded: false,
    }
}

fn window(entries: Vec<TranscriptEntry>) -> TranscriptWindow {
    TranscriptWindow {
        session_id: "0199aaaa-0000-7000-8000-000000000009".to_owned(),
        title: Some("migrate auth flow".to_owned()),
        total_entries: 1_204,
        entries,
    }
}

fn ctx_with(store: StubStore) -> (ToolContext, std::sync::Arc<StubStore>) {
    let arc = std::sync::Arc::new(store);
    let ctx = ToolContext {
        cwd: std::path::PathBuf::from("/tmp"),
        command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
        timeout: None,
        state: None,
        session_id: Some(SessionId::from(
            "0199aaaa-0000-7000-8000-000000000001".to_owned(),
        )),
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: None,
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: None,
        session_store: Some(SessionStoreService::new(arc.clone())),
        trouper_system: None,
    };
    (ctx, arc)
}

async fn run(ctx: ToolContext, args: serde_json::Value) -> ToolResult {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "session_fetch".to_owned(),
        arguments: args.to_string(),
    };
    execute(call, ctx).await
}

#[rstest::rstest]
#[tokio::test]
async fn anchored_fetch_passes_entry_id_and_context() {
    // Given a stub store serving an anchored window.
    let anchor_id = ChatEntryId::new();
    let window = window(vec![entry(
        5,
        ChatEntry::assistant("we decided the junction rewrites were fine"),
    )]);
    let (ctx, stub) = ctx_with(StubStore::with_window(window));

    // When fetching with an entry_id and context.
    let result = run(
        ctx,
        serde_json::json!({ "entry_id": anchor_id.to_string(), "context": 10 }),
    )
    .await;

    // Then the window read succeeded with the requested context.
    assert!(result.success, "{}", result.content);
    assert_eq!(stub.last_read().as_deref(), Some("window:10"));
}

#[rstest::rstest]
#[tokio::test]
async fn tail_read_without_entry_id_uses_limit() {
    // Given a stub store serving a tail window.
    let window = window(vec![
        entry(1, ChatEntry::user("hello")),
        entry(2, ChatEntry::assistant("hi")),
    ]);
    let (ctx, stub) = ctx_with(StubStore::with_window(window));

    // When fetching without entry_id.
    let result = run(ctx, serde_json::json!({ "limit": 15 })).await;

    // Then the tail read used the requested limit.
    assert!(result.success, "{}", result.content);
    assert_eq!(stub.last_read().as_deref(), Some("tail:15"));
}

#[rstest::rstest]
#[tokio::test]
async fn transcript_renders_header_and_positioned_lines() {
    // Given a window with two entries.
    let window = window(vec![
        entry(408, ChatEntry::user("why was rowid mapping dropped?")),
        entry(
            409,
            ChatEntry::assistant("the junction rewrites made it moot"),
        ),
    ]);
    let (ctx, _stub) = ctx_with(StubStore::with_window(window));

    // When fetching.
    let result = run(ctx, serde_json::json!({ "limit": 2 })).await;

    // Then the header shows the live position range and each line is labeled.
    let content = result.content;
    assert!(
        content.contains("session \"migrate auth flow\" (0199aaaa-0000-7000-8000-000000000009) — entries 408–409 of 1204"),
        "{content}"
    );
    assert!(content.contains("[408] user:"), "{content}");
    assert!(content.contains("[409] assistant:"), "{content}");
}

#[rstest::rstest]
#[tokio::test]
async fn gap_in_window_is_announced() {
    // Given a window whose ordinals skip (outer truncation aside).
    let window = window(vec![
        entry(2, ChatEntry::user("a")),
        entry(5, ChatEntry::user("b")),
    ]);
    let (ctx, _stub) = ctx_with(StubStore::with_window(window));

    // When fetching.
    let result = run(ctx, serde_json::json!({ "limit": 2 })).await;

    // Then the skipped positions are announced.
    assert!(
        result.content.contains("[3] … 2 entries not shown …"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn excluded_entry_is_flagged() {
    // Given a window containing an excluded entry.
    let mut excluded = TranscriptEntry {
        ordinal: 412,
        entry: ChatEntry::tool_result(
            "t1",
            "grep",
            "big output",
            jinn_domain::protocol::ToolResultStatus::Success,
        ),
        excluded: false,
    };
    excluded.excluded = true;
    let window = window(vec![entry(411, ChatEntry::user("run grep")), excluded]);
    let (ctx, _stub) = ctx_with(StubStore::with_window(window));

    // When fetching.
    let result = run(ctx, serde_json::json!({ "limit": 2 })).await;

    // Then the excluded entry carries the flag and the included one does not.
    assert!(
        result
            .content
            .contains("[412] tool_result [excluded from context]:"),
        "{}",
        result.content
    );
    assert!(
        !result.content.contains("[411] user [excluded"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn oversized_entry_text_is_elided_with_note() {
    // Given a window whose assistant entry far exceeds the per-entry cap.
    let long = "word ".repeat(900);
    let window = window(vec![entry(1, ChatEntry::assistant(long.trim().to_owned()))]);
    let (ctx, _stub) = ctx_with(StubStore::with_window(window));

    // When fetching.
    let result = run(ctx, serde_json::json!({ "limit": 1 })).await;

    // Then the entry is truncated with an elision note.
    assert!(
        result.content.contains("[entry truncated,"),
        "{}",
        result.content
    );
    // And the rendered entry stays within the cap plus ellipsis.
    let line = result
        .content
        .lines()
        .find(|l| l.starts_with("[1] assistant:"))
        .expect("entry line");
    assert!(line.len() < 2_100, "line len {}", line.len());
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_session_or_anchor_is_a_legible_error() {
    // Given a stub store that returns no window.
    let (ctx, _stub) = ctx_with(StubStore::default());

    // When fetching by entry id.
    let result = run(ctx, serde_json::json!({ "entry_id": "e-1" })).await;

    // Then the error points back at search and mentions the session.
    assert!(!result.success);
    assert!(
        result.content.contains("not found") && result.content.contains("session_search"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn no_session_and_no_current_is_an_error() {
    // Given a context with no session store and no current session.
    let arc = std::sync::Arc::new(StubStore::default());
    let ctx = ToolContext {
        cwd: std::path::PathBuf::from("/tmp"),
        command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
        timeout: None,
        state: None,
        session_id: None,
        app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        session_cap: None,
        mcp_coordinator: None,
        interactive_term: None,
        task_spawns: None,
        session_store: Some(SessionStoreService::new(arc)),
        trouper_system: None,
    };

    // When fetching without a session_id.
    let result = run(ctx, serde_json::json!({ "limit": 5 })).await;

    // Then the error explains both resolution paths failed.
    assert!(!result.success);
    assert!(
        result
            .content
            .contains("no session_id given and no current session"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[test]
fn definition_names_session_fetch() {
    // Given the tool definition.
    let def = definition();

    // Then it is named session_fetch and requires no parameters.
    assert_eq!(def.name, "session_fetch");
    assert!(def.parameters["required"].is_null());
}

#[rstest::rstest]
#[tokio::test]
async fn outer_truncated_result_carries_full_content() {
    // Given a window with many entries and tight outer caps.
    let entries: Vec<TranscriptEntry> = (1..=80)
        .map(|i| entry(i, ChatEntry::user(format!("entry number {i} here"))))
        .collect();
    let (ctx, _stub) = {
        let arc = std::sync::Arc::new(StubStore::with_window(window(entries)));
        let ctx = ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: Some(SessionId::from(
                "0199aaaa-0000-7000-8000-000000000001".to_owned(),
            )),
            app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
            bus: None,
            max_output_lines: Some(10),
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: Some(SessionStoreService::new(arc.clone())),
            trouper_system: None,
        };
        (ctx, arc)
    };

    // When fetching.
    let result = run(ctx, serde_json::json!({ "limit": 80 })).await;

    // Then the result reports truncation metadata.
    assert!(result.success, "{}", result.content);
    let meta = result.truncation.expect("truncation meta");
    assert_eq!(
        meta.truncated_by,
        jinn_core_types::tool_types::TruncatedBy::Lines
    );
    assert_eq!(meta.total_lines, 82); // header, gap announcement, 80 entries
    assert_eq!(meta.output_lines, 10);

    // And the untruncated transcript is carried in full_content.
    let full = result.full_content.expect("full content");
    assert!(full.contains("[80] user:"), "full content has the tail");
    assert!(
        !result.content.contains("[80] user:"),
        "clipped content does not"
    );
}
