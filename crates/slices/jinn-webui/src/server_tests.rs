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

        // Then it renders with the regions htmx drives.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("sse-connect"), "missing SSE connect: {body}");
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
        // Given the router over an empty state.
        let (_state, audit, _slices, router) = harness();
        let id = SessionId::new();

        // When posting an activate for an unknown id.
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

        // Then the composer offers send, not cancel.
        assert!(body.contains("/web/send"), "{body}");
        assert!(!body.contains("/web/cancel"), "{body}");
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

        // Then the composer offers cancel, not send.
        assert!(body.contains("/web/cancel"), "{body}");
        assert!(!body.contains("/web/send"), "{body}");
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
        // model switch, session switch.
        let home = get(&router, "/web").await;
        let page = get(&router, &format!("/web/session/{id}")).await;
        let sessions = get(&router, "/web/sessions").await;
        let transcript = get(&router, &format!("/web/transcript/{id}")).await;
        let picker = get(&router, &format!("/web/model-picker/{id}")).await;
        let vendor = get(&router, "/vendor/htmx.min.js").await;
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
        assert!(home.1.contains("sse-connect"), "home: {}", home.1);
        assert!(page.1.contains("Trip plan"), "page: {}", page.1);
        assert!(
            sessions.1.contains(&id.to_string()),
            "sessions: {}",
            sessions.1
        );
        assert!(picker.1.contains("openai/gpt-4o"), "picker: {}", picker.1);
        assert_eq!(vendor.0, StatusCode::OK);
        assert_eq!(health.0, StatusCode::OK);

        // And every action published its schema — send, model, switch.
        assert_eq!(send.0, StatusCode::OK);
        assert_eq!(model.0, StatusCode::OK);
        assert_eq!(switch.0, StatusCode::OK);
        let names = audit.names();
        for schema in [
            "EnqueueUserMessage",
            "ProviderSwitch",
            "SessionLoadRequested",
        ] {
            assert!(
                names.iter().any(|n| n.ends_with(schema)),
                "{schema} missing from {names:?}"
            );
        }
    }
}
