//! Tests for the `session_search` tool.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::Mutex;

use error_stack::Report;

use crate::session_search::{definition, execute};
use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::feat::session::session_store::{
    SessionStore, SessionStoreError, SessionStoreService,
};
use jinn_domain::feat::session::session_summary::SessionSummary;
use jinn_domain::feat::session_search::{SearchOutcome, SearchParams, SearchableRole};
use jinn_domain::protocol::{ChatEntryId, SessionId};

/// A stub store with canned summaries and a canned search outcome,
/// recording the last params it was asked to search.
#[derive(Debug, Default)]
struct StubStore {
    summaries: Mutex<Vec<SessionSummary>>,
    outcome: Mutex<Option<SearchOutcome>>,
    last_params: Mutex<Option<SearchParams>>,
    fail_search: Mutex<bool>,
}

impl StubStore {
    fn new() -> Self {
        Self::default()
    }

    fn with_summaries(self, summaries: Vec<SessionSummary>) -> Self {
        *self.summaries.lock().unwrap() = summaries;
        self
    }

    fn with_outcome(self, outcome: SearchOutcome) -> Self {
        *self.outcome.lock().unwrap() = Some(outcome);
        self
    }

    fn failing(self) -> Self {
        *self.fail_search.lock().unwrap() = true;
        self
    }

    fn last_params(&self) -> Option<SearchParams> {
        self.last_params.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SessionStore for StubStore {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn save(
        &self,
        _session: &jinn_domain::feat::session::chat_session::ChatSessionState,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(self.summaries.lock().unwrap().clone())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<
        Option<jinn_domain::feat::session::chat_session::ChatSessionState>,
        Report<SessionStoreError>,
    > {
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
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(self.summaries.lock().unwrap().clone())
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
        params: SearchParams,
    ) -> Result<SearchOutcome, Report<SessionStoreError>> {
        if *self.fail_search.lock().unwrap() {
            return Err(Report::new(SessionStoreError).attach("fts5: syntax error near \"(\""));
        }
        *self.last_params.lock().unwrap() = Some(params);
        Ok(self
            .outcome
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(SearchOutcome {
                total_matches: 0,
                per_session: Vec::new(),
                hits: Vec::new(),
            }))
    }

    async fn fetch_window(
        &self,
        _session_id: &SessionId,
        _anchor: &ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<jinn_domain::feat::session_search::TranscriptWindow>,
        Report<SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        _limit: usize,
    ) -> Result<
        Option<jinn_domain::feat::session_search::TranscriptWindow>,
        Report<SessionStoreError>,
    > {
        Ok(None)
    }
}

fn summary(id: &str, title: &str, project: Option<&str>) -> SessionSummary {
    SessionSummary {
        session_id: SessionId::from(id.to_owned()),
        title: title.to_owned(),
        updated_at: jiff::Timestamp::now(),
        created_at: jiff::Timestamp::now(),
        session_state: jinn_domain::feat::session::chat_session::SessionState::Loaded,
        parent_session: None,
        project: project.map(std::path::PathBuf::from),
    }
}

fn hit(
    session_id: &str,
    entry_id: &str,
    role: &str,
    snippet: &str,
    excluded: bool,
) -> jinn_domain::feat::session_search::SearchHit {
    jinn_domain::feat::session_search::SearchHit {
        session_id: session_id.to_owned(),
        entry_id: entry_id.to_owned(),
        role: role.to_owned(),
        snippet: snippet.to_owned(),
        entry_ts: "2026-09-08T14:30:00Z".to_owned(),
        excluded,
    }
}

fn tool_ctx(store: SessionStoreService, session_id: Option<SessionId>) -> ToolContext {
    ToolContext {
        cwd: std::path::PathBuf::from("/tmp"),
        command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
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
        session_store: Some(store),
            trouper_system: None,
    }
}

fn stub_ctx(
    store: StubStore,
    session_id: Option<SessionId>,
) -> (ToolContext, std::sync::Arc<StubStore>) {
    let arc = std::sync::Arc::new(store);
    (
        tool_ctx(SessionStoreService::new(arc.clone()), session_id),
        arc,
    )
}

async fn run(ctx: ToolContext, args: serde_json::Value) -> ToolResult {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "session_search".to_owned(),
        arguments: args.to_string(),
    };
    execute(call, ctx).await
}

const CURRENT: &str = "0199aaaa-0000-7000-8000-000000000001";
const OTHER: &str = "0199aaaa-0000-7000-8000-000000000002";

#[rstest::rstest]
#[tokio::test]
async fn missing_query_is_rejected() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching without a query.
    let result = run(ctx, serde_json::json!({})).await;

    // Then the result fails and names the missing argument.
    assert!(!result.success);
    assert!(
        result.content.contains("query is required"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn blank_query_is_rejected() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with a whitespace-only query.
    let result = run(ctx, serde_json::json!({ "query": "   " })).await;

    // Then the result fails.
    assert!(!result.success);
    assert!(
        result.content.contains("query is required"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn invalid_json_arguments_are_rejected() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), None);

    // When invoking with malformed JSON.
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "session_search".to_owned(),
        arguments: "{not json".to_owned(),
    };
    let result = execute(call, ctx).await;

    // Then the result fails with the parse error.
    assert!(!result.success);
    assert!(
        result.content.contains("invalid JSON"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn omitted_scope_searches_current_session() {
    // Given a stub store and a current session.
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with no scope.
    run(ctx, serde_json::json!({ "query": "needle" })).await;

    // Then the search was scoped to the calling session.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![CURRENT.to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn current_scope_without_current_session_errors() {
    // Given a stub store and no current session.
    let (ctx, _stub) = stub_ctx(StubStore::new(), None);

    // When searching with no scope.
    let result = run(ctx, serde_json::json!({ "query": "needle" })).await;

    // Then the result explains there is no current session.
    assert!(!result.success);
    assert!(
        result.content.contains("no current session"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn all_scope_searches_every_session() {
    // Given two persisted sessions.
    let (ctx, stub) = stub_ctx(
        StubStore::new().with_summaries(vec![
            summary(CURRENT, "one", None),
            summary(OTHER, "two", None),
        ]),
        Some(SessionId::from(CURRENT.to_owned())),
    );

    // When searching with scope "all".
    run(
        ctx,
        serde_json::json!({ "query": "needle", "scope": "all" }),
    )
    .await;

    // Then both session ids were passed to the store.
    let params = stub.last_params().expect("search ran");
    let mut ids = params.session_ids.clone();
    ids.sort();
    assert_eq!(ids, vec![CURRENT.to_owned(), OTHER.to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn project_scope_matches_project_name_case_insensitively() {
    // Given sessions in two projects.
    let (ctx, stub) = stub_ctx(
        StubStore::new().with_summaries(vec![
            summary(CURRENT, "one", Some("/home/j/session-search")),
            summary(OTHER, "two", Some("/home/j/other-proj")),
        ]),
        Some(SessionId::from(CURRENT.to_owned())),
    );

    // When searching with a differently-cased project name.
    run(
        ctx,
        serde_json::json!({ "query": "needle", "scope": "project:SESSION-SEARCH" }),
    )
    .await;

    // Then only the matching project's session was searched.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![CURRENT.to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_project_name_lists_known_projects() {
    // Given a session in a known project.
    let (ctx, _stub) = stub_ctx(
        StubStore::new().with_summaries(vec![summary(
            CURRENT,
            "one",
            Some("/home/j/session-search"),
        )]),
        None,
    );

    // When searching a project that does not exist.
    let result = run(
        ctx,
        serde_json::json!({ "query": "n", "scope": "project:nope" }),
    )
    .await;

    // Then the error lists the known project names.
    assert!(!result.success);
    assert!(
        result.content.contains("unknown project 'nope'")
            && result.content.contains("session-search"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn invalid_scope_value_is_rejected() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with an unrecognized scope.
    let result = run(
        ctx,
        serde_json::json!({ "query": "n", "scope": "everything" }),
    )
    .await;

    // Then the error explains the valid scope forms.
    assert!(!result.success);
    assert!(
        result.content.contains("invalid scope"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn explicit_session_id_overrides_scope() {
    // Given a stub store with the current session set.
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with an explicit other session id.
    run(
        ctx,
        serde_json::json!({ "query": "n", "scope": "all", "session_id": OTHER }),
    )
    .await;

    // Then only the explicit session was searched.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![OTHER.to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn default_fields_are_assistant_and_user() {
    // Given a stub store.
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching without fields.
    run(ctx, serde_json::json!({ "query": "n" })).await;

    // Then the store received assistant and user roles only.
    let params = stub.last_params().expect("search ran");
    assert_eq!(
        params.roles,
        vec![SearchableRole::Assistant, SearchableRole::User]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn fields_opt_in_tool_result_is_passed_through() {
    // Given a stub store.
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with fields [tool_result].
    run(
        ctx,
        serde_json::json!({ "query": "n", "fields": ["tool_result"] }),
    )
    .await;

    // Then the store received the tool_result role.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.roles, vec![SearchableRole::ToolResult]);
}

#[rstest::rstest]
#[tokio::test]
async fn actor_field_is_rejected_as_never_indexed() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with fields including actor.
    let result = run(
        ctx,
        serde_json::json!({ "query": "n", "fields": ["assistant", "actor"] }),
    )
    .await;

    // Then the request is rejected.
    assert!(!result.success);
    assert!(
        result.content.contains("never indexed"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[case::over(500, 50)]
#[case::zero(0, 1)]
#[case::typical(10, 10)]
fn limit_is_clamped_into_allowed_range(#[case] requested: u64, #[case] expected: u64) {
    // Given parsed limit values at the boundary cases.
    // When clamping.
    let clamped = requested.clamp(1, 50);

    // Then the result is within 1..=50.
    assert_eq!(clamped, expected);
}

#[rstest::rstest]
#[tokio::test]
async fn invalid_since_date_is_rejected() {
    // Given a tool context.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with a malformed since date.
    let result = run(
        ctx,
        serde_json::json!({ "query": "n", "since": "september" }),
    )
    .await;

    // Then the error names the argument.
    assert!(!result.success);
    assert!(
        result.content.contains("invalid since"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn date_only_since_is_accepted_as_utc() {
    // Given a stub store.
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching with a date-only since.
    run(
        ctx,
        serde_json::json!({ "query": "n", "since": "2026-09-01" }),
    )
    .await;

    // Then the store received a UTC midnight instant.
    let params = stub.last_params().expect("search ran");
    let since = params.since.expect("since set");
    assert_eq!(since.to_string(), "2026-09-01T00:00:00Z");
}

#[rstest::rstest]
#[tokio::test]
async fn search_failure_surfaced_verbatim() {
    // Given a stub store whose search fails with an fts5 message.
    let (ctx, _stub) = stub_ctx(
        StubStore::new().failing(),
        Some(SessionId::from(CURRENT.to_owned())),
    );

    // When searching with a syntactically invalid query.
    let result = run(ctx, serde_json::json!({ "query": "AND (" })).await;

    // Then the fts5 message text appears in the failed result.
    assert!(!result.success);
    assert!(
        result.content.contains("fts5: syntax error"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn missing_session_store_fails_gracefully() {
    // Given a context with no session store.
    let ctx = tool_ctx(
        SessionStoreService::new(std::sync::Arc::new(
            jinn_domain::common::services::test_services::FakeSessionStore,
        )),
        None,
    );
    let ctx = ToolContext {
        session_store: None,
            trouper_system: None,
        ..ctx
    };

    // When searching.
    let result = run(ctx, serde_json::json!({ "query": "n" })).await;

    // Then the tool reports the store is unavailable.
    assert!(!result.success);
    assert!(
        result.content.contains("session store unavailable"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn zero_matches_renders_no_match_header() {
    // Given a stub store returning an empty outcome.
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(SessionId::from(CURRENT.to_owned())));

    // When searching.
    let result = run(ctx, serde_json::json!({ "query": "zzz" })).await;

    // Then the output is the no-match header.
    assert!(result.success);
    assert_eq!(result.content, "search: 'zzz' — no matches\n");
}

#[rstest::rstest]
#[tokio::test]
async fn outcome_renders_header_legend_rollup_and_hits() {
    // Given a stub store returning two hits across two sessions.
    let outcome = SearchOutcome {
        total_matches: 47,
        per_session: vec![(CURRENT.to_owned(), 41), (OTHER.to_owned(), 6)],
        hits: vec![
            hit(
                CURRENT,
                "e-1",
                "assistant",
                "…we decided <<rowid mapping>> was unnecessary…",
                false,
            ),
            hit(OTHER, "e-2", "tool_result", "…CREATE TABLE <<fts5>>…", true),
        ],
    };
    let (ctx, _stub) = stub_ctx(
        StubStore::new().with_outcome(outcome).with_summaries(vec![
            summary(CURRENT, "migrate auth flow", None),
            summary(OTHER, "session search", None),
        ]),
        Some(SessionId::from(CURRENT.to_owned())),
    );

    // When searching.
    let result = run(ctx, serde_json::json!({ "query": "rowid" })).await;

    // Then the output carries the legend, per-session rollup, and labeled hit lines.
    assert!(result.success);
    let lines: Vec<&str> = result.content.lines().collect();
    assert!(
        lines[0].contains("search: 'rowid' — 47 matches in 2 sessions (showing 2 best)"),
        "{}",
        result.content
    );
    assert!(lines[1].starts_with("legend:"), "{}", result.content);
    assert!(
        lines[2].contains(&format!("`{CURRENT}` \"migrate auth flow\" ×41"))
            && lines[2].contains(&format!("`{OTHER}` \"session search\" ×6")),
        "{}",
        result.content
    );
    assert!(
        lines[3].contains("`e-1`")
            && lines[3].contains("assistant")
            && lines[3].contains("<<rowid mapping>>"),
        "{}",
        result.content
    );
    assert!(
        lines[4].contains("[excluded from context]"),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn multiline_snippet_is_collapsed_to_one_line() {
    // Given an outcome whose snippet contains newlines and pipes.
    let outcome = SearchOutcome {
        total_matches: 1,
        per_session: vec![(CURRENT.to_owned(), 1)],
        hits: vec![hit(
            CURRENT,
            "e-1",
            "assistant",
            "line one\nline\ttwo | three <<needle>>",
            false,
        )],
    };
    let (ctx, _stub) = stub_ctx(
        StubStore::new()
            .with_outcome(outcome)
            .with_summaries(vec![summary(CURRENT, "t", None)]),
        Some(SessionId::from(CURRENT.to_owned())),
    );

    // When searching.
    let result = run(ctx, serde_json::json!({ "query": "needle" })).await;

    // Then the hit renders as exactly one line.
    let hit_lines: Vec<&str> = result
        .content
        .lines()
        .filter(|l| l.contains("`e-1`"))
        .collect();
    assert_eq!(hit_lines.len(), 1);
    assert!(hit_lines[0].contains("line one line two | three <<needle>>"));
}

#[rstest::rstest]
#[test]
fn definition_names_session_search() {
    // Given the tool definition.
    let def = definition();

    // Then it is named session_search and requires the query parameter.
    assert_eq!(def.name, "session_search");
    let required = def.parameters["required"].as_array().expect("required");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "query");
}
