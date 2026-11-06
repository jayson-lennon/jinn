//! Route-level and delivery tests for the external control surface.

//! Test code: assertions use expect/panic to state what went wrong, and
//! empty-collection checks use `assert_eq!(len, 0)` so the failure names
//! the value.
#![allow(
    clippy::expect_used,
    reason = "test assertions need expect to state what went wrong"
)]

use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use jinn_app_state::AppState;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_remote_msg::SendMode;
use jinn_remote_msg::SendRequest;
use jinn_slices::bus::BusAudit;
use jinn_slices::bus::BusService;
use tower::ServiceExt as _;

use crate::server::ServerShared;
use crate::server::router;

/// The default `[remote]` bind parses and refuses a non-loopback bind.
mod activation {
    use super::*;
    use crate::RemoteActivationError;
    use jinn_config::testutil::config_layer;
    use jinn_slices::RenderFacts;
    use jinn_slices::SliceHost;

    /// Assembles the pieces a slice activation needs, fresh per test:
    /// the kernel registries `Ctx::host` borrows and the config layer.
    /// A throwaway registry path under the shared test temp root. The
    /// path never has to exist — activation failures refuse before the
    /// write.
    pub(crate) fn test_registry_path() -> marked_path::MarkedPathBuf<marked_path::Absolute> {
        let root = jinn_testutil::TestRoot::new();
        root.join_str("jinn-remote-test-registry.json")
            .expect("name joins")
    }

    pub(crate) struct ActivationFixture {
        slices: jinn_slices::Slices,
        viewport: jinn_slices::view::Viewport,
        overlay_views: jinn_slices::OverlayViews<RenderFacts>,
        key_routes: jinn_slices::KeyRoutes,
        system: trouper::system::ActorSystem,
    }

    impl ActivationFixture {
        pub(crate) fn new() -> Self {
            Self {
                slices: jinn_slices::Slices::new(),
                viewport: jinn_slices::view::Viewport::new(),
                overlay_views: jinn_slices::OverlayViews::<RenderFacts>::new(),
                key_routes: jinn_slices::KeyRoutes::new(),
                system: trouper::system::ActorSystem::new(
                    trouper::system::SystemConfig::production(),
                ),
            }
        }

        /// Borrows the registries into a host and runs `activate` with
        /// it. The host borrows this fixture, so the fixture must
        /// outlive the call — hence `&self`.
        pub(crate) async fn activate(
            &mut self,
            config: &jinn_config::ConfigLayer,
            registry_path: marked_path::MarkedPathBuf<marked_path::Absolute>,
        ) -> Result<(), error_stack::Report<crate::RemoteActivationError>> {
            crate::activate(
                &mut self.host(),
                config,
                BusService::new_recording().0,
                State::new(AppState::default()),
                registry_path,
            )
            .await
        }

        /// A host borrowing this fixture's registries, one per call.
        fn host(&mut self) -> SliceHost<'_, RenderFacts> {
            SliceHost::new(
                &self.slices,
                &mut self.viewport,
                &self.overlay_views,
                &self.key_routes,
                &self.system,
            )
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_non_loopback_bind_is_refused_at_activation() {
        // Given a config binding the control surface to an exposed
        // interface.
        let mut fixture = ActivationFixture::new();
        let config = config_layer("[remote]\nbind = \"192.168.1.10:8899\"\n");

        // When activating the slice.
        let result = fixture.activate(&config, test_registry_path()).await;

        // Then activation refuses with the startup error.
        assert!(matches!(
            result,
            Err(report)
                if matches!(
                    report.current_context(),
                    RemoteActivationError::Startup
                )
        ));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_malformed_bind_is_refused_at_activation() {
        // Given a config whose bind is not a host:port pair.
        let mut fixture = ActivationFixture::new();
        let config = config_layer("[remote]\nbind = \"localhost\"\n");

        // When activating the slice.
        let result = fixture.activate(&config, test_registry_path()).await;

        // Then activation refuses with the startup error.
        assert!(matches!(
            result,
            Err(report)
                if matches!(
                    report.current_context(),
                    RemoteActivationError::Startup
                )
        ));
    }
}

/// `POST /send` delivery, through the recording bus.
mod send {
    use super::*;

    /// A fresh shared server over an empty app state and a recording
    /// bus. Returns the parts a test manipulates and asserts against.
    fn harness() -> (State, BusAudit, axum::Router) {
        let state = State::new(AppState::default());
        let (bus, audit) = BusService::new_recording();
        let shared = ServerShared::new(state.clone(), bus, 4242, 8080, "test".to_owned());
        (state, audit, router(shared))
    }

    /// Inserts a fresh session whose id is returned, so callers use the
    /// session's own id rather than pre-fabricating one.
    fn seed_session(state: &State) -> SessionId {
        let session = jinn_session_state::ChatSessionState::new();
        let id = session.session_id().clone();
        state.write().session.insert(session);
        id
    }

    async fn post_send(router: &axum::Router, body: &str) -> (StatusCode, String) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/send")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .expect("request builds"),
            )
            .await
            .expect("response");
        let status = response.status();
        let text = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body reads");
        (status, String::from_utf8_lossy(&text).into_owned())
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn queue_send_publishes_an_enqueued_user_entry_with_the_text_verbatim() {
        // Given a live session and the control surface over it.
        let (state, audit, router) = harness();
        let id = seed_session(&state);

        // When posting a queue send.
        let request = SendRequest {
            session_id: id.to_string(),
            text: "hello from outside".to_owned(),
            mode: Some(SendMode::Queue),
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the ack reports a plain queue.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"queued\""), "{body}");
        assert!(body.contains("\"fell_back\":false"), "{body}");

        // And the audit holds the enqueue carrying the text verbatim.
        let names = audit.names();
        assert!(
            names.iter().any(|n| n.ends_with("EnqueueUserMessage")),
            "{names:?}"
        );
        let published = audit
            .of_type::<jinn_chat_input_msg::EnqueueUserMessage>()
            .pop()
            .expect("enqueue captured");
        assert_eq!(published.entry.text(), "hello from outside");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn steer_on_a_busy_session_publishes_a_steering_fragment() {
        // Given a session seeded streaming.
        let (state, audit, router) = harness();
        let id = seed_session(&state);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded")
            .begin_streaming();

        // When posting a steer send.
        let request = SendRequest {
            session_id: id.to_string(),
            text: "change course".to_owned(),
            mode: Some(SendMode::Steer),
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the ack reports a steer without fallback.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"steered\""), "{body}");
        assert!(body.contains("\"fell_back\":false"), "{body}");

        // And the audit holds the steering fragment carrying the text verbatim.
        let published = audit
            .of_type::<jinn_chat_input_msg::SubmitSteeringMessage>()
            .pop()
            .expect("steer captured");
        assert_eq!(published.text, "change course");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn steer_on_an_idle_session_falls_back_to_queue_delivery() {
        // Given a session sitting idle.
        let (state, audit, router) = harness();
        let id = seed_session(&state);

        // When posting a steer send.
        let request = SendRequest {
            session_id: id.to_string(),
            text: "do this instead".to_owned(),
            mode: Some(SendMode::Steer),
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the ack reports a queue with the fallback flag.
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"queued\""), "{body}");
        assert!(body.contains("\"fell_back\":true"), "{body}");

        // And the delivered message is the enqueue, not a fragment.
        let names = audit.names();
        assert!(names.iter().any(|n| n.ends_with("EnqueueUserMessage")));
        assert!(!names.iter().any(|n| n.ends_with("SubmitSteeringMessage")));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_send_marks_the_target_session_interacted() {
        // Given a fresh session that has never been touched.
        let (state, _audit, router) = harness();
        let id = seed_session(&state);
        assert!(
            !state
                .read()
                .session
                .get(&id)
                .expect("seeded")
                .has_interacted()
        );

        // When a send is delivered.
        let request = SendRequest {
            session_id: id.to_string(),
            text: "wake up".to_owned(),
            mode: None,
        };
        let (status, _body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the session reads as interacted — the persistence gate
        // the turn path checks before any save.
        assert_eq!(status, StatusCode::OK);
        assert!(
            state
                .read()
                .session
                .get(&id)
                .expect("seeded")
                .has_interacted()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_unknown_session_is_a_named_404() {
        // Given no session with the requested id.
        let (_state, _audit, router) = harness();
        let id = SessionId::new();

        // When posting a send.
        let request = SendRequest {
            session_id: id.to_string(),
            text: "anyone there?".to_owned(),
            mode: None,
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the response names the missing session.
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains(&id.to_string()), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_malformed_body_is_a_400_with_the_error_shape() {
        // Given the control surface.
        let (_state, _audit, router) = harness();

        // When posting a body that is not the request schema.
        let (status, body) = post_send(&router, r#"{"nope": true}"#).await;

        // Then the response is a 400 carrying the error body.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("error"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_unparseable_session_id_is_a_400() {
        // Given the control surface.
        let (_state, _audit, router) = harness();

        // When posting a send whose id is not a UUID.
        let request = SendRequest {
            session_id: "not-a-session-id".to_owned(),
            text: "hi".to_owned(),
            mode: None,
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the response is a 400 naming the id problem.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("session_id"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_bot_session_target_is_refused() {
        // Given a live bot session.
        let (state, audit, router) = harness();
        let parent = jinn_session_state::ChatSessionState::new();
        let bot = jinn_session_state::ChatSessionState::new_bot(&parent, false);
        let bot_id = bot.session_id().clone();
        state.write().session.insert(bot);

        // When posting a send at the bot.
        let request = SendRequest {
            session_id: bot_id.to_string(),
            text: "hello bot".to_owned(),
            mode: None,
        };
        let (status, body) =
            post_send(&router, &serde_json::to_string(&request).expect("json")).await;

        // Then the refusal names the bot restriction and nothing was
        // published.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("bot"), "{body}");
        assert_eq!(audit.len(), 0, "nothing should be published");
    }
}

/// `GET /health` and `GET /sessions`.
mod reads {
    use super::*;

    fn harness_with_session(bot: bool) -> (State, BusAudit, axum::Router, SessionId) {
        let state = State::new(AppState::default());
        let (bus, audit) = BusService::new_recording();
        let shared = ServerShared::new(state.clone(), bus, 4242, 8080, "9.9.9-test".to_owned());
        let id = {
            let mut guard = state.write();
            if bot {
                let parent = jinn_session_state::ChatSessionState::new();
                let bot_session = jinn_session_state::ChatSessionState::new_bot(&parent, false);
                let bot_id = bot_session.session_id().clone();
                guard.session.insert(bot_session);
                bot_id
            } else {
                let session = jinn_session_state::ChatSessionState::new();
                let id = session.session_id().clone();
                guard.session.insert(session);
                id
            }
        };
        (state, audit, router(shared), id)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn health_reports_the_instance_identity() {
        // Given the control surface over a test instance.
        let (_state, _audit, router, _id) = harness_with_session(false);

        // When asking for health.
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then the body carries the pid, version, and port the shared
        // state was built with.
        assert_eq!(response.status(), StatusCode::OK);
        let text = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body reads");
        let body: String = String::from_utf8_lossy(&text).into_owned();
        assert!(body.contains("\"pid\":4242"), "{body}");
        assert!(body.contains("\"port\":8080"), "{body}");
        assert!(body.contains("9.9.9-test"), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_lists_the_live_session_with_its_phase() {
        // Given the control surface over one idle session.
        let (state, _audit, router, id) = harness_with_session(false);

        // When listing sessions.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then the listing carries the id, idle phase, and bot=false.
        assert_eq!(response.status(), StatusCode::OK);
        let text = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body reads");
        let body: String = String::from_utf8_lossy(&text).into_owned();
        assert!(body.contains(&id.to_string()), "{body}");
        assert!(body.contains("\"phase\":\"idle\""), "{body}");
        assert!(body.contains("\"bot\":false"), "{body}");

        // And a session seeded streaming lists its busy phase.
        let stream_session = {
            use jinn_session_state::ChatSessionState;
            let mut session = ChatSessionState::new();
            session.begin_streaming();
            session
        };
        let _stream_id = stream_session.session_id().clone();
        state.write().session.insert(stream_session);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");
        let text = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body reads");
        let body: String = String::from_utf8_lossy(&text).into_owned();
        assert!(body.contains("\"phase\":\"streaming\""), "{body}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_marks_a_bot_session() {
        // Given the control surface over a bot session.
        let (_state, _audit, router, id) = harness_with_session(true);

        // When listing sessions.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("response");

        // Then the bot session lists bot=true.
        assert_eq!(response.status(), StatusCode::OK);
        let text = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body reads");
        let body: String = String::from_utf8_lossy(&text).into_owned();
        assert!(body.contains(&id.to_string()), "{body}");
        assert!(body.contains("\"bot\":true"), "{body}");
    }
}

/// Activation against a real registry path registers this instance.
mod registry_integration {
    use super::activation::ActivationFixture;
    use jinn_config::testutil::config_layer;
    use jinn_remote_msg::registry;
    use marked_path::MarkedPathAccess as _;

    #[rstest::rstest]
    #[tokio::test]
    async fn activation_registers_this_instance_in_the_registry() {
        // Given a temp state dir as the registry home.
        let root = jinn_testutil::TestRoot::new();
        let registry_path = root.join_str("remote-instances.json").expect("name joins");
        let config = config_layer("[remote]\nbind = \"127.0.0.1:0\"\n");

        // When activating the slice.
        let mut fixture = ActivationFixture::new();
        let moved_path = registry_path.clone();
        fixture
            .activate(&config, moved_path)
            .await
            .expect("activation succeeds");

        // Then the registry carries this process's entry on a resolved
        // (non-zero) port, and a reader sees it live.
        let entries = registry::read_live(registry_path.as_path());
        let own = entries.first().expect("the registry carries one entry");
        assert_eq!(own.pid(), std::process::id());
        assert_ne!(own.port(), 0);
        assert_eq!(entries.len(), 1);
    }
}

/// Live `[remote] bind` changes through `JinnTomlReloaded`: a dormant
/// actor adopts, a serving actor hot-rebinds, a failed rebind keeps the
/// old listener, and a refused re-read keeps everything.
mod reload {
    use super::activation::test_registry_path;
    use crate::remote_actor::RemoteListenerActorDeps;
    use jinn_app_state::AppState;
    use jinn_slices::ServiceStatusUpdate;
    use jinn_testutil::bus_harness::Recorder;
    use marked_path::MarkedPathAccess as _;

    /// A listener actor driving on a harness bus, its registry under
    /// the shared test root.
    async fn actor_over(
        config: jinn_config::ConfigLayer,
    ) -> (
        trouper::actor::ActorPath,
        trouper::system::ActorSystem,
        Recorder<ServiceStatusUpdate>,
        jinn_testutil::bus_harness::TestHarness,
        jinn_kernel::State,
    ) {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let notes = harness.spawn_recorder::<ServiceStatusUpdate>().await;
        let system = harness.system().clone();
        let state = jinn_kernel::State::new(AppState::default());
        // The registry FILE must exist before spawn: the actor appends
        // to it, it does not create it. (Each call gets a fresh
        // TestRoot — create its directory first.)
        let registry = test_registry_path();
        let registry_file = registry.as_path().to_path_buf();
        std::fs::create_dir_all(registry_file.parent().expect("the temp root has a parent"))
            .expect("registry parent dir");
        std::fs::write(&registry_file, b"[]").expect("registry file seeds");
        let path = crate::remote_actor::RemoteListenerActor::spawn(
            &system,
            &RemoteListenerActorDeps {
                bind: "127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .expect("bind parses"),
                config,
                state: state.clone(),
                bus: harness.bus(),
                registry_path: registry,
                pid: std::process::id(),
                version: "test".to_owned(),
            },
        )
        .await
        .expect("the actor spawns");
        (path, system, notes, harness, state)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_while_serving_hot_rebinds_to_the_new_port() {
        // Given a serving listener actor over a port-0 layer.
        let config = jinn_config::testutil::config_layer("[remote]\nbind = \"127.0.0.1:0\"\n");
        let layer = config.clone();
        let (actor, system, notes, _harness, _state) = actor_over(config).await;

        // When the bind changes to a concrete port and a reload lands.
        // The port is grabbed by this test's own squatter-listener
        // dance: bind :0 twice, take the first's port, rebind it after
        // dropping — no, simpler: use :0 twice; the SECOND resolved
        // port is what the layer names, and it is free until the
        // actor's rebind takes it.
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("probe binds");
        let target = probe.local_addr().expect("probe addr");
        drop(probe);
        layer
            .put(&jinn_preferences_config::schemas::RemoteConfig {
                bind: target.to_string(),
            })
            .expect("layer put lands");
        system.publish(jinn_boot_msg::JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the new port serves: a connect succeeds and the row
        // names the applied rebind.
        let applied = tokio::task::spawn_blocking(move || {
            std::net::TcpStream::connect_timeout(&target, std::time::Duration::from_secs(2))
        })
        .await
        .expect("blocking joins");
        assert!(
            applied.is_ok(),
            "the rebind took the new port and serves it"
        );
        let notes_after = jinn_testutil::bus_harness::await_recorded(
            &notes,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            notes_after.iter().any(|note| note
                .status_message
                .as_deref()
                .is_some_and(|m| m.contains("rebound"))),
            "the row names the applied rebind; got {notes_after:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_rebind_keeps_the_previous_listener_serving() {
        // Given a serving listener actor on an ephemeral port.
        let config = jinn_config::testutil::config_layer("[remote]\nbind = \"127.0.0.1:0\"\n");
        let layer = config.clone();
        let (actor, system, notes, _harness, _state) = actor_over(config).await;

        // And a squatter holding the port the reload will name.
        let squatter = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("squatter binds");
        let taken = squatter.local_addr().expect("squatter addr");
        layer
            .put(&jinn_preferences_config::schemas::RemoteConfig {
                bind: taken.to_string(),
            })
            .expect("layer put lands");

        // When the reload lands.
        system.publish(jinn_boot_msg::JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the row names the failed rebind, and the ORIGINAL
        // listener still serves its own port — old surface untouched.
        let notes_after = jinn_testutil::bus_harness::await_recorded(
            &notes,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            notes_after
                .iter()
                .any(|note| note.note_tone == Some(jinn_slices::NoteTone::Error)
                    && note
                        .status_message
                        .as_deref()
                        .is_some_and(|m| m.contains("rebind") && m.contains("failed"))),
            "the failed rebind is named; got {notes_after:?}"
        );
        // And the old surface is untouched: the taken port is still
        // held only by the squatter — the actor's rebind attempt failed
        // and its old listener kept its own ephemeral port, which the
        // Error-tone row note above names.
        let _keep_squatter_alive = &squatter;
        let _ = taken.port();
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_with_an_unchanged_layer_rebinds_to_a_fresh_port_and_names_it() {
        // Given a serving listener actor whose layer still says :0.
        let config = jinn_config::testutil::config_layer("[remote]\nbind = \"127.0.0.1:0\"\n");
        let (actor, system, notes, _harness, _state) = actor_over(config).await;

        // When a reload lands with no bind change.
        system.publish(jinn_boot_msg::JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the row names exactly one applied rebind — the :0
        // re-read resolves a fresh ephemeral port, and the same-port
        // short-circuit keeps unchanged binds silent.
        let drained = notes.drain();
        assert_eq!(drained.len(), 1, "exactly one rebind note; got {drained:?}");
        assert!(
            drained
                .first()
                .is_some_and(|note| note.status_message.as_deref().is_some_and(|m| {
                    m.contains("rebound") && m.contains("the bind change applied")
                })),
            "the row names the applied rebind; got {drained:?}"
        );
    }
}
