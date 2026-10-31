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
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_kernel::common::app_paths::AppPaths;
use jinn_kernel::protocol::ChatEntryId;
use jinn_session_state::SessionSnapshot;
use jinn_session_state::{SessionStore, SessionStoreError, SessionStoreService};
use jinn_session_store_msg::SessionSummary;
use jinn_session_store_msg::{SearchOutcome, SearchParams, SearchableRole};
use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};

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

    async fn save(&self, _snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(self.summaries.lock().unwrap().clone())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
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

    async fn set_in_library(
        &self,
        _session_id: &SessionId,
        _in_library: bool,
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
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        _limit: usize,
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }
}

fn summary(id: &str, title: &str, project: Option<&str>) -> SessionSummary {
    SessionSummary {
        session_id: SessionId::try_from_string(id).unwrap(),
        title: title.to_owned(),
        updated_at: jiff::Timestamp::now(),
        created_at: jiff::Timestamp::now(),
        session_state: jinn_session_store_msg::SessionState::Loaded,
        in_library: false,
        is_bot: false,
        parent_session: None,
        project: project.map(|p| {
            let root = std::env::current_dir().expect("process cwd is readable");
            MarkedPathBuf::<Absolute>::new(root.join(p))
                .expect("project paths in these fixtures are repo-relative")
        }),
    }
}

fn hit(
    session_id: &str,
    entry_id: &str,
    role: &str,
    snippet: &str,
    excluded: bool,
) -> jinn_session_store_msg::SearchHit {
    jinn_session_store_msg::SearchHit {
        session_id: SessionId::try_from_string(session_id).unwrap(),
        entry_id: ChatEntryId::new_v5_tag(entry_id),
        role: role.to_owned(),
        snippet: snippet.to_owned(),
        entry_ts: "2026-09-08T14:30:00Z".to_owned(),
        excluded,
    }
}

fn tool_ctx(
    store: SessionStoreService,
    session_id: Option<SessionId>,
    paths_root: &MarkedPathBuf<Absolute>,
) -> ToolContext {
    ToolContext {
        turn: crate::tool_types::test_turn(),
        cwd: paths_root.clone(),
        config: jinn_config::testutil::config_layer(""),
        shells: Default::default(),
        timeout: None,
        state: None,
        session_id,
        app_paths: AppPaths::new_in(&paths_root.as_marked_path()),
        bus: None,
        max_output_lines: None,
        max_output_bytes: None,
        dispatched_at: jiff::Timestamp::now(),
        mcp_coordinator: None,
        canvas_server: None,
        interactive_term: None,
        task_spawns: None,
        session_store: Some(store),
        trouper_system: None,
        model_name_resolver: None,
    }
}

fn stub_ctx(
    store: StubStore,
    session_id: Option<SessionId>,
    paths_root: &MarkedPathBuf<Absolute>,
) -> (ToolContext, std::sync::Arc<StubStore>) {
    let arc = std::sync::Arc::new(store);
    (
        tool_ctx(
            SessionStoreService::new(arc.clone()),
            session_id,
            paths_root,
        ),
        arc,
    )
}

async fn run(ctx: ToolContext, args: serde_json::Value) -> ToolResult {
    let call = ToolCall {
        id: "call-1".into(),
        name: "session_search".to_owned(),
        arguments: args.to_string(),
    };
    execute(call, ctx).await
}

const CURRENT: &str = "0199aaaa-0000-7000-8000-000000000001";
const OTHER: &str = "0199aaaa-0000-7000-8000-000000000002";

fn current_id() -> SessionId {
    SessionId::try_from_string(CURRENT).unwrap()
}

fn other_id() -> SessionId {
    SessionId::try_from_string(OTHER).unwrap()
}

#[rstest::rstest]
#[tokio::test]
async fn missing_query_is_rejected() {
    // Given a tool context.
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), None, &dir.path);

    // When invoking with malformed JSON.
    let call = ToolCall {
        id: "call-1".into(),
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
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

    // When searching with no scope.
    run(ctx, serde_json::json!({ "query": "needle" })).await;

    // Then the search was scoped to the calling session.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![current_id()]);
}

#[rstest::rstest]
#[tokio::test]
async fn current_scope_without_current_session_errors() {
    // Given a stub store and no current session.
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), None, &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(
        StubStore::new().with_summaries(vec![
            summary(CURRENT, "one", None),
            summary(OTHER, "two", None),
        ]),
        Some(current_id()),
        &dir.path,
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
    assert_eq!(ids, vec![current_id(), other_id()]);
}

#[rstest::rstest]
#[tokio::test]
async fn project_scope_matches_project_name_case_insensitively() {
    // Given sessions in two projects, the project paths owned by the test.
    let dir = jinn_testutil::tempdir();
    let project = dir
        .path
        .join("home/j/session-search")
        .expect("temp root joins a bare name");
    let project_name = project
        .file_name()
        .expect("named project")
        .to_string_lossy()
        .to_string();
    let other_project = dir
        .path
        .join("home/j/other-proj")
        .expect("temp root joins a bare name");
    let (ctx, stub) = stub_ctx(
        StubStore::new().with_summaries(vec![
            summary(CURRENT, "one", Some(&project.to_string())),
            summary(OTHER, "two", Some(&other_project.to_string())),
        ]),
        Some(current_id()),
        &dir.path,
    );

    // When searching with a differently-cased form of the project's name.
    run(
        ctx,
        serde_json::json!({
            "query": "needle",
            "scope": format!("project:{}", project_name.to_uppercase())
        }),
    )
    .await;

    // Then only the matching project's session was searched.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![current_id()]);
}

#[rstest::rstest]
#[tokio::test]
async fn unknown_project_name_lists_known_projects() {
    // Given a session in a known project whose path the test owns.
    let dir = jinn_testutil::tempdir();
    let project = dir
        .path
        .join("home/j/session-search")
        .expect("temp root joins a bare name");
    let project_name = project
        .file_name()
        .expect("named project")
        .to_string_lossy()
        .to_string();
    let (ctx, _stub) = stub_ctx(
        StubStore::new().with_summaries(vec![summary(CURRENT, "one", Some(&project.to_string()))]),
        None,
        &dir.path,
    );

    // When searching a project that does not exist.
    let result = run(
        ctx,
        serde_json::json!({ "query": "n", "scope": "project:nope" }),
    )
    .await;

    // Then the error lists the known project's name.
    assert!(!result.success);
    assert!(
        result.content.contains("unknown project 'nope'") && result.content.contains(&project_name),
        "{}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn invalid_scope_value_is_rejected() {
    // Given a tool context.
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

    // When searching with an explicit other session id.
    run(
        ctx,
        serde_json::json!({ "query": "n", "scope": "all", "session_id": OTHER }),
    )
    .await;

    // Then only the explicit session was searched.
    let params = stub.last_params().expect("search ran");
    assert_eq!(params.session_ids, vec![other_id()]);
}

#[rstest::rstest]
#[tokio::test]
async fn default_fields_are_assistant_and_user() {
    // Given a stub store.
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new().failing(), Some(current_id()), &dir.path);

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
    let dir = jinn_testutil::tempdir();
    let ctx = tool_ctx(
        SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        None,
        &dir.path,
    );
    let ctx = ToolContext {
        config: jinn_config::testutil::config_layer(""),
        shells: Default::default(),
        session_store: None,
        trouper_system: None,
        model_name_resolver: None,
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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(StubStore::new(), Some(current_id()), &dir.path);

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
        per_session: vec![(current_id(), 41), (other_id(), 6)],
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
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(
        StubStore::new().with_outcome(outcome).with_summaries(vec![
            summary(CURRENT, "migrate auth flow", None),
            summary(OTHER, "session search", None),
        ]),
        Some(current_id()),
        &dir.path,
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
        lines[3].contains(&format!("`{}`", ChatEntryId::new_v5_tag("e-1")))
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
        per_session: vec![(current_id(), 1)],
        hits: vec![hit(
            CURRENT,
            "e-1",
            "assistant",
            "line one\nline\ttwo | three <<needle>>",
            false,
        )],
    };
    let dir = jinn_testutil::tempdir();
    let (ctx, _stub) = stub_ctx(
        StubStore::new()
            .with_outcome(outcome)
            .with_summaries(vec![summary(CURRENT, "t", None)]),
        Some(current_id()),
        &dir.path,
    );

    // When searching.
    let result = run(ctx, serde_json::json!({ "query": "needle" })).await;

    // Then the hit renders as exactly one line.
    let hit_lines: Vec<&str> = result
        .content
        .lines()
        .filter(|l| l.contains(&format!("`{}`", ChatEntryId::new_v5_tag("e-1"))))
        .collect();
    assert_eq!(hit_lines.len(), 1);
    assert!(hit_lines[0].contains("line one line two | three <<needle>>"));
}

#[rstest::rstest]
#[test]
fn definition_names_session_search() {
    // Given the tool definition.
    let def = definition();

    // When reading its name and required parameters.
    // Then it is named session_search and requires the query parameter.
    assert_eq!(def.name, "session_search");
    let required = def.parameters["required"].as_array().expect("required");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "query");
}
