//! Handler tests: the read paths (page, transcript cursor, sessions,
//! model picker) and the action paths (send, cancel, model, switch)
//! driven through the router onto a recording bus.

//! Test code: assertions use expect/panic to state what went wrong, and
//! empty-collection checks use `assert_eq!(len, 0)` so the failure names
//! the value.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use crate::contract;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use jinn_app_state::AppState;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_slices::bus::BusAudit;
use jinn_slices::bus::BusService;
use tower::ServiceExt as _;

use crate::server::ServerShared;
use crate::server::router;

/// Builds the router over an empty app state, a fresh provider cell on
/// a private registry (seeded with one keyless `openai` provider
/// offering `openai/gpt-4o`, so the picker renders an available row the
/// switch endpoint accepts), and a recording bus. Returns the parts a
/// test manipulates and asserts against.
fn harness() -> (State, BusAudit, Slices, axum::Router) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once per harness");
    let mcp_runtime_cell = slices
        .register(
            jinn_mcp_msg::mcp_runtime_slot(),
            jinn_mcp_msg::McpRuntimeState::default(),
        )
        .expect("the MCP runtime cell registers once");
    let unread_marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let (bus, audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        jinn_provider_config::ProviderRegistryService::new(harness_registry()),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        jinn_config::empty_config_layer().clone(),
        fanout,
        8387,
        "test".to_owned(),
    );
    (state, audit, slices, router(shared))
}

/// Registers the unread-mark cell and returns its write handle — the
/// same resolution production `activate` performs against the shared
/// catalog, against a harness-local registry instead.
fn unread_marks_for(
    slices: &Slices,
) -> jinn_slices::cell::TypedCell<jinn_webui_msg::UnreadSessionMarks> {
    slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per harness")
}

/// The harness registry: one keyless `openai` provider offering the
/// model `gpt-4o` — a static row the picker renders available without
/// discovery (requires_key false), which is also the fresh-boot case
/// the rows-based switch validation keeps working. The expanded id is
/// `{name}/{model}` = `openai/gpt-4o`, the full shape the rows carry.
fn harness_registry() -> jinn_provider_config::ProviderRegistry {
    jinn_provider_config::ProviderRegistry::from_config(jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([(
            "openai".to_owned(),
            jinn_provider_config::ProviderEntry {
                backend: "openai".to_owned(),
                models: vec!["gpt-4o".to_owned()],
                base_url: None,
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
                model_info: Vec::new(),
            },
        )]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    })
    .expect("the harness providers config expands")
}

/// Inserts a fresh live session whose id is returned, so callers use
/// the session's own id rather than pre-fabricating one.
fn seed_session(state: &State) -> SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    id
}

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("response");
    let status = response.status();
    let text = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    (status, String::from_utf8_lossy(&text).into_owned())
}

async fn post_form(router: &axum::Router, uri: &str, body: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_owned()))
                .expect("request builds"),
        )
        .await
        .expect("response");
    let status = response.status();
    let text = axax_bytes(response).await;
    (status, text)
}

async fn axax_bytes(response: axum::response::Response) -> String {
    let text = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    String::from_utf8_lossy(&text).into_owned()
}

/// The ids of the rows carrying the active class in sessions-list
/// markup — per-row truth, not a whole-document guess.
fn active_row_ids(html: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let row_open = format!("{} ", crate::contract::SESSION_ROW_CLASS);
    for segment in html.split(&row_open) {
        // The row opens as `class="srow active bot unread"`, so every
        // split boundary after the first is inside a class list; a
        // boundary whose following text continues with `active` marks
        // an active row. The id is the session attribute's quoted
        // value on the same tag.
        let Some(classes) = segment.split('"').next() else {
            continue;
        };
        let classes: Vec<&str> = classes.split_whitespace().collect();
        if !classes.contains(&"active") {
            continue;
        }
        let attr = format!("{}=\"", crate::contract::SESSION_ID_DATA_ATTR);
        let Some(after) = segment.split(attr.as_str()).nth(1) else {
            continue;
        };
        let Some(id) = after.split('"').next() else {
            continue;
        };
        ids.push(id.to_owned());
    }
    ids
}

/// The harness plus a **real** store actor: the single writer of the
/// active pointer, so switch-convergence tests exercise the actual
/// async handoff the recording-bus harness cannot (nothing there
/// consumes the published command, so no activation ever lands).
///
/// Cells resolve from the shared catalog exactly as production
/// activation does; the store behind the actor is the kernel's
/// in-memory fake (loads return `None`, so the in-memory activation
/// path is what runs). The router publishes onto the actor's troupe
/// bus — the delivering fabric — while the audit stays recording-only
/// (a recording bus captures publishes and delivers nothing, so the
/// switch command could never reach the actor through it); the tests
/// this harness serves assert on state and markup, not the audit.
async fn harness_with_store_actor() -> (State, BusAudit, Slices, axum::Router) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let unread_marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    // The actor's own `Services`: a fake-config container whose troupe
    // bus delivers its publishes, catalog-seeded cells resolved the way
    // the slice's production `activate` resolves them, and the shared
    // `State` the actor writes the active pointer into.
    let actor_services = jinn_kernel::Services::new_fake().await;
    let session_picker_cell = actor_services
        .slices
        .reader::<jinn_session_store_msg::SessionPickerState>(
            &jinn_session_store_msg::session_picker_slot(),
        )
        .expect("the cell catalog registers the session-picker slot");
    let library_cell = actor_services
        .slices
        .reader::<jinn_session_store_msg::LibraryState>(&jinn_session_store_msg::library_slot())
        .expect("the cell catalog registers the library slot");
    jinn_session_store::session_store_actor::SessionStoreActor::spawn(
        &actor_services.trouper_system,
        jinn_session_store::session_store_actor::SessionStoreActorDeps {
            services: actor_services.clone(),
            state: state.clone(),
            session_picker_cell,
            library_cell,
        },
    );
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(
            &system,
            state.clone(),
            actor_services.bus.clone(),
            unread_marks,
        )
    };
    let shared = ServerShared::new(
        state.clone(),
        actor_services.bus.clone(),
        actor_services.session_store,
        provider_cell,
        actor_services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("the cell catalog registers the personas slot"),
        actor_services
            .slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("the cell catalog registers the MCP runtime slot"),
        jinn_provider_config::ProviderRegistryService::new(harness_registry()),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        jinn_config::empty_config_layer().clone(),
        fanout,
        8388,
        "test-store-actor".to_owned(),
    );
    let (_bus, audit) = BusService::new_recording();
    (state, audit, slices, router(shared))
}

/// The count of recorded publishes whose schema short name ends with
/// the named message type.
fn audit_count(audit: &BusAudit, name: &str) -> usize {
    audit.names().iter().filter(|n| n.ends_with(name)).count()
}

/// The session's current title as live state holds it.
fn title_of(state: &State, id: &SessionId) -> Option<String> {
    state
        .read()
        .session
        .get(id)
        .map_or_default(|session| session.title().map(str::to_owned))
}

/// A session store serving one fixed archived summary from
/// `load_summaries`; every other operation behaves like the fake.
#[derive(Debug)]
struct OneArchivedRowStore(jinn_core_types::SessionId, String);

#[async_trait::async_trait]
impl jinn_session_state::SessionStore for OneArchivedRowStore {
    fn name(&self) -> &'static str {
        "one-archived-row"
    }

    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(vec![jinn_session_store_msg::SessionSummary {
            session_id: self.0.clone(),
            title: self.1.clone(),
            updated_at: jiff::Timestamp::now(),
            created_at: jiff::Timestamp::now(),
            session_state: jinn_session_store_msg::SessionState::Archived,
            in_library: false,
            is_bot: false,
            parent_session: None,
            project: None,
        }])
    }

    async fn save(
        &self,
        _snapshot: &jinn_session_state::SessionSnapshot,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn load_session(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<
        Option<jinn_session_state::SessionSnapshot>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }

    async fn delete(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &jinn_core_types::SessionId,
        _at_ordinal: usize,
    ) -> Result<
        jinn_core_types::SessionId,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(jinn_core_types::SessionId::new())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn search(
        &self,
        _params: jinn_session_store_msg::SearchParams,
    ) -> Result<
        jinn_session_store_msg::SearchOutcome,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(jinn_session_store_msg::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn set_archived(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[jinn_core_types::SessionId],
        _archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn set_in_library(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _in_library: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<
        Vec<jinn_core_types::SessionId>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _max_entries: usize,
    ) -> Result<bool, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(
        &self,
    ) -> Result<usize, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(0)
    }

    async fn fetch_window(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _anchor: &jinn_core_types::ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _limit: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }
}

/// The rename harness: the standard router plus a subscriber over the
/// fan-out's broadcast channel, with the store serving one archived
/// summary row. Returns `(state, audit, receiver, router)`.
fn harness_with_archived_row_and_receiver() -> (
    State,
    BusAudit,
    tokio::sync::broadcast::Receiver<jinn_webui_msg::SseEnvelope>,
    axum::Router,
) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let archived_id = SessionId::new();
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once per harness");
    let mcp_runtime_cell = slices
        .register(
            jinn_mcp_msg::mcp_runtime_slot(),
            jinn_mcp_msg::McpRuntimeState::default(),
        )
        .expect("the MCP runtime cell registers once");
    let (bus, audit) = BusService::new_recording();
    let (fanout, rx) =
        crate::fanout::WebUiFanoutActor::test_pair(&state, bus.clone(), unread_marks_for(&slices));
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneArchivedRowStore(
            archived_id,
            "Archived from last run".to_owned(),
        ))),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        jinn_provider_config::ProviderRegistryService::new(harness_registry()),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        jinn_config::empty_config_layer().clone(),
        fanout,
        8387,
        "test".to_owned(),
    );
    (state, audit, rx, router(shared))
}

/// Read paths: page shell, sessions list, transcript cursor grammar.
mod reads {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn home_renders_the_page_shell_with_sse_markup() {
        // Given the router over an empty state.
        let (_state, _audit, _slices, router) = harness();

        // When getting the home page.
        let (status, body) = get(&router, "/web").await;

        // Then it renders with the regions the runtime drives, and
        // the auto-initializing Datastar script tag is present.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!(
                "src=\"{}{}\"",
                contract::VENDOR_PREFIX,
                crate::templates::app_js_versioned_path()
            )),
            "missing client script: {body}"
        );
        assert!(body.contains("id=\"composer-controls\""));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_page_renders_a_seeded_session_title() {
        // Given a live session with a title.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_title("Fix the leak".to_owned());

        // When getting its page.
        let (status, body) = get(&router, &format!("/web/session/{id}")).await;

        // Then the title and full transcript render.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Fix the leak"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn transcript_since_returns_only_later_entries() {
        // Given a session holding three user entries and its id.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            for text in ["one", "two", "three"] {
                session.push_entry(jinn_core_types::ChatEntry::user(text));
            }
        }

        // When pulling the transcript from cursor 1.
        let (status, body) = get(&router, &format!("/web/transcript/{id}?since=1")).await;

        // Then only entries at and after the cursor render — the first
        // stays out, the rest arrive in order.
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("one"), "cursor entry leaked: {body}");
        assert!(body.contains("two"), "{body}");
        assert!(body.contains("three"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn transcript_since_beyond_the_length_returns_the_full_transcript() {
        // Given a session holding two entries and its id.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            for text in ["alpha", "beta"] {
                session.push_entry(jinn_core_types::ChatEntry::user(text));
            }
        }

        // When pulling with a cursor past the end — a history that
        // shrank under a rewind-retry.
        let (status, body) = get(&router, &format!("/web/transcript/{id}?since=9")).await;

        // Then the pull re-syncs with everything.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("alpha"), "{body}");
        assert!(body.contains("beta"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn transcript_unknown_session_is_a_404() {
        // Given no such session — neither live in state nor in the
        // store.
        let (_state, _audit, _slices, router) = harness();
        let id = SessionId::new();

        // When pulling its transcript.
        let (status, _body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the pull names the miss so a stale tab fails loudly
        // instead of rendering a phantom page.
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_list_renders_seeded_rows() {
        // Given one live session.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When getting the sessions fragment.
        let (status, body) = get(&router, "/web/sessions").await;

        // Then the fragment renders a row for it.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&id.to_string()), "{body}");
    }
}

/// Rendered entries: verbatim user text, markdown, tool pairing,
/// escaping — through the transcript endpoint.
mod renders {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn user_entry_renders_its_display_text_verbatim() {
        // Given a session with a user entry.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .push_entry(jinn_core_types::ChatEntry::user("hello from outside"));

        // When rendering the transcript.
        let (_status, body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the display text appears as written.
        assert!(body.contains("hello from outside"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn assistant_entry_renders_markdown_as_html() {
        // Given a session with an assistant entry carrying markdown.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .push_entry(jinn_core_types::ChatEntry::assistant("*emph* and `code`"));

        // When rendering the transcript.
        let (_status, body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the markdown is rendered server-side.
        assert!(body.contains("<em>emph</em>"), "{body}");
        assert!(body.contains("<code>code</code>"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_call_and_result_render_as_a_collapsible_pair() {
        // Given a session with a tool call and its result.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::tool_call(
                "t1",
                "read_file",
                "/tmp/x",
            ));
            session.push_entry(jinn_core_types::ChatEntry::tool_result(
                "t1",
                "read_file",
                "the file body",
                jinn_core_types::ToolResultStatus::Success,
            ));
        }

        // When rendering the transcript.
        let (_status, body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the pair renders as one disclosure naming the tool.
        assert!(body.contains("<details"), "{body}");
        assert!(body.contains("read_file"), "{body}");
        assert!(body.contains("the file body"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn html_in_tool_output_is_escaped() {
        // Given a tool result whose body carries markup.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::tool_call("t1", "run", "x"));
            session.push_entry(jinn_core_types::ChatEntry::tool_result(
                "t1",
                "run",
                "<script>alert(1)</script>",
                jinn_core_types::ToolResultStatus::Success,
            ));
        }

        // When rendering the transcript.
        let (_status, body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the markup is escaped — no live tag survives.
        assert!(!body.contains("<script>"), "{body}");
        assert!(body.contains("&lt;script&gt;"), "{body}");
    }
}

/// Action paths: each publish names its existing schema, and the
/// composer controls swap on phase.
mod actions {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn send_publishes_an_enqueued_user_message_with_the_text_verbatim() {
        // Given a live session and the router over it.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a send.
        let (status, _body) = post_form(
            &router,
            "/web/send",
            &format!("session_id={id}&text=hello+from+outside"),
        )
        .await;

        // Then the ack is OK and the bus names EnqueueUserMessage.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "EnqueueUserMessage"), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_appends_the_user_entry_to_the_session() {
        // Given a live session and the router over it.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a send.
        post_form(
            &router,
            "/web/send",
            &format!("session_id={id}&text=note+to+self"),
        )
        .await;

        // Then the published enqueue carries the entry verbatim — the
        // chat-input actor owns the session-side append.
        let published = audit.of_type::<jinn_chat_input_msg::EnqueueUserMessage>();
        let [message] = published.as_slice() else {
            panic!("exactly one enqueue was published, got {}", published.len())
        };
        assert_eq!(message.session_id, id);
        assert_eq!(message.entry.text(), "note to self");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_refuses_an_empty_text() {
        // Given a live session.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a send with an empty body.
        let (status, _body) =
            post_form(&router, "/web/send", &format!("session_id={id}&text=")).await;

        // Then the request is refused with the client-error status and
        // nothing is published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_publishes_cancel_turn() {
        // Given a live session.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a cancel.
        let (status, _body) = post_form(&router, "/web/cancel", &format!("session_id={id}")).await;

        // Then the bus names CancelTurn.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "CancelTurn"), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_switch_publishes_a_single_provider_switch() {
        // Given a live session and a configured provider (the harness
        // registry's keyless `openai` offering `openai/gpt-4o`) — no
        // discovery needed: a fresh boot renders the row already.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a model pick for the full provider id the row
        // carries.
        let (status, body) = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=openai%2Fgpt-4o"),
        )
        .await;

        // Then the bus names ProviderSwitch and the response is the
        // re-rendered titlebar button.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "ProviderSwitch"), 1);
        assert!(
            body.contains(&format!(
                "id=\"{}\"",
                crate::contract::MODEL_PICKER_BUTTON_ID
            )),
            "{body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_switch_response_names_the_picked_model_not_the_previous_one() {
        // Given a live session and a configured provider (the harness
        // registry's keyless `openai` offering `openai/gpt-4o`).
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a model pick.
        let (status, body) = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=openai%2Fgpt-4o"),
        )
        .await;

        // Then the response's button text names the picked model —
        // the fragment is rendered from state that already holds the
        // pick, never one switch behind (the stale-label bug: the bus
        // is async, so a fragment rendered off a not-yet-applied
        // command named the PREVIOUS model, and the button only caught
        // up on the next interaction).
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("openai/gpt-4o"),
            "the switch response must name the picked model: {body}"
        );
        // And the button text agrees with the pick.
        let button = body
            .split_once(&format!(
                "id=\"{}\"",
                crate::contract::MODEL_PICKER_BUTTON_ID
            ))
            .and_then(|(_, rest)| rest.split_once('>'))
            .and_then(|(_, rest)| rest.split('<').next())
            .unwrap_or_default();
        assert_eq!(
            button, "openai/gpt-4o",
            "the button text must name the picked model: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_switch_accepts_a_cache_discovered_model_from_an_unknown_provider() {
        // Given a live session and a cache entry for a provider the
        // config does NOT list (an unknown provider is unavailable by
        // the shared builder's rule) — plus a cache entry whose
        // provider IS configured.
        let (state, audit, slices, router) = harness();
        let id = seed_session(&state);
        slices
            .reader::<jinn_provider_selection_msg::ProviderCell>(
                &jinn_provider_selection_msg::provider_state_slot(),
            )
            .expect("harness registered the provider cell")
            .update(|cell| {
                cell.model_cache = Some(jinn_provider_config::ModelCache {
                    entries: std::collections::HashMap::from([(
                        "ghost-provider".to_owned(),
                        vec![jinn_provider::ModelInfo {
                            id: "ghost/phantom".to_owned(),
                            context_length: None,
                            input_modalities: jinn_provider::InputModalities::default(),
                        }],
                    )]),
                    last_updated_at: None,
                });
            });

        // When posting a pick for that cache-only id.
        let (status, _body) = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=ghost%2Fphantom"),
        )
        .await;

        // Then the server refuses: the cache holds it, but its
        // provider has no config entry, so the row renders unavailable
        // — and unavailable rows are not selectable (the terminal
        // shows them with the `✗` marker for the same reason).
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit_count(&audit, "ProviderSwitch"), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn switch_publishes_session_load_requested() {
        // Given the router holding a live session (the client only
        // activates rows it rendered).
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting an activate for it.
        let (status, _body) = post_form(&router, &format!("/web/session/{id}/activate"), "").await;

        // Then the bus names SessionLoadRequested — the same schema the
        // TUI switch uses.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "SessionLoadRequested"), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn refresh_models_publishes_one_refresh_command_and_answers_the_button() {
        // Given a live session.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting the refresh-models action.
        let (status, body) = post_form(
            &router,
            crate::contract::MODELS_REFRESH_ROUTE,
            &format!("session_id={id}"),
        )
        .await;

        // Then exactly one `RefreshModels` published and the response
        // is the re-rendered BUTTON fragment — the POST never blocks
        // on discovery.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "RefreshModels"), 1);
        assert!(
            body.contains(&format!(
                "id=\"{}\"",
                crate::contract::MODEL_PICKER_BUTTON_ID
            )),
            "{body}"
        );
        assert!(
            !body.contains(&format!(
                "id=\"{}\"",
                crate::contract::MODEL_PICKER_MODAL_ID
            )),
            "the refresh POST must not answer with the modal: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn refresh_models_refuses_a_malformed_session_id() {
        // Given no session (and a malformed id).
        let (_state, _audit, _slices, router) = harness();

        // When posting the refresh-models action.
        let (status, _body) = post_form(
            &router,
            crate::contract::MODELS_REFRESH_ROUTE,
            "session_id=not-a-uuid",
        )
        .await;

        // Then the request is a 400.
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_switch_refuses_an_id_outside_the_available_rows() {
        // Given a live session (the harness registry offers only
        // `openai/gpt-4o`, no cache seeded).
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a pick for a model no rendered row carries.
        let (status, _body) = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=ollama%2Fnot-discovered"),
        )
        .await;

        // Then the server refuses with 400 and publishes nothing.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit_count(&audit, "ProviderSwitch"), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn composer_shows_send_when_idle() {
        // Given an idle live session.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When rendering the session page.
        let (_status, body) = get(&router, &format!("/web/session/{id}")).await;

        // Then the composer offers send, not cancel — the send
        // button's declarative click posts the send endpoint spelled
        // from the contract.
        assert!(
            body.contains(&format!("@post('{}'", crate::contract::SEND_PATH)),
            "{body}"
        );
        assert!(
            !body.contains(&format!(
                "{}=\"{}\"",
                crate::contract::ACTION_DATA_ATTR,
                crate::contract::ACTION_CANCEL
            )),
            "{body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn composer_shows_cancel_while_the_session_is_busy() {
        // Given a live session in an awaiting phase.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .begin_streaming();

        // When rendering the session page.
        let (_status, body) = get(&router, &format!("/web/session/{id}")).await;

        // Then the composer offers cancel, not send — the cancel
        // button's declarative click posts the cancel endpoint
        // spelled from the contract.
        assert!(
            body.contains(&format!("@post('{}'", crate::contract::CANCEL_PATH)),
            "{body}"
        );
        assert!(
            !body.contains(&format!(
                "{}=\"{}\"",
                crate::contract::ACTION_DATA_ATTR,
                crate::contract::ACTION_SEND
            )),
            "{body}"
        );
    }
}

/// Persona: the TUI confirm's semantics through the web endpoint.
mod persona_actions {
    use super::*;

    /// Seeds the personas cell the harness registers: the first persona
    /// carries a compaction block, the second does not — the two shapes
    /// every persona-switch behavior branches on.
    fn seed_personas(slices: &Slices) {
        let cell: jinn_slices::cell::TypedCell<jinn_persona_msg::Personas> = slices
            .reader(&jinn_persona_msg::personas_slot())
            .expect("the harness registered the personas cell");
        cell.update(|personas: &mut jinn_persona_msg::Personas| {
            personas.entries = vec![
                jinn_slices::Persona {
                    name: "blockful".to_owned(),
                    description: "carries a compaction block".to_owned(),
                    body: String::new(),
                    handoff_prompt: "<compaction>x</compaction>".to_owned(),
                },
                jinn_slices::Persona {
                    name: "blockless".to_owned(),
                    description: "carries none".to_owned(),
                    body: String::new(),
                    handoff_prompt: String::new(),
                },
            ];
        });
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persona_switch_publishes_update_app_state_and_mark_session_interacted() {
        // Given a live session, the personas cell seeded, and the
        // router over them.
        let (state, audit, slices, router) = harness();
        let id = seed_session(&state);
        seed_personas(&slices);

        // When posting a persona pick for the blockful persona.
        let (status, body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=blockful"),
        )
        .await;

        // Then the ack is OK and the bus names exactly the message set
        // the TUI confirm publishes — the app-state seed and the
        // interaction mark, no new schema.
        assert_eq!(status, StatusCode::OK);
        let published =
            audit.of_type::<jinn_preferences_config::protocol::app_state_command::UpdateAppState>();
        let [update] = published.as_slice() else {
            panic!(
                "exactly one UpdateAppState was published, got {}",
                published.len()
            )
        };
        assert!(
            matches!(
                update.updates.as_slice(),
                [jinn_preferences_config::protocol::app_state_command::AppStateUpdate::SetPersona(Some(name))] if name == "blockful"
            ),
            "{update:?}"
        );
        let marks = audit.of_type::<jinn_session_msg::MarkSessionInteracted>();
        let [mark] = marks.as_slice() else {
            panic!(
                "exactly one MarkSessionInteracted was published, got {}",
                marks.len()
            )
        };
        assert_eq!(mark.session_id, id);
        // And the response is the re-rendered modal with the new
        // active mark (the stale-fragment pattern — the modal, not the
        // old row, is what the response swaps). The SSE frames carry
        // the markup line-folded, so the two attributes are matched
        // independently on the active row's own div.
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::PERSONA_MODAL_ID)),
            "{body}"
        );
        let row = body
            .split_once(&format!(
                "{}=\"blockful\"",
                crate::contract::PERSONA_NAME_DATA_ATTR
            ))
            .and_then(|(before, _)| before.rsplit("<div").next().map(str::to_owned))
            .expect("the blockful row renders");
        assert!(
            row.contains(&format!(
                "{}=\"true\"",
                crate::contract::PERSONA_ACTIVE_ATTR
            )),
            "the new persona's row is the marked one: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persona_switch_applies_the_viewed_sessions_persona_before_publishing() {
        // Given a live session and the seeded personas cell.
        let (state, _audit, slices, router) = harness();
        let id = seed_session(&state);
        seed_personas(&slices);

        // When posting a persona pick.
        post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=blockful"),
        )
        .await;

        // Then the session already carries the persona — the write
        // precedes the publish, so the response fragments re-render
        // from state that holds the pick.
        assert_eq!(
            state.read().session.get(&id).expect("live").persona_name(),
            "blockful"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_blockless_persona_switch_answers_with_the_transcript_carrying_the_error_entry() {
        // Given a live session and the seeded personas cell.
        let (state, _audit, slices, router) = harness();
        let id = seed_session(&state);
        seed_personas(&slices);

        // When posting a persona pick for the persona with no
        // compaction block.
        let (status, body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=blockless"),
        )
        .await;

        // Then the transcript swap is part of the response (the
        // fan-out handles no chat-entry event; the POST response is
        // the only path that shows the entry without a reload), the
        // entry carries the exact message the TUI pushes, and the
        // state already holds it. The SSE frames line-fold the
        // markup, so the swap is matched by its selector line.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!("selector #{}", crate::contract::TRANSCRIPT_ID)),
            "the transcript swap is in the response: {body}"
        );
        let expected = jinn_persona_msg::missing_compaction_block_message("blockless");
        // The wire HTML-escapes the body text; compare escaped.
        let expected_escaped = expected
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        assert!(
            body.contains(&expected_escaped),
            "the loud entry renders: {body}"
        );
        let session = state.read().session.get(&id).expect("live").clone();
        assert!(
            session
                .history()
                .iter()
                .any(|entry| entry.text() == expected),
            "the entry is in the history"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_blockful_persona_switch_answers_without_a_transcript_swap() {
        // Given a live session and the seeded personas cell.
        let (state, _audit, slices, router) = harness();
        let id = seed_session(&state);
        seed_personas(&slices);

        // When posting a persona pick for the persona carrying a
        // compaction block.
        let (status, body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=blockful"),
        )
        .await;

        // Then the response swaps the modal and no transcript swap is
        // in it — a blanket transcript would race the fan-out's
        // convergence pushes for no benefit. Matched by selector line
        // (the SSE frames line-fold markup).
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!("selector #{}", crate::contract::PERSONA_MODAL_ID)),
            "{body}"
        );
        assert!(
            !body.contains(&format!("selector #{}", crate::contract::TRANSCRIPT_ID)),
            "no transcript swap rides a blockful switch: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persona_switch_refuses_a_name_outside_the_rendered_rows() {
        // Given a live session and the seeded personas cell.
        let (state, audit, slices, router) = harness();
        let id = seed_session(&state);
        seed_personas(&slices);

        // When posting a persona pick naming a persona the modal never
        // rendered.
        let (status, body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=ghost"),
        )
        .await;

        // Then the request is refused as a client error and nothing is
        // published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("ghost"), "{body}");
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persona_switch_refuses_a_session_absent_from_live_state() {
        // Given the seeded personas cell and no live session.
        let (_state, audit, slices, router) = harness();
        seed_personas(&slices);
        let id = SessionId::new();

        // When posting a persona pick for the absent session.
        let (status, _body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            &format!("session_id={id}&persona=blockful"),
        )
        .await;

        // Then the request is refused (404, the model-switch class)
        // and nothing is published.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn persona_switch_refuses_a_malformed_session_id() {
        // Given the seeded personas cell.
        let (_state, audit, slices, router) = harness();
        seed_personas(&slices);

        // When posting a persona pick with a malformed id.
        let (status, _body) = post_form(
            &router,
            crate::contract::PERSONA_PATH,
            "session_id=not-a-uuid&persona=blockful",
        )
        .await;

        // Then the request is refused as a client error and nothing is
        // published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }
}

mod mcp_actions {
    use super::*;

    /// The MCP section the toggle tests seed: one normal server and
    /// one singleton.
    const MCP_DOC: &str = r#"
[mcp.echo]
command = "echo-bin"

[mcp.shared]
command = "shared-bin"
singleton = true
"#;

    /// The harness over an MCP-seeded config layer: the same shape as
    /// [`harness`], but the shared state's config snapshot carries the
    /// `[mcp.*]` section (the toggle's accepted set).
    fn harness_with_config(doc: &str) -> (State, BusAudit, axum::Router) {
        let slices = Slices::new();
        let provider_cell = slices
            .register(
                jinn_provider_selection_msg::provider_state_slot(),
                jinn_provider_selection_msg::ProviderCell::default(),
            )
            .expect("the provider cell registers once per harness");
        let personas_cell = slices
            .register(
                jinn_persona_msg::personas_slot(),
                jinn_persona_msg::Personas::default(),
            )
            .expect("the personas cell registers once per harness");
        let mcp_runtime_cell = slices
            .register(
                jinn_mcp_msg::mcp_runtime_slot(),
                jinn_mcp_msg::McpRuntimeState::default(),
            )
            .expect("the MCP runtime cell registers once");
        let unread_marks = slices
            .register(
                jinn_webui_msg::webui_unread_slot(),
                jinn_webui_msg::UnreadSessionMarks::default(),
            )
            .expect("the unread-mark cell registers once per harness");
        let state = {
            let app = AppState::default();
            app.session.attach_slices(slices.clone());
            State::new(app)
        };
        let (bus, audit) = BusService::new_recording();
        let fanout = {
            use trouper::system::ActorSystem;
            let system = ActorSystem::new(trouper::system::SystemConfig::production());
            crate::fanout::WebUiFanoutActor::spawn(
                &system,
                state.clone(),
                bus.clone(),
                unread_marks,
            )
        };
        let shared = ServerShared::new(
            state.clone(),
            bus,
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
                jinn_kernel::common::services::test_services::FakeSessionStore,
            )),
            provider_cell,
            personas_cell,
            mcp_runtime_cell,
            jinn_provider_config::ProviderRegistryService::new(harness_registry()),
            jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
            jinn_config::testutil::config_layer(doc),
            fanout,
            8387,
            "test".to_owned(),
        );
        (state, audit, router(shared))
    }

    /// Reads the session's enabled set back from state.
    fn enabled_of(state: &State, id: &SessionId) -> std::collections::BTreeSet<String> {
        state
            .read()
            .session
            .get(id)
            .map_or_default(|session| session.enabled_mcp_servers().clone())
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mcp_toggle_enables_publishes_the_full_set_and_writes_state_first() {
        // Given a live session with no servers enabled and a config
        // seeding two servers.
        let (state, audit, router) = harness_with_config(MCP_DOC);
        let id = seed_session(&state);

        // When posting a toggle naming `echo`.
        let (status, body) = post_form(
            &router,
            crate::contract::MCP_TOGGLE_PATH,
            &format!("session_id={id}&server=echo"),
        )
        .await;

        // Then the ack is OK, state already holds the enabled set (the
        // write happened synchronously, before the publish), and the
        // bus names exactly one McpEnablementChanged carrying the FULL
        // desired set — the documented contract, never a delta.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            enabled_of(&state, &id),
            ["echo".to_owned()].into_iter().collect()
        );
        let published = audit.of_type::<jinn_mcp_msg::McpEnablementChanged>();
        let [message] = published.as_slice() else {
            panic!(
                "exactly one McpEnablementChanged was published, got {}",
                published.len()
            )
        };
        assert_eq!(message.session_id, id);
        assert_eq!(
            message.enabled,
            ["echo".to_owned()].into_iter().collect(),
            "the published set is the full desired set"
        );
        // And the response is the re-rendered modal with the enabled
        // mark on the echo row (the stale-fragment pattern). The SSE
        // frames line-fold the markup, so the body is unfolded first
        // (every `data:` continuation line rejoined) and the row's own
        // div-to-</div> fragment is the probed slice.
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::MCP_MODAL_ID)),
            "{body}"
        );
        let unfolded = body.replace("data: elements ", "");
        let row = unfolded
            .split_once(&format!(
                "{}=\"echo\"",
                crate::contract::MCP_SERVER_DATA_ATTR
            ))
            .and_then(|(before, _)| {
                let start = before.rfind("<div")?;
                let end = unfolded.get(start..)?.find("</div>")?;
                unfolded.get(start..start + end).map(str::to_owned)
            })
            .expect("the echo row renders in the response");
        assert!(
            row.contains("mcp-row-enabled"),
            "the echo row is the enabled one: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_second_toggle_disables_and_publishes_the_emptied_set() {
        // Given a live session with `echo` already enabled.
        let (state, audit, router) = harness_with_config(MCP_DOC);
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_enabled_mcp_servers(["echo".to_owned()].into_iter().collect());

        // When posting a toggle naming `echo` again.
        let (status, _body) = post_form(
            &router,
            crate::contract::MCP_TOGGLE_PATH,
            &format!("session_id={id}&server=echo"),
        )
        .await;

        // Then the ack is OK, state holds the emptied set, and the one
        // published message carries it in full.
        assert_eq!(status, StatusCode::OK);
        assert!(enabled_of(&state, &id).is_empty());
        let published = audit.of_type::<jinn_mcp_msg::McpEnablementChanged>();
        let [message] = published.as_slice() else {
            panic!(
                "exactly one McpEnablementChanged was published, got {}",
                published.len()
            )
        };
        assert!(message.enabled.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mcp_toggle_refuses_an_unconfigured_server_without_mutating() {
        // Given a live session and the MCP-seeded config.
        let (state, audit, router) = harness_with_config(MCP_DOC);
        let id = seed_session(&state);

        // When posting a toggle naming a server no `[mcp.<name>]`
        // section defines.
        let (status, body) = post_form(
            &router,
            crate::contract::MCP_TOGGLE_PATH,
            &format!("session_id={id}&server=never-configured"),
        )
        .await;

        // Then it refuses with 400, no message rides the bus, and the
        // session's enabled set is untouched.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("never-configured"), "{body}");
        assert_eq!(audit.names().len(), 0);
        assert!(enabled_of(&state, &id).is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mcp_toggle_refuses_an_unknown_session_id() {
        // Given the MCP-seeded config and no live session.
        let (_state, audit, router) = harness_with_config(MCP_DOC);
        let ghost = SessionId::new();

        // When posting a toggle for it.
        let (status, body) = post_form(
            &router,
            crate::contract::MCP_TOGGLE_PATH,
            &format!("session_id={ghost}&server=echo"),
        )
        .await;

        // Then it refuses with 404 and publishes nothing.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("no live session"), "{body}");
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mcp_toggle_refuses_a_malformed_session_id() {
        // Given the MCP-seeded config.
        let (_state, audit, router) = harness_with_config(MCP_DOC);

        // When posting a toggle whose session id is not a UUID.
        let (status, _body) = post_form(
            &router,
            crate::contract::MCP_TOGGLE_PATH,
            "session_id=not-a-uuid&server=echo",
        )
        .await;

        // Then it refuses with 400 and publishes nothing.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }
}

mod skill_actions {
    use super::*;

    /// Writes `skills` into a session's ephemeral discovered set (the
    /// state write the scan actor's handling performs) — the toggle
    /// and load handlers' accepted set.
    fn discover(state: &State, id: &SessionId, names: &[&str]) {
        let dir = jinn_testutil::tempdir();
        let skills = names
            .iter()
            .map(|name| jinn_skills_msg::Skill {
                name: (*name).to_owned(),
                description: format!("{name} description"),
                body: format!("{name} body"),
                file_path: dir
                    .path
                    .join_all([name, "SKILL.md"])
                    .expect("typed parent joins bare names"),
                base_dir: dir.path.join(name).expect("typed parent joins a bare name"),
                source: jinn_skills_msg::SkillSource::Global,
            })
            .collect();
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(id)
                .set_discovered_skills(skills);
        });
    }

    /// Reads the session's skill filter back from state.
    fn filter_of(state: &State, id: &SessionId) -> Option<jinn_core_types::NameFilter> {
        state
            .read()
            .session
            .get(id)
            .map_or_default(|session| session.skill_filter().cloned())
    }

    /// Slices one row's div-to-`</div>` fragment out of unfolded SSE
    /// markup (the mcp row-slice pattern).
    fn row_of(unfolded: &str, skill: &str) -> String {
        let marker = format!("{}=\"{skill}\"", crate::contract::SKILL_NAME_DATA_ATTR);
        let (before, _after) = unfolded
            .split_once(&marker)
            .unwrap_or_else(|| panic!("a row renders for {skill}: {unfolded}"));
        let start = before
            .rfind("<div")
            .unwrap_or_else(|| panic!("a row div precedes the {skill} marker"));
        let end = unfolded
            .get(start..)
            .and_then(|tail| tail.find("</div>"))
            .unwrap_or_else(|| panic!("the {skill} row closes"));
        unfolded
            .get(start..start + end + "</div>".len())
            .unwrap_or_else(|| panic!("the {skill} row slices cleanly"))
            .to_owned()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_withholds_and_publishes_nothing() {
        // Given a live session with two discovered skills and no
        // filter (the boot state).
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["alpha", "beta"]);

        // When posting a toggle naming `alpha`.
        let (status, body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={id}&skill=alpha"),
        )
        .await;

        // Then the ack is OK, the filter now exists and withholds
        // exactly `alpha` (deny mode — the materialized inherited
        // filter with the one decision carried), and the bus records
        // NO publish (TUI parity: the toggle publishes nothing).
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("a filter materializes");
        assert_eq!(filter.mode, jinn_core_types::FilterMode::Deny);
        assert_eq!(
            filter.names,
            ["alpha".to_owned()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(
            !state
                .read()
                .session
                .get(&id)
                .is_none_or(|session| session.is_skill_enabled("alpha"))
        );
        assert_eq!(audit.names().len(), 0);
        // And the response is the re-rendered modal with the flip on
        // the alpha row (the stale-fragment pattern).
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::SKILL_MODAL_ID)),
            "{body}"
        );
        let unfolded = body.replace("data: elements ", "");
        let row = row_of(&unfolded, "alpha");
        assert!(
            !row.contains("skill-row-enabled"),
            "the toggled-off row renders hollow: {row}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_permits_back_preserving_the_deny_mode() {
        // Given a live session whose filter withholds `alpha` (a
        // previous toggle).
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["alpha", "beta"]);
        state.with_session(|view| {
            view.session.map().get_or_create(&id).set_skill_filter(Some(
                jinn_core_types::NameFilter::deny(["alpha".to_owned()]),
            ));
        });

        // When posting a toggle naming `alpha` again.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={id}&skill=alpha"),
        )
        .await;

        // Then the ack is OK and the same deny filter now names
        // nothing — the name left the deny list, the mode survived.
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("the filter survives");
        assert_eq!(filter.mode, jinn_core_types::FilterMode::Deny);
        assert_eq!(filter.names.len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_preserves_an_allow_mode_filter() {
        // Given a live session under an allow filter permitting only
        // `alpha` (a bot-frozen session, the mode the deny-clobber
        // would demote).
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["alpha", "beta"]);
        state.with_session(|view| {
            view.session.map().get_or_create(&id).set_skill_filter(Some(
                jinn_core_types::NameFilter {
                    mode: jinn_core_types::FilterMode::Allow,
                    names: ["alpha".to_owned()].into_iter().collect(),
                },
            ));
        });

        // When toggling `beta` (withheld by the allow list) to permit
        // it.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={id}&skill=beta"),
        )
        .await;

        // Then the filter is STILL allow mode and now permits both
        // names — permit added `beta` instead of rewriting the filter
        // as deny(disabled), which would have withheld everything
        // else the allow list was permitting.
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("the filter survives");
        assert_eq!(filter.mode, jinn_core_types::FilterMode::Allow);
        assert_eq!(
            filter.names,
            ["alpha".to_owned(), "beta".to_owned()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        // And the allow-listed skill the click did not touch is
        // untouched in state.
        assert!(
            state
                .read()
                .session
                .get(&id)
                .is_none_or(|session| session.is_skill_enabled("alpha"))
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_on_a_loaded_skill_is_a_silent_no_op() {
        // Given a live session with one discovered skill whose body is
        // pinned in history (the loaded state).
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["pinned"]);
        state.with_session(|view| {
            let session = view.session.map().get_or_create(&id);
            let call =
                jinn_core_types::ChatEntry::tool_call("call-1", "skill", "{\"name\":\"pinned\"}");
            let mut result = jinn_core_types::ChatEntry::tool_result(
                "call-1",
                "skill",
                "<skill name=\"pinned\" location=\"/tmp/pinned/SKILL.md\">\npinned body\n</skill>",
                jinn_core_types::ToolResultStatus::Success,
            );
            result.pin_position = Some(jinn_core_types::PinPosition::Relative);
            session.push_entry(call);
            session.push_entry(result);
        });

        // When posting a toggle naming `pinned`.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={id}&skill=pinned"),
        )
        .await;

        // Then the ack is OK but NOTHING changes: no filter
        // materializes (the absent filter stays absent — the TUI
        // no-op, verbatim) and nothing publishes.
        assert_eq!(status, StatusCode::OK);
        assert!(filter_of(&state, &id).is_none());
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_refuses_a_skill_outside_the_discovered_set() {
        // Given a live session with one discovered skill.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["real"]);

        // When posting a toggle naming a skill that was never
        // rendered.
        let (status, body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={id}&skill=ghost"),
        )
        .await;

        // Then it refuses with 400 and publishes nothing, and no
        // filter materializes.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("discovered skills"), "{body}");
        assert!(filter_of(&state, &id).is_none());
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_refuses_an_unknown_session() {
        // Given no live session carrying the ghost id.
        let (_state, audit, _slices, router) = harness();
        let ghost = SessionId::new();

        // When posting a toggle for it.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            &format!("session_id={ghost}&skill=alpha"),
        )
        .await;

        // Then it refuses with 404 and publishes nothing.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_toggle_refuses_a_malformed_session_id() {
        // Given the default harness.
        let (_state, audit, _slices, router) = harness();

        // When posting a toggle whose session id is not a UUID.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_TOGGLE_PATH,
            "session_id=not-a-uuid&skill=alpha",
        )
        .await;

        // Then it refuses with 400 and publishes nothing.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_load_pins_the_wrapped_pair_and_publishes_interacted() {
        // Given a live session with one discovered, enabled skill.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["alpha"]);

        // When posting a load naming `alpha`.
        let (status, body) = post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={id}&skill=alpha"),
        )
        .await;

        // Then the ack is OK, history holds exactly the pinned pair —
        // the tool call naming the `skill` tool and the success result
        // carrying the wrapped body, pinned relative — and the bus
        // names exactly one MarkSessionInteracted (the only publish).
        assert_eq!(status, StatusCode::OK);
        let (call_name, result_content, pinned) =
            state
                .read()
                .session
                .get(&id)
                .map_or_else(Default::default, |session| {
                    let history = session.history();
                    let call_name = history.iter().rev().find_map(|entry| match &entry.kind {
                        jinn_core_types::ChatEntryKind::ToolCall { name, .. } => Some(name.clone()),
                        _ => None,
                    });
                    let result_content = history.iter().rev().find_map(|entry| match &entry.kind {
                        jinn_core_types::ChatEntryKind::ToolResult { content, .. } => {
                            Some(content.clone())
                        }
                        _ => None,
                    });
                    let pinned = history.iter().filter(|entry| entry.is_pinned()).count();
                    (call_name, result_content, pinned)
                });
        assert_eq!(call_name.as_deref(), Some("skill"));
        assert!(
            result_content
                .as_deref()
                .is_some_and(|content| content.contains("<skill name=\"alpha\" location=\"")),
            "the body rides wrapped: {result_content:?}"
        );
        assert!(pinned >= 1, "the result is pinned");
        let published = audit.of_type::<jinn_session_msg::MarkSessionInteracted>();
        let [message] = published.as_slice() else {
            panic!(
                "exactly one MarkSessionInteracted was published, got {}",
                published.len()
            )
        };
        assert_eq!(message.session_id, id);
        // And the response swaps both the re-rendered modal and the
        // transcript — the pinned pair lands without a reload. The
        // transcript fragment carries entries only (its container id is
        // the patch selector, not markup), so the swap is probed by
        // selector.
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::SKILL_MODAL_ID)),
            "{body}"
        );
        assert!(
            body.contains(&format!("selector #{}", crate::contract::TRANSCRIPT_ID)),
            "{body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_load_auto_enables_a_disabled_skill_durably() {
        // Given a live session with `beta` discovered but withheld by
        // a deny filter.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["beta"]);
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&id)
                .set_skill_filter(Some(jinn_core_types::NameFilter::deny(["beta".to_owned()])));
        });

        // When posting a load naming `beta`.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={id}&skill=beta"),
        )
        .await;

        // Then the ack is OK, the pair pinned, and the filter now
        // permits `beta` (the name left the deny list — the durable
        // auto-enable), with the interaction mark published.
        assert_eq!(status, StatusCode::OK);
        assert!(
            state
                .read()
                .session
                .get(&id)
                .is_none_or(|session| session.is_skill_enabled("beta"))
        );
        let filter = filter_of(&state, &id).expect("the filter survives");
        assert_eq!(filter.names.len(), 0);
        assert_eq!(
            audit
                .of_type::<jinn_session_msg::MarkSessionInteracted>()
                .len(),
            1
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_load_on_a_loaded_skill_appends_the_notice_instead_of_a_pair() {
        // Given a live session where `alpha` is already loaded (a
        // prior load).
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["alpha"]);
        post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={id}&skill=alpha"),
        )
        .await;
        let pairs_before = state.read().session.get(&id).map_or(0, |session| {
            session
                .history()
                .iter()
                .filter(|entry| {
                    matches!(
                        &entry.kind,
                        jinn_core_types::ChatEntryKind::ToolCall { name, .. } if name == "skill"
                    )
                })
                .count()
        });

        // When posting a load naming `alpha` again.
        let (status, body) = post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={id}&skill=alpha"),
        )
        .await;

        // Then the ack is OK, NO second pair appended, the TUI's
        // transient notice appended instead, and no second interaction
        // mark published (the TUI's duplicate branch publishes
        // nothing).
        assert_eq!(status, StatusCode::OK);
        let (pairs_after, notice) =
            state
                .read()
                .session
                .get(&id)
                .map_or_else(Default::default, |session| {
                    let history = session.history();
                    let pairs = history
                    .iter()
                    .filter(|entry| {
                        matches!(
                            &entry.kind,
                            jinn_core_types::ChatEntryKind::ToolCall { name, .. } if name == "skill"
                        )
                    })
                    .count();
                    let notice = history.iter().any(|entry| {
                        matches!(&entry.kind, jinn_core_types::ChatEntryKind::Transient(text)
                        if text == "Skill 'alpha' is already loaded")
                    });
                    (pairs, notice)
                });
        assert_eq!(pairs_after, pairs_before);
        assert!(notice, "the duplicate notice appends");
        assert_eq!(
            audit
                .of_type::<jinn_session_msg::MarkSessionInteracted>()
                .len(),
            1
        );
        // And the response re-renders the transcript, where the notice
        // is visible (askama escapes the apostrophe).
        let unfolded = body.replace("data: elements ", "");
        assert!(
            unfolded.contains("Skill &#39;alpha&#39; is already loaded"),
            "the notice renders into the transcript swap: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_load_refuses_a_skill_outside_the_discovered_set() {
        // Given a live session with one discovered skill.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        discover(&state, &id, &["real"]);

        // When posting a load naming a skill that was never rendered.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={id}&skill=ghost"),
        )
        .await;

        // Then it refuses with 400, appends nothing, and publishes
        // nothing.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn skill_load_refuses_an_unknown_session() {
        // Given no live session carrying the ghost id.
        let (_state, audit, _slices, router) = harness();
        let ghost = SessionId::new();

        // When posting a load for it.
        let (status, _body) = post_form(
            &router,
            crate::contract::SKILL_LOAD_PATH,
            &format!("session_id={ghost}&skill=alpha"),
        )
        .await;

        // Then it refuses with 404 and publishes nothing.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit.names().len(), 0);
    }
}

mod rename_actions {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_applies_the_title_and_publishes_one_persist() {
        // Given a live titled session and the router over it.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_title("Before".to_owned());

        // When posting a rename.
        let (status, body) = post_form(
            &router,
            &format!("/web/session/{id}/rename"),
            "title=Rename+Again",
        )
        .await;

        // Then the ack is OK, the session's live title is the new one,
        // and exactly one PersistSession naming the session published.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(title_of(&state, &id), Some("Rename Again".to_owned()));
        let published = audit.of_type::<jinn_session_store_msg::PersistSession>();
        let [command] = published.as_slice() else {
            panic!("exactly one persist was published, got {}", published.len())
        };
        assert_eq!(command.session_id, id);

        // And the response is the sessions-list fragment (no page
        // shell), carrying the new title as a row.
        assert!(!body.contains("<!DOCTYPE html>"), "{body}");
        assert!(body.contains("Rename Again"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_trims_surrounding_whitespace_off_the_title() {
        // Given a live session.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When posting a rename whose body value carries padding.
        let (status, _body) = post_form(
            &router,
            &format!("/web/session/{id}/rename"),
            "title=%20+Padded+Title+%20",
        )
        .await;

        // Then the applied title is the trimmed text — the TUI
        // rename's own trim, mirrored.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(title_of(&state, &id), Some("Padded Title".to_owned()));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_refuses_an_empty_after_trim_title() {
        // Given a live titled session.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_title("Keep".to_owned());

        // When posting a rename whose title is whitespace only.
        let (status, _body) = post_form(
            &router,
            &format!("/web/session/{id}/rename"),
            "title=%20+++%20",
        )
        .await;

        // Then the request is refused 400, nothing published, and the
        // title unchanged.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
        assert_eq!(title_of(&state, &id), Some("Keep".to_owned()));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_refuses_a_malformed_session_id() {
        // Given the router.
        let (_state, audit, _slices, router) = harness();

        // When posting a rename to a path id that is not a UUID.
        let (status, _body) =
            post_form(&router, "/web/session/not-a-uuid/rename", "title=Whatever").await;

        // Then the request is refused 400 and nothing published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_refuses_a_session_absent_from_live_state() {
        // Given the router over an empty state.
        let (_state, audit, _slices, router) = harness();
        let id = SessionId::new();

        // When posting a rename for the unknown id.
        let (status, body) =
            post_form(&router, &format!("/web/session/{id}/rename"), "title=Ghost").await;

        // Then the request is refused 404 (a well-formed request naming
        // nothing), nothing published.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit.names().len(), 0);
        assert!(body.contains("no live session with id"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_renames_a_bot_session_like_any_other() {
        // Given a live bot session.
        let (state, audit, _slices, router) = harness();
        let id = seed_session(&state);
        let bot = {
            let guard = state.read();
            let source = guard.session.get(&id).expect("seeded session");
            jinn_session_state::ChatSessionState::new_bot(source, true)
        };
        let bot_id = bot.session_id().clone();
        state.write().session.insert(bot);

        // When posting a rename.
        let (status, _body) = post_form(
            &router,
            &format!("/web/session/{bot_id}/rename"),
            "title=Watchdog+Title",
        )
        .await;

        // Then the rename lands — the bot refusal exists only on the
        // text-delivery path, exactly as the TUI rename behaves.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(title_of(&state, &bot_id), Some("Watchdog Title".to_owned()));
        assert_eq!(audit_count(&audit, "PersistSession"), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn rename_push_and_response_carry_the_same_merged_list() {
        // Given a live session, one store-only archived row the live
        // map never carries, and a subscriber to the fan-out's push.
        let (state, _audit, mut rx, router) = harness_with_archived_row_and_receiver();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_title("Fresh rename".to_owned());

        // When posting the rename.
        let (status, body) = post_form(
            &router,
            &format!("/web/session/{id}/rename"),
            "title=Fresh+rename",
        )
        .await;

        // Then the push the handler emitted carries the archived row
        // too — the same merged shape the response serves. A push of
        // the live-only shape would wipe the archived row from every
        // connected sidebar one frame before the response restores it.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Archived from last run"), "{body}");
        let envelope = rx.try_recv().expect("the rename push");
        assert_eq!(envelope.kind, jinn_webui_msg::SseKind::Sessions);
        assert_eq!(envelope.session_id, id);
        assert!(
            envelope.html.contains("Archived from last run"),
            "push must carry the merged list, got: {}",
            envelope.html
        );
        assert!(envelope.html.contains("Fresh rename"), "{}", envelope.html);
        // And exactly one sessions push went out — no duplicate frames
        // for the same refresh.
        assert!(rx.try_recv().is_err());
    }
}

/// SSE catch-up: the reconnect path (render_catchup_frames is pure;
/// the endpoint's live half rides the fan-out broadcast).
mod stream_catchup {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn catchup_frames_cover_only_entries_after_the_cursor() {
        // Given a live session with three entries and a client that saw
        // the first one.
        let (state, _audit, _slices, _router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::assistant("second"));
            session.push_entry(jinn_core_types::ChatEntry::assistant("third"));
        }
        let guard = state.read();
        let session = guard.session.get(&id).expect("seeded session");
        let transcript = crate::transcript::TranscriptModel::from_live(
            session.history(),
            session.title(),
            "model",
            session.phase().as_str(),
            session.is_bot(),
        );
        drop(guard);

        // When building the catch-up events from cursor 1.
        let events =
            crate::server::render_catchup_events(transcript.since(Some(1)), 1, transcript.len());

        // Then one entry event arrives — everything after cursor 1 —
        // and the trailing park advances the resume cursor to the
        // length while bumping chatTick so the reader's scroll effect
        // follows the catch-up tail: the entry's `id:` is 2 (cursor
        // 1 + 1) and the trailing event is the tick signal patch.
        let [entry, trailing] = events.as_slice() else {
            panic!(
                "an entry event and the trailing event, got {}",
                events.len()
            )
        };
        assert_eq!(entry.id.as_deref(), Some("2"));
        let entry_data = entry.data.join("\n");
        assert!(entry_data.contains("third"), "{entry_data}");
        assert_eq!(trailing.id.as_deref(), Some("2"));
        assert_eq!(
            trailing.data.join(""),
            format!("signals {{\"{}\":2}}", crate::contract::SIGNAL_CHAT_TICK)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn catchup_frames_with_nothing_missed_still_advance_the_cursor() {
        // Given a transcript and a client already current.
        let transcript = crate::transcript::TranscriptModel::from_live(
            &[jinn_core_types::ChatEntry::user("only")],
            Some("t"),
            "model",
            "idle",
            false,
        );

        // When building catch-up from the full cursor.
        let total = transcript.len();
        let events = crate::server::render_catchup_events(transcript.since(Some(1)), 1, total);

        // Then no entry events arrive, and one park event advances the
        // resume cursor so the client does not re-catch-up forever —
        // with the chatTick bump (nothing rendered, so a reader at the
        // bottom stays there; the effect's scroll is a no-op).
        let [trailing] = events.as_slice() else {
            panic!("only the trailing event, got {}", events.len())
        };
        assert_eq!(trailing.id.as_deref(), Some("1"));
        assert_eq!(
            trailing.data.join(""),
            format!("signals {{\"{}\":1}}", crate::contract::SIGNAL_CHAT_TICK)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_stream_route_emits_catchup_then_stays_open() {
        // Given a live session with entries.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::user("hello"));
        }

        // When opening the stream with a Last-Event-ID of 0, then
        // bounding the read (the stream stays open by design).
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/web/stream/{id}"))
                    .header("last-event-id", "0")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then the response is SSE.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        let body = aggregate_body(response.into_body()).await;

        // Then the response is SSE carrying the catch-up entry as a
        // patch-elements append (cursor 1 on the id line), and
        // keepalive comments.
        assert!(body.contains("event: datastar-patch-elements"), "{body}");
        assert!(body.contains("id: 1"), "{body}");
        assert!(body.contains("hello"), "{body}");
    }

    /// Polls the body with bounded reads — enough for the
    /// stream yields its catch-up frames on the first polls.
    async fn aggregate_body(body: Body) -> String {
        use http_body_util::BodyExt as _;
        let mut body = body;
        let mut out = Vec::new();
        for _ in 0..32 {
            let polled =
                tokio::time::timeout(std::time::Duration::from_millis(200), body.frame()).await;
            if let Ok(Some(Ok(data))) =
                polled.map(|frame| frame.map(|frame| frame.expect("frame").into_data()))
            {
                out.extend_from_slice(&data);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// The AC1 whole-flow: one router serving every read and action the
/// browser page drives.
mod whole_flow {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn the_page_surface_serves_every_route_end_to_end() {
        // Given a live session, a seeded model cache, and the router.
        let (state, audit, slices, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded")
            .set_title("Trip plan".to_owned());
        slices
            .reader::<jinn_provider_selection_msg::ProviderCell>(
                &jinn_provider_selection_msg::provider_state_slot(),
            )
            .expect("provider cell registered")
            .update(|cell| {
                cell.model_cache = Some(jinn_provider_config::ModelCache {
                    entries: std::collections::HashMap::from([(
                        "openai".to_owned(),
                        vec![jinn_provider::ModelInfo {
                            id: "openai/gpt-4o".to_owned(),
                            context_length: None,
                            input_modalities: jinn_provider::InputModalities::default(),
                        }],
                    )]),
                    last_updated_at: None,
                });
            });

        // When the page's requests fire in one pass: home shell,
        // session page, sessions list, transcript, model picker, send,
        // model switch, session switch, rename.
        let home = get(&router, "/web").await;
        let page = get(&router, &format!("/web/session/{id}")).await;
        let sessions = get(&router, "/web/sessions").await;
        let transcript = get(&router, &format!("/web/transcript/{id}")).await;
        let picker = get(&router, &format!("/web/model-picker/{id}")).await;
        let vendor = get(
            &router,
            &format!(
                "/vendor/{}",
                crate::vendor::versioned_path(crate::contract::VENDOR_APP_JS)
                    .expect("entry module is a row")
            ),
        )
        .await;
        let health = get(&router, "/healthz").await;
        let send = post_form(
            &router,
            "/web/send",
            &format!("session_id={id}&text=pack+light"),
        )
        .await;
        let model = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=openai%2Fgpt-4o"),
        )
        .await;
        let switch = post_form(&router, &format!("/web/session/{id}/activate"), "").await;
        let rename = post_form(
            &router,
            &format!("/web/session/{id}/rename"),
            "title=Trip+plan+renamed",
        )
        .await;

        // Then every read answers 200 and names its content.
        let reads = [
            ("home", &home),
            ("session page", &page),
            ("sessions list", &sessions),
            ("transcript", &transcript),
            ("model picker", &picker),
        ];
        for (name, (status, body)) in reads {
            assert_eq!(*status, StatusCode::OK, "{name}: {body}");
        }
        assert!(
            home.1.contains(&format!(
                "src=\"{}{}\"",
                contract::VENDOR_PREFIX,
                crate::templates::app_js_versioned_path()
            )),
            "home: {}",
            home.1
        );
        assert!(page.1.contains("Trip plan"), "page: {}", page.1);
        assert!(
            sessions.1.contains(&id.to_string()),
            "sessions: {}",
            sessions.1
        );
        assert!(picker.1.contains("openai/gpt-4o"), "picker: {}", picker.1);
        assert_eq!(vendor.0, StatusCode::OK);
        assert_eq!(health.0, StatusCode::OK);

        // And every action published its schema — send, model, switch,
        // rename.
        assert_eq!(send.0, StatusCode::OK);
        assert_eq!(model.0, StatusCode::OK);
        assert_eq!(switch.0, StatusCode::OK);
        assert_eq!(rename.0, StatusCode::OK);
        assert!(
            rename.1.contains("Trip plan renamed"),
            "rename: {}",
            rename.1
        );
        let names = audit.names();
        for schema in [
            "EnqueueUserMessage",
            "ProviderSwitch",
            "SessionLoadRequested",
            "PersistSession",
        ] {
            assert!(
                names.iter().any(|n| n.ends_with(schema)),
                "{schema} missing from {names:?}"
            );
        }
    }
}

/// The bare-host root.
mod root {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn the_bare_host_root_redirects_to_web() {
        // Given the router.
        let (_state, _audit, _slices, router) = harness();

        // When getting the root path.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then the response redirects to /web — a bare visit to the
        // host never lands on a 404.
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/web")
        );
    }
}

/// Transient entries: rendered with TUI parity (muted notices), never
/// a crash. Persistence still excludes them (the store filters before
/// SQLite); this module pins the **web render** behavior.
mod transients {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn a_live_transient_entry_renders_its_body_muted() {
        // Given a live session whose history carries a transient
        // (boot, model refresh, and skills refresh all push these).
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::user("before"));
            session.push_entry(jinn_core_types::ChatEntry::transient(
                "Refreshing models...",
            ));
            session.push_entry(jinn_core_types::ChatEntry::user("after"));
        }

        // When pulling the full transcript.
        let (status, body) = get(&router, &format!("/web/transcript/{id}")).await;

        // Then the transient renders with its body (TUI parity — the
        // web shows what the terminal shows), escaped, and carries the
        // transient class the CSS mutes.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("before"), "{body}");
        assert!(body.contains("after"), "{body}");
        assert!(body.contains("Refreshing models..."), "{body}");
        assert!(body.contains("entry transient"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_transient_entry_participates_in_the_cursor_space() {
        // Given a session with one transient between two entries.
        let (state, _audit, _slices, _router) = harness();
        let id = seed_session(&state);
        {
            let mut guard = state.write();
            let session = guard.session.get_mut(&id).expect("seeded session");
            session.push_entry(jinn_core_types::ChatEntry::user("one"));
            session.push_entry(jinn_core_types::ChatEntry::transient(
                "Restarting MCP server",
            ));
            session.push_entry(jinn_core_types::ChatEntry::user("two"));
        }

        // When projecting the transcript and stamping the cursor the
        // fan-out stamps it (rendered count).
        let guard = state.read();
        let session = guard.session.get(&id).expect("seeded session");
        let transcript = crate::transcript::TranscriptModel::from_live(
            session.history(),
            session.title(),
            "model",
            session.phase().as_str(),
            session.is_bot(),
        );
        let stamped = session
            .history()
            .iter()
            .filter(|e| crate::transcript::is_renderable(e))
            .count();

        // Then the cursor space counts every entry including the
        // transient — pull and push share the one projection, so the
        // space cannot drift — and a client at that cursor re-pulls
        // nothing.
        assert_eq!(transcript.len(), 3);
        assert_eq!(stamped, transcript.len());
        assert!(transcript.since(Some(stamped)).is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_archived_transient_renders_the_same_way() {
        // Given a snapshot carrying a transient — metadata taken from a
        // real capture, since the metadata struct is wide. (Live
        // persistence filters transients before SQLite, so a store
        // round-trip would not carry one; the projection handles one
        // arriving through the snapshot shape anyway.)
        let mut snapshot = jinn_session_state::ChatSessionState::new().capture_snapshot();
        snapshot.entries = vec![
            jinn_core_types::ChatEntry::user("kept"),
            jinn_core_types::ChatEntry::transient("UI-only"),
        ];

        // When projecting it.
        let transcript = crate::transcript::TranscriptModel::from_snapshot(
            &snapshot, None, "model", "idle", false,
        );

        // Then both entries land in the cursor space, the transient
        // rendered like the live path renders it.
        assert_eq!(transcript.len(), 2);
        let [kept, notice] = transcript.entries.as_slice() else {
            panic!("two rendered entries, got {}", transcript.len())
        };
        assert!(kept.body_html.contains("kept"));
        assert_eq!(notice.class, "transient");
        assert!(notice.body_html.contains("UI-only"));
    }
}

/// The switch flow's markup contract.
mod switch_markup {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn the_session_row_targets_an_id_the_page_shell_defines() {
        // Given the page shell and a session row.
        let (state, _audit, _slices, router) = harness();
        let _id = seed_session(&state);
        let (_status, page) = get(&router, "/web").await;
        let (_status, sessions) = get(&router, "/web/sessions").await;

        // Then the row's data attribute names a session and the page
        // defines the root the client's swap replaces — a missing
        // target is a silent no-op click, never a visible error.
        assert!(
            sessions.contains(crate::contract::SESSION_ID_DATA_ATTR),
            "{sessions}"
        );
        assert!(page.contains("id=\"page-root\""), "{page}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn switching_returns_the_sessions_fragment_with_the_new_active_row() {
        // Given two live sessions.
        let (state, audit, _slices, router) = harness();
        let a = seed_session(&state);
        let b = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&b)
            .expect("seeded session")
            .set_title("Second session".to_owned());

        // When activating the second from the web.
        let (status, body) = post_form(&router, &format!("/web/session/{b}/activate"), "").await;

        // Then the response is the navigation patch sequence for B —
        // the page root replaced wholesale inside an SSE body — and
        // the publish rides the shared command.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!(
                "selector #{}\ndata: mode replace",
                crate::contract::PAGE_ROOT_ID
            )),
            "{body}"
        );
        assert!(body.contains("Second session"), "{body}");
        assert_eq!(audit_count(&audit, "SessionLoadRequested"), 1);
        let _ = a;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_converges_the_active_pointer_before_the_response() {
        // Given the router whose switch path publishes to a real store
        // actor — the single writer of the active pointer.
        let (state, _audit, _slices, router) = harness_with_store_actor().await;
        let a = seed_session(&state);
        let b = seed_session(&state);
        state.write().session.set_active(a.clone());

        // When activating the second from the web.
        let (status, body) = post_form(&router, &format!("/web/session/{b}/activate"), "").await;

        // Then the response already names B as the active row: the
        // handler waited for the store actor's `set_active` before
        // rendering, so the swapped-in page is never one click behind.
        assert_eq!(status, StatusCode::OK);
        let active_rows = active_row_ids(&body);
        assert_eq!(active_rows, vec![b.to_string()], "response page: {body}");

        // And live state agrees — the pointer itself moved.
        assert_eq!(state.read().session.active_session_id(), &b);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activating_an_unknown_session_never_hangs_the_request() {
        // Given the router over an empty session map with a real store
        // actor — nothing will ever make an unknown id active.
        let (_state, _audit, _slices, router) = harness_with_store_actor().await;
        let unknown = SessionId::new();

        // When activating it.
        let started = std::time::Instant::now();
        let (status, _body) =
            post_form(&router, &format!("/web/session/{unknown}/activate"), "").await;
        let elapsed = started.elapsed();

        // Then the request answers promptly (the convergence wait is
        // skipped for a session nothing can activate) with the store
        // fall-through's 404 — not a hang.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "unknown-id activate took {elapsed:?}; the convergence wait must be skipped"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_switch_waits_until_the_active_pointer_moves() {
        // Given a live session and a state whose pointer flips to it
        // only after a controlled delay — a stand-in for the store
        // actor's asynchronous `set_active`. The recording bus
        // delivers nothing, so the flip comes only from the task.
        let (state, _audit, _slices, _router) = harness();
        let target = seed_session(&state);
        let flipper_state = state.clone();
        let flipper_target = target.clone();
        let flipper = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            flipper_state.write().session.set_active(flipper_target);
        });
        let (bus, _switch_audit) = BusService::new_recording();

        // When switching to it through the delivery handler directly.
        let started = std::time::Instant::now();
        crate::delivery::handle_switch(&state, &bus, &target.to_string())
            .await
            .expect("the switch publishes");
        let elapsed = started.elapsed();

        // Then the handler returned only after the pointer had moved —
        // the wait did the blocking, and the response would render the
        // new active row. With the wait reverted this returns in
        // microseconds with the pointer unmoved, failing both arms.
        assert!(
            elapsed >= std::time::Duration::from_millis(80),
            "handle_switch returned after {elapsed:?}; it must wait for the pointer"
        );
        assert_eq!(state.read().session.active_session_id(), &target);
        flipper.await.expect("the flipper task joins");
    }
}

mod tools_actions {
    use super::*;

    /// The tools actions harness: the catalog-registered cell registry
    /// (so `AppState::tool_registry` resolves — the pull and the
    /// toggle both read the registry through the shared `State`), a
    /// recording bus, and the router over it. Returns the state (for
    /// seeding and readback), a recording bus (the tools POSTs publish
    /// nothing, so its audit must stay empty), and the router.
    fn harness_with_cells() -> (State, BusAudit, axum::Router) {
        let slices = Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let provider_cell = slices
            .reader::<jinn_provider_selection_msg::ProviderCell>(
                &jinn_provider_selection_msg::provider_state_slot(),
            )
            .expect("the catalog registered the provider slot");
        let personas_cell = slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("the catalog registered the personas slot");
        let mcp_runtime_cell = slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("the catalog registered the MCP runtime slot");
        let unread_marks = slices
            .reader::<jinn_webui_msg::UnreadSessionMarks>(&jinn_webui_msg::webui_unread_slot())
            .expect("the catalog registered the unread slot");
        let state = {
            let app = AppState::default();
            app.frontend.attach_slices(slices.clone());
            app.session.attach_slices(slices.clone());
            State::new(app)
        };
        let (bus, audit) = BusService::new_recording();
        let fanout = {
            use trouper::system::ActorSystem;
            let system = ActorSystem::new(trouper::system::SystemConfig::production());
            crate::fanout::WebUiFanoutActor::spawn(
                &system,
                state.clone(),
                bus.clone(),
                unread_marks,
            )
        };
        let shared = ServerShared::new(
            state.clone(),
            bus,
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
                jinn_kernel::common::services::test_services::FakeSessionStore,
            )),
            provider_cell,
            personas_cell,
            mcp_runtime_cell,
            jinn_provider_config::ProviderRegistryService::new(harness_registry()),
            jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
            jinn_config::empty_config_layer().clone(),
            fanout,
            8387,
            "test".to_owned(),
        );
        (state, audit, router(shared))
    }

    /// Registers `names` as global tool definitions in the registry
    /// cell (the seeded registry shape the TUI picker tests use).
    fn seed_registry(state: &State, names: &[&str]) {
        let cell = state
            .read()
            .tool_registry()
            .expect("the tools registry cell resolves");
        cell.update(|registry| {
            for name in names {
                registry.global.insert(
                    (*name).to_owned(),
                    jinn_core_types::ToolDefinition {
                        name: (*name).to_owned(),
                        description: format!("{name} description"),
                        parameters: serde_json::json!({ "type": "object" }),
                        prompt_snippet: None,
                        prompt_guidelines: vec![],
                        server_tool_type: None,
                    },
                );
            }
        });
    }

    /// Reads the session's tool filter back from state.
    fn filter_of(state: &State, id: &SessionId) -> Option<jinn_core_types::NameFilter> {
        state
            .read()
            .session
            .get(id)
            .and_then(|session| session.tool_filter().cloned())
    }

    /// The toggle POST's urlencoded body: the session and the clicked
    /// tool, nothing else.
    fn toggle_body(id: &SessionId, tool: &str) -> String {
        format!("sessionId={id}&tool={tool}")
    }

    /// The revert POST's urlencoded body: the session and the raw
    /// snapshot string (absence posts `null`).
    fn revert_body(id: &SessionId, snapshot: &str) -> String {
        format!("sessionId={id}&snapshot={snapshot}")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_toggle_commits_a_deny_filter_over_the_post_click_off_rows() {
        // Given a live session with two tools and no filter.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit"]);

        // When toggling `bash` off.
        let (status, body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // Then the ack is OK, and the session's filter is a deny filter
        // withholding exactly the post-click off-rows — the TUI
        // confirm's semantic, committed server-side.
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("the toggle committed a filter");
        assert_eq!(filter.mode, jinn_core_types::FilterMode::Deny);
        assert_eq!(filter.names, ["bash".to_owned()].into_iter().collect());

        // And the response is the re-rendered rows (the
        // stale-fragment pattern, rows-only) with the row marks
        // correct: the toggled row off, the untouched row on. The
        // rows-only swap never re-renders the modal skeleton or the
        // revert stamp — a toggle must not re-execute the stamp's
        // `data-init` nor nest a second modal inside the open one.
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_ROWS_ID)),
            "{body}"
        );
        assert!(
            !body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_MODAL_ID)),
            "{body}"
        );
        assert!(
            !body.contains(&format!(
                "id=\"{}\"",
                crate::contract::TOOLS_REVERT_STAMP_ID
            )),
            "{body}"
        );
        let row = body
            .split_once(&format!(
                "{}=\"bash\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            ))
            .and_then(|(before, _)| {
                let start = before.rfind("<div")?;
                let end = body.get(start..)?.find("</div>")?;
                body.get(start..start + end).map(str::to_owned)
            })
            .expect("the bash row renders in the response");
        assert!(
            row.contains("tools-row-off"),
            "the toggled row renders off: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_toggle_publishes_nothing() {
        // Given a live session with tools.
        let (state, audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash"]);

        // When toggling `bash` off.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // Then the ack is OK and the bus stays empty — the TUI picker
        // publishes nothing, and the web adds no persistence behavior
        // beyond the TUI's (no PersistSession on this path).
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_second_toggle_adds_the_row_to_the_deny_list() {
        // Given a session already carrying the one-toggle deny filter.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit"]);
        let _ = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // When toggling `edit` off too.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "edit"),
        )
        .await;

        // Then both withheld names ride the deny filter — the modal's
        // off-rows after the second click.
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("the deny filter survives");
        assert_eq!(
            filter.names,
            ["bash".to_owned(), "edit".to_owned()].into_iter().collect()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn toggling_an_off_row_back_on_empties_the_deny_list() {
        // Given a session whose `bash` row is off.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit"]);
        let _ = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // When toggling `bash` back on.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // Then the deny filter withholds nothing — an empty deny list,
        // the whole-registry equivalent (never absence), matching the
        // TUI confirm's derivation.
        assert_eq!(status, StatusCode::OK);
        let filter = filter_of(&state, &id).expect("the deny filter survives");
        assert_eq!(filter.mode, jinn_core_types::FilterMode::Deny);
        assert_eq!(filter.names.len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_toggle_names_a_tool_the_modal_never_renders_is_refused_without_writing() {
        // Given a live session with tools and no filter.
        let (state, audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash"]);

        // When posting a tool name outside the rendered rows.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "nonexistent"),
        )
        .await;

        // Then the refusal is 400 and the bus stays empty.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);

        // And the session's filter is still absent — a refusal writes
        // nothing.
        assert!(filter_of(&state, &id).is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_toggle_names_an_unknown_session_is_refused() {
        // Given the tools-seeded harness.
        let (_state, audit, router) = harness_with_cells();

        // When posting a toggle whose session id no live session
        // carries.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&SessionId::new(), "bash"),
        )
        .await;

        // Then the refusal is 404 and nothing published.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_toggle_with_a_malformed_session_id_is_refused() {
        // Given the tools-seeded harness.
        let (_state, audit, router) = harness_with_cells();

        // When posting a toggle whose session id is not a UUID.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            "sessionId=not-a-uuid&tool=bash",
        )
        .await;

        // Then the refusal is 400 and nothing published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_revert_restores_the_open_time_filter_verbatim() {
        // Given a session whose open-time filter denied `bash`, which
        // a toggle then re-enabled (the deny list emptied).
        let (state, audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit"]);
        let open_time = jinn_core_types::NameFilter::deny(["bash".to_owned()]);
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&id)
                .set_tool_filter(Some(open_time.clone()));
        });
        let _ = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;

        // When the dismissal posts the open-time snapshot.
        let snapshot = serde_json::to_string(&open_time).expect("the snapshot serializes");
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_REVERT_PATH,
            &revert_body(&id, &snapshot),
        )
        .await;

        // Then the session holds the open-time filter again — mode and
        // names, verbatim — and nothing published.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(filter_of(&state, &id), Some(open_time));
        assert_eq!(audit.names().len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_revert_restores_absence_when_the_session_opened_with_none() {
        // Given a session that opened the modal with no filter and a
        // toggle then committed one.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash"]);
        let _ = post_form(
            &router,
            crate::contract::TOOLS_TOGGLE_PATH,
            &toggle_body(&id, "bash"),
        )
        .await;
        assert!(filter_of(&state, &id).is_some(), "the toggle committed");

        // When the dismissal posts the null snapshot.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_REVERT_PATH,
            &revert_body(&id, "null"),
        )
        .await;

        // Then absence is restored as absence — `None` filter back to
        // `None`, not an empty filter of any mode.
        assert_eq!(status, StatusCode::OK);
        assert!(filter_of(&state, &id).is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_revert_carrying_garbage_is_refused_without_writing() {
        // Given a session holding a filter.
        let (state, audit, router) = harness_with_cells();
        let id = seed_session(&state);
        let before = jinn_core_types::NameFilter::deny(["bash".to_owned()]);
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&id)
                .set_tool_filter(Some(before.clone()));
        });

        // When posting a snapshot that is neither null nor a parseable
        // filter.
        let (status, _body) = post_form(
            &router,
            crate::contract::TOOLS_REVERT_PATH,
            &revert_body(&id, "not-a-filter"),
        )
        .await;

        // Then the refusal is 400, the bus stays empty, and the
        // session still holds the pre-revert filter — a refusal writes
        // nothing.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audit.names().len(), 0);
        assert_eq!(filter_of(&state, &id), Some(before));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_pull_serves_the_tui_parity_modal_for_the_viewed_session() {
        // Given a live session with a seeded registry and a live
        // deny filter.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit", "Grep"]);
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&id)
                .set_tool_filter(Some(jinn_core_types::NameFilter::deny(["Grep".to_owned()])));
        });

        // When pulling the tools modal for that session.
        let (status, body) = get(
            &router,
            &format!("{}{id}", crate::contract::TOOLS_PICKER_PREFIX),
        )
        .await;

        // Then the modal renders as a fragment with the row set in the
        // shared order, the withheld row off, the others on, and the
        // revert stamp carrying the open-time filter's JSON.
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("<!DOCTYPE html>"), "{body}");
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_MODAL_ID)),
            "{body}"
        );
        let bash = body
            .find(&format!(
                "{}=\"bash\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            ))
            .expect("the bash row renders");
        let edit = body
            .find(&format!(
                "{}=\"edit\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            ))
            .expect("the edit row renders");
        let grep = body
            .find(&format!(
                "{}=\"Grep\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            ))
            .expect("the Grep row renders");
        assert!(bash < edit && edit < grep, "shared order: {body}");
        let grep_row = body
            .split_once(&format!(
                "{}=\"Grep\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            ))
            .and_then(|(before, _)| {
                let start = before.rfind("<div")?;
                let end = body.get(start..)?.find("</div>")?;
                body.get(start..start + end).map(str::to_owned)
            })
            .expect("the Grep row slices");
        assert!(
            grep_row.contains("tools-row-off"),
            "the withheld row renders off: {body}"
        );
        // The stamp is a quoted HTML attribute: the JSON's quotes
        // render as their entity escapes; the assertion decodes them
        // the way the browser does before parsing the attribute.
        let json = serde_json::to_string(&jinn_core_types::NameFilter::deny(["Grep".to_owned()]))
            .expect("the filter serializes");
        let decoded = body.replace("&#39;", "'").replace("&#34;", "\"");
        assert!(
            decoded.contains(&format!("data-init=\"$toolsRevert = '{json}'\"")),
            "the stamp carries the open-time filter: {decoded}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_rows_pull_serves_the_rows_fragment_alone() {
        // Given a live session with a seeded registry and a deny
        // filter.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash", "edit"]);
        state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&id)
                .set_tool_filter(Some(jinn_core_types::NameFilter::deny(["edit".to_owned()])));
        });

        // When the tick effect's re-pull hits the rows route.
        let (status, body) = get(
            &router,
            &format!("{}{id}", crate::contract::TOOLS_ROWS_PREFIX),
        )
        .await;

        // Then the body is the rows fragment only: both rows render
        // with their marks, and neither the modal id nor the revert
        // stamp ride it — a mid-open re-pull cannot re-execute the
        // stamp's `data-init` and reset the open-time snapshot.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_ROWS_ID)),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "{}=\"bash\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "{}=\"edit\"",
                crate::contract::TOOL_NAME_DATA_ATTR
            )),
            "{body}"
        );
        assert!(
            !body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_MODAL_ID)),
            "the skeleton is not re-rendered: {body}"
        );
        assert!(
            !body.contains(&format!(
                "id=\"{}\"",
                crate::contract::TOOLS_REVERT_STAMP_ID
            )),
            "the stamp is not re-rendered: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_rows_pull_swaps_the_rows_container_only() {
        // Given a live session.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash"]);

        // When the rows route answers.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{}{id}", crate::contract::TOOLS_ROWS_PREFIX))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then it names the rows container as its swap target and
        // REPLACES that element — the fragment carries its own
        // `#tools-rows`, so an inner swap would nest a duplicate.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("datastar-selector")
                .and_then(|value| value.to_str().ok()),
            Some(crate::contract::TOOLS_ROWS_SELECTOR),
        );
        assert_eq!(
            response
                .headers()
                .get("datastar-mode")
                .and_then(|value| value.to_str().ok()),
            Some("outer"),
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_modal_pull_carries_the_tick_effect_against_the_rows_route() {
        // Given a live session.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);
        seed_registry(&state, &["bash"]);

        // When pulling the modal.
        let (status, body) = get(
            &router,
            &format!("{}{id}", crate::contract::TOOLS_PICKER_PREFIX),
        )
        .await;

        // Then the skeleton carries the tick effect, its `@get` target
        // is the rows route for the viewed session, and its selector
        // is the rows container — the signal's re-pull can only touch
        // the rows.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("data-effect="), "{body}");
        assert!(
            body.contains(&format!("{}{id}", crate::contract::TOOLS_ROWS_PREFIX)),
            "the effect targets the rows route: {body}"
        );
        assert!(
            body.contains(crate::contract::TOOLS_ROWS_SELECTOR),
            "the effect swaps the rows container: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_pull_for_a_session_without_a_registry_renders_zero_rows() {
        // Given a live session and an empty registry.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);

        // When pulling the tools modal.
        let (status, body) = get(
            &router,
            &format!("{}{id}", crate::contract::TOOLS_PICKER_PREFIX),
        )
        .await;

        // Then the modal shell renders with zero rows and no error —
        // the pull degrades, never fails.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!("id=\"{}\"", crate::contract::TOOLS_ROWS_ID)),
            "{body}"
        );
        assert!(
            !body.contains(&format!("{}=", crate::contract::TOOL_NAME_DATA_ATTR)),
            "no rows render: {body}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_session_page_carries_the_menu_item_and_host_and_the_landing_carries_neither() {
        // Given a live session.
        let (state, _audit, router) = harness_with_cells();
        let id = seed_session(&state);

        // When loading the session page and the landing page.
        let (page_status, page) = get(&router, &format!("/web/session/{id}")).await;
        let (landing_status, landing) = get(&router, "/web").await;

        // Then the session page carries the Tools item and the modal
        // host span, and the landing page carries neither — the menu
        // gating the recorded persona-item rule holds for the new
        // surface.
        assert_eq!(page_status, StatusCode::OK);
        assert_eq!(landing_status, StatusCode::OK);
        assert!(
            page.contains(&format!("id=\"{}\"", crate::contract::MENU_ITEM_TOOLS_ID)),
            "{page}"
        );
        assert!(
            page.contains(&format!("id=\"{}\"", crate::contract::TOOLS_MODAL_HOST_ID)),
            "{page}"
        );
        assert!(
            !landing.contains(&format!("id=\"{}\"", crate::contract::MENU_ITEM_TOOLS_ID)),
            "{landing}"
        );
        // The modal host span itself is page-shell chrome and renders on
        // every page — the persona and MCP hosts ride the same way. The
        // gated surface is the menu item: without it nothing pulls into
        // the span or opens the modal, so the landing's span stays inert.
    }
}
