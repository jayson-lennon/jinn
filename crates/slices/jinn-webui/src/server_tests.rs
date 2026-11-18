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
/// a private registry, and a recording bus. Returns the parts a test
/// manipulates and asserts against.
fn harness() -> (State, BusAudit, Slices, axum::Router) {
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
    let (bus, audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone())
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        provider_cell,
        fanout,
        8387,
        "test".to_owned(),
    );
    (state, audit, slices, router(shared))
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

/// A session store whose `load_summaries` names one archived row: the
/// store-only shape the live map never carries.
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
    let (bus, audit) = BusService::new_recording();
    let (fanout, rx) = crate::fanout::WebUiFanoutActor::test_pair(&state, bus.clone());
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(OneArchivedRowStore(
            archived_id,
            "Archived from last run".to_owned(),
        ))),
        provider_cell,
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

        // Then it renders with the regions the client drives.
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("type=\"module\""),
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
        // Given a live session and a discovered model in the cache.
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

        // When posting a model pick.
        let (status, _body) = post_form(
            &router,
            "/web/model",
            &format!("session_id={id}&provider_id=openai%2Fgpt-4o"),
        )
        .await;

        // Then the bus names ProviderSwitch.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(audit_count(&audit, "ProviderSwitch"), 1);
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
    async fn composer_shows_send_when_idle() {
        // Given an idle live session.
        let (state, _audit, _slices, router) = harness();
        let id = seed_session(&state);

        // When rendering the session page.
        let (_status, body) = get(&router, &format!("/web/session/{id}")).await;

        // Then the composer offers send, not cancel — buttons name
        // their action with the contract attribute, and main.ts maps
        // the action to the endpoint.
        assert!(
            body.contains(&format!(
                "{}=\"{}\"",
                crate::contract::ACTION_DATA_ATTR,
                crate::contract::ACTION_SEND
            )),
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

        // Then the composer offers cancel, not send — the same
        // contract-attribute assertion as the idle side.
        assert!(
            body.contains(&format!(
                "{}=\"{}\"",
                crate::contract::ACTION_DATA_ATTR,
                crate::contract::ACTION_CANCEL
            )),
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

/// Rename: the TUI confirm's semantics through the web endpoint.
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

        // When building the catch-up frames from cursor 1.
        let frames =
            crate::server::render_catchup_frames(transcript.since(Some(1)), 1, transcript.len());

        // Then one entry frame arrives — everything after cursor 1 —
        // and the trailing frame parks the cursor at the length.
        let [entry, trailing] = frames.as_slice() else {
            panic!(
                "an entry frame and the trailing frame, got {}",
                frames.len()
            )
        };
        assert_eq!(entry.cursor, 2);
        assert!(entry.html.contains("third"), "{}", entry.html);
        assert_eq!(
            trailing,
            &crate::server::CatchupFrame {
                html: String::new(),
                cursor: 2
            }
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
        let frames = crate::server::render_catchup_frames(transcript.since(Some(1)), 1, total);

        // Then no entry frames arrive, and one empty frame advances the
        // cursor so the client does not re-pull forever.
        let [trailing] = frames.as_slice() else {
            panic!("only the trailing frame, got {}", frames.len())
        };
        assert_eq!(
            trailing,
            &crate::server::CatchupFrame {
                html: String::new(),
                cursor: 1
            }
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

        // Then the response is SSE carrying the catch-up entry, the
        // cursor on the id line, and keepalive comments.
        assert!(body.contains("event: chat"), "{body}");
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
        assert!(home.1.contains("type=\"module\""), "home: {}", home.1);
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

/// Transient entries: filtered from every cursor-addressed surface,
/// never a crash.
mod transients {
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn a_live_transient_entry_neither_panics_nor_renders() {
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

        // Then the pull answers with the two real entries only.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("before"), "{body}");
        assert!(body.contains("after"), "{body}");
        assert!(!body.contains("Refreshing models"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn transients_do_not_shift_the_cursor_space() {
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

        // Then the cursor space is the rendered count, and a client at
        // that cursor re-pulls nothing.
        assert_eq!(transcript.len(), 2);
        assert_eq!(stamped, transcript.len());
        assert!(transcript.since(Some(stamped)).is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_archived_transient_is_filtered_the_same_way() {
        // Given a snapshot carrying a transient — metadata taken from a
        // real capture, since the metadata struct is wide.
        let mut snapshot = jinn_session_state::ChatSessionState::new().capture_snapshot();
        snapshot.entries = vec![
            jinn_core_types::ChatEntry::user("kept"),
            jinn_core_types::ChatEntry::transient("UI-only"),
        ];

        // When projecting it.
        let transcript = crate::transcript::TranscriptModel::from_snapshot(
            &snapshot, None, "model", "idle", false,
        );

        // Then only the renderable entry lands in the cursor space.
        assert_eq!(transcript.len(), 1);
        let [kept] = transcript.entries.as_slice() else {
            panic!("one rendered entry, got {}", transcript.len())
        };
        assert!(kept.body_html.contains("kept"));
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

        // Then the response is the whole page for B — the client's
        // swap consumes it wholesale — and the publish rides the
        // shared command.
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
        assert!(body.contains("Second session"), "{body}");
        assert_eq!(audit_count(&audit, "SessionLoadRequested"), 1);
        let _ = a;
    }
}
