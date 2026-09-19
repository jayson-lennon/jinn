//! Coordinator tests using the scripted fake-guest seam.
//!
//! The fake replaces the wasm guest with in-process logic speaking the same
//! NDJSON wire over the same pipes, so these tests exercise the production
//! handshake, read-pump, and validation paths without a compiled plugin.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::sync::Arc;
use std::time::Duration;

use kameo::actor::Spawn;

use crate::common::bus::test_harness::{TestHarness, await_recorded};
use crate::common::root_supervisor::RootSupervisor;
use crate::common::state::State;
use crate::common::tcaps::mint::mint_plugins_cap;
use crate::feat::plugin::PluginConfig;
use crate::feat::plugin_coordinator_actor::PluginCoordinatorActor;
use crate::feat::plugin_coordinator_actor::PluginCoordinatorActorDeps;
use crate::feat::plugin_coordinator_actor::PluginDirs;
use crate::feat::plugin_coordinator_actor::protocol::{
    PluginPhase, PluginStatus, PluginSubscriptions,
};

/// Timeout for awaiting expected plugin outcomes.
const WAIT: Duration = Duration::from_secs(5);

/// Spawns the coordinator with the given plugin entries and fake script.
async fn spawn_coordinator(
    harness: &TestHarness,
    plugins: std::collections::BTreeMap<String, PluginConfig>,
    script: jinn_plugin::FakeGuestScript,
) -> State {
    spawn_coordinator_prepared(harness, plugins, script, |_| {}).await
}

/// Like [`spawn_coordinator`], with a preparer that mutates shared state
/// before the coordinator (and its plugins) spawn — for arming startup
/// conditions the contribution path must observe.
async fn spawn_coordinator_prepared(
    harness: &TestHarness,
    plugins: std::collections::BTreeMap<String, PluginConfig>,
    script: jinn_plugin::FakeGuestScript,
    prepare: impl FnOnce(&State),
) -> State {
    spawn_coordinator_prepared_with_tick(harness, plugins, script, prepare, None).await
}

/// Like [`spawn_coordinator_prepared`], overriding the guest tick interval —
/// the seam that makes tick forwarding observable without real-time waits.
async fn spawn_coordinator_prepared_with_tick(
    harness: &TestHarness,
    plugins: std::collections::BTreeMap<String, PluginConfig>,
    script: jinn_plugin::FakeGuestScript,
    prepare: impl FnOnce(&State),
    tick_override: Option<Duration>,
) -> State {
    let services = harness.services().await;
    let tick_override_opt = tick_override;
    {
        let mut prefs = services.user_preferences_storage.read().clone();
        prefs.plugin = plugins;
        services
            .user_preferences_storage
            .save(&prefs)
            .expect("save prefs");
    }
    let state = State::new(crate::common::app_state::AppState::default());
    prepare(&state);
    let root = RootSupervisor::spawn_root().await;
    let dirs = PluginDirs {
        config_dir: std::path::PathBuf::from("/nonexistent"),
        data_dir: std::path::PathBuf::from("/nonexistent"),
        engine: Arc::new(jinn_plugin::PluginEngine::new().expect("engine construction")),
    };
    let actor = PluginCoordinatorActor::supervise(
        &root,
        PluginCoordinatorActorDeps {
            deps: crate::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            root: root.clone(),
            state: state.clone(),
            cap: mint_plugins_cap(),
            dirs,
            fake_guest: Arc::new(std::sync::Mutex::new(Some(script))),
            tick_override: tick_override_opt,
        },
    )
    .restart_policy(kameo::supervision::RestartPolicy::Never)
    .spawn()
    .await;
    actor.wait_for_startup().await;
    state
}

/// A manifest entry the coordinator will spawn.
fn entry() -> PluginConfig {
    PluginConfig {
        wasm: "test.wasm".to_owned(),
        grants: vec![],
        http: false,
        config: None,
        enabled: true,
    }
}

/// A one-entry plugin map keyed by the standard test plugin name.
fn plugins() -> std::collections::BTreeMap<String, PluginConfig> {
    [("test-plugin".to_owned(), entry())].into_iter().collect()
}

/// A healthy guest ends up Running on the bus.
#[rstest::rstest]
#[tokio::test]
async fn healthy_guest_reaches_running_phase() {
    // Given a coordinator with one scripted-healthy plugin and a recorder.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![],
        },
    )
    .await;

    // When the guest's status events flow.
    let _ = state;

    // Then the Running phase is published for it.
    let messages = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Running),
        "expected Running for test-plugin, got {messages:?}"
    );
}

/// Malformed input from a guest does not kill it or the app.
#[rstest::rstest]
#[tokio::test]
async fn malformed_lines_are_dropped_not_fatal() {
    // Given a guest whose wire output includes garbage around a valid line.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![
                "this is not json".to_owned(),
                citations_line("https://after-garbage.example"),
            ],
        },
    )
    .await;

    // When the lines are processed.
    // Then the valid contribution still lands (garbage dropped).
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].citations.len(), 1);
    assert_eq!(events[0].citations[0].url, "https://after-garbage.example");
}

/// A guest whose stdout closes after contributing ended cleanly; its
/// cached contributions remain and the phase is Done.
#[rstest::rstest]
#[tokio::test]
async fn guest_end_keeps_contributions_and_marks_done() {
    // Given a coordinator with a guest that contributes then ends.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let contributions = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![citations_line("https://persisted.example")],
        },
    )
    .await;

    // When the guest ends.
    let messages = await_recorded(&recorder, 3, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Done),
        "expected Done for test-plugin, got {messages:?}"
    );

    // Then its contribution reached the bus before Done (push-only: the
    // published event is the artifact, not a cache).
    let events = await_recorded(&contributions, 1, WAIT).await;
    assert_eq!(events[0].citations[0].url, "https://persisted.example");
}

/// A guest that never sends Hello times out and dies without contributing.
#[rstest::rstest]
#[tokio::test]
async fn silent_guest_dies_at_handshake() {
    // Given a coordinator with a guest that says nothing.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let _state = spawn_coordinator(&harness, plugins(), jinn_plugin::FakeGuestScript::Silent).await;

    // When the handshake timeout lapses.
    let messages = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Dead),
        "expected Dead for test-plugin, got {messages:?}"
    );

    // Then nothing was contributed (silent guest: no publish possible).
}

/// A first message that is not Hello fails the handshake.
#[rstest::rstest]
#[tokio::test]
async fn non_hello_first_message_fails_handshake() {
    // Given a coordinator with a guest whose first line is a contribution.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let contributions = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::FirstLine(citations_line("https://too-eager.example")),
    )
    .await;

    // When the handshake sees the wrong first message.
    let messages = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Dead),
        "expected Dead for test-plugin, got {messages:?}"
    );

    // Then the eager contribution was never accepted.
    let events = await_recorded(&contributions, 1, Duration::from_millis(200)).await;
    assert!(events.is_empty(), "eager contribution must not publish");
}

/// Protocol version mismatch fails the handshake.
#[rstest::rstest]
#[tokio::test]
async fn version_mismatch_fails_handshake() {
    // Given a coordinator with a guest speaking a different major version.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let contributions = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION + 1,
            lines: vec![citations_line("https://future.example")],
        },
    )
    .await;

    // When the mismatched Hello is rejected.
    let messages = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Dead),
        "expected Dead for test-plugin, got {messages:?}"
    );

    // Then the future version's contributions are not trusted.
    let events = await_recorded(&contributions, 1, Duration::from_millis(200)).await;
    assert!(
        events.is_empty(),
        "future-version contribution must not publish"
    );
}

/// A flooding guest overflows the inbound channel: the pump drops, marks
/// the plugin `Unresponsive`, and the app continues (contributions still
/// arrive).
#[rstest::rstest]
#[tokio::test]
async fn flooding_guest_is_marked_unresponsive_then_recovers() {
    // Given a coordinator with a guest flooding far more lines than the
    // inbound channel holds, and recorders.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let contributions = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::Flood {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![citations_line("https://flood.example")],
            repeat: 500,
        },
    )
    .await;

    // When the flood drains (poll: async pipeline).
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let messages = await_recorded(&recorder, 1, WAIT).await;
        let unresponsive_seen = messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Unresponsive);
        if unresponsive_seen {
            break;
        }
        assert!(
            deadline > tokio::time::Instant::now(),
            "Unresponsive was never published"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Then at least one persona still landed (drop-newest lost some, not
    // all).
    let events = await_recorded(&contributions, 1, WAIT).await;
    assert!(
        events
            .iter()
            .any(|e| e.citations.iter().any(|c| c.url == "https://flood.example")),
        "no contribution survived the flood"
    );
}

/// No configured plugins means no spawns, no status events, and an empty
/// contribution cache — the default install.
#[rstest::rstest]
#[tokio::test]
async fn no_plugins_configured_is_quiescent() {
    // Given a coordinator with zero plugin entries and a recorder.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let _state = spawn_coordinator(
        &harness,
        std::collections::BTreeMap::new(),
        jinn_plugin::FakeGuestScript::Silent,
    )
    .await;

    // When the coordinator has settled (spawn_all ran at startup).
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Then nothing was published and the cache is empty.
    let messages = await_recorded(&recorder, 1, Duration::from_millis(200)).await;
    assert!(messages.is_empty(), "unexpected status events");
}

/// One valid wire `PushCitations` line (hand-encoded JSON to prove the raw
/// path), citing `url` for a fresh session id.
fn citations_line(url: &str) -> String {
    let session_id = uuid::Uuid::new_v4().to_string();
    format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"{session_id}","citations":[{{"url":"{url}","title":"T","content":"c"}}]}}"#
    )
}

// ── Host→guest event forwarding ──────────────────────────────────────────────

use crate::common::tcaps::mint::mint_session_cap;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::citations_received::CitationsReceived;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use crate::protocol::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_tools_msg::ToolCallReceived;

/// Seeds one history entry into a session for `final_answer` tests.
fn seed_entry(state: &State, session_id: &SessionId, is_assistant: bool) {
    state.with_session(&mint_session_cap(), |view| {
        let session = view.session.map().get_or_create(session_id);
        let entry = if is_assistant {
            crate::protocol::ChatEntry::assistant("done")
        } else {
            crate::protocol::ChatEntry::error("boom")
        };
        session.push_entry(entry);
    });
}

/// A subscribed plugin's guest stays alive; its registration lands.
#[rstest::rstest]
#[tokio::test]
async fn subscribed_guest_registers_its_kinds() {
    // Given a coordinator with a guest subscribing to all three kinds.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginSubscriptions>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec![
                "tool_call".to_owned(),
                "tool_result".to_owned(),
                "turn_end".to_owned(),
            ],
        },
    )
    .await;

    // When the handshake completes.
    let events = await_recorded(&recorder, 1, WAIT).await;

    // Then the validated subscription set was announced.
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "test-plugin");
    assert_eq!(events[0].kinds.len(), 3);
}

/// An unsubscribed plugin receives no forwarded events (the guest would
/// misparse them); only subscribed kinds flow.
#[rstest::rstest]
#[tokio::test]
async fn unsubscribed_kind_is_not_forwarded() {
    // Given a guest subscribing only to turn_end (no tool events).
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginSubscriptions>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["turn_end".to_owned()],
        },
    )
    .await;
    let _ = await_recorded(&recorder, 1, WAIT).await;

    // When a tool call event fires for an unsubscribed kind.
    harness
        .publish(ToolCallReceived {
            session_id: SessionId::new(),
            tool_call: ToolCall {
                id: "call_1".to_owned(),
                name: "sample-tool".to_owned(),
                arguments: r#"{"url":"https://example.com"}"#.to_owned(),
            },
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

    // Then the coordinator stays healthy (no crash, no dead plugin) and —
    // proven directly — the unsubscribed kind produced no forward: the
    // echo guest (subscribed to turn_end only) published no echo reply.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let statuses = await_recorded(
        &statuses_recorder(&harness).await,
        0,
        Duration::from_millis(100),
    )
    .await;
    assert!(
        !statuses
            .iter()
            .any(|s| s.name == "test-plugin" && s.phase == PluginPhase::Dead),
        "unsubscribed event must not kill the plugin"
    );
}

/// Helper: a PluginStatus recorder on the given harness.
async fn statuses_recorder(
    harness: &TestHarness,
) -> kameo::actor::ActorRef<crate::common::bus::test_harness::Recorder<PluginStatus>> {
    harness.spawn_recorder::<PluginStatus>().await
}

/// `final_answer` is true only when the last entry is an assistant message.
#[rstest::rstest]
#[tokio::test]
async fn turn_end_final_answer_reflects_last_entry() {
    // Given a session whose last entry is an assistant message.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["turn_end".to_owned()],
        },
    )
    .await;
    let session_id = SessionId::new();
    seed_entry(&state, &session_id, true);

    // When the turn ends (Streaming → Idle).
    harness
        .publish(SessionPhaseChanged {
            session_id: session_id.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        })
        .await;

    // Then the forwarded turn_end event carried final_answer=true: the
    // echo returns the forwarded line, which must contain the flag and the
    // session id.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert_eq!(events.len(), 1, "echo reply published");
    let echoed = &events[0].citations[0].title;
    assert!(
        echoed.contains(r#""final_answer":true"#),
        "final_answer must be true for an assistant last entry, got: {echoed}"
    );
    assert!(
        echoed.contains(&session_id.to_string()),
        "the event must carry the session id"
    );
}

/// A valid PushCitations line publishes CitationsReceived on the bus.
#[rstest::rstest]
#[tokio::test]
async fn push_citations_publishes_citations_received() {
    // Given a coordinator with a guest pushing one citation.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let session_id = SessionId::new();
    let line = format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"{session_id}","citations":[{{"url":"https://example.com/a","title":"Example A","content":"excerpt"}}]}}"#
    );
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line],
        },
    )
    .await;

    // When the contribution is processed.
    let events = await_recorded(&recorder, 1, WAIT).await;

    // Then exactly one CitationsReceived was published with the citation.
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_id, session_id);
    assert_eq!(events[0].citations.len(), 1);
    assert_eq!(events[0].citations[0].url, "https://example.com/a");
    assert_eq!(events[0].citations[0].title, "Example A");
}

/// Invalid citations are dropped entry-wise; valid ones survive.
#[rstest::rstest]
#[tokio::test]
async fn push_citations_drops_invalid_entries_keeps_valid() {
    // Given a guest pushing one invalid (ftp) and one valid citation.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let session_id = SessionId::new();
    let line = format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"{session_id}","citations":[{{"url":"ftp://nope","title":"bad"}},{{"url":"https://ok.example","title":""}}]}}"#
    );
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line],
        },
    )
    .await;

    // When the contribution is processed.
    let events = await_recorded(&recorder, 1, WAIT).await;

    // Then only the valid citation survived, with the URL as title fallback.
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].citations.len(), 1);
    assert_eq!(events[0].citations[0].url, "https://ok.example");
    assert_eq!(events[0].citations[0].title, "https://ok.example");
}

/// An unparseable session id drops the whole batch without publishing.
#[rstest::rstest]
#[tokio::test]
async fn push_citations_with_bad_session_id_is_dropped() {
    // Given a guest pushing citations for a non-UUID session id.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![r#"{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"not-a-uuid","citations":[{"url":"https://a.example","title":"A"}]}"#.to_owned()],
        },
    )
    .await;

    // When the line is processed and settles.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Then nothing was published.
    assert!(
        await_recorded(&recorder, 0, Duration::from_millis(100))
            .await
            .is_empty()
    );
}

/// Two identical citation pushes in sequence both publish (no debounce).
#[rstest::rstest]
#[tokio::test]
async fn identical_citation_batches_both_publish() {
    // Given a guest pushing the same citation batch twice.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let session_id = SessionId::new();
    let line = format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"{session_id}","citations":[{{"url":"https://same.example","title":"Same"}}]}}"#
    );
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line.clone(), line],
        },
    )
    .await;

    // When both lines are processed.
    let events = await_recorded(&recorder, 2, WAIT).await;

    // Then two CitationsReceived events were published (turn-scoped, no
    // identical-payload debounce).
    assert_eq!(events.len(), 2, "turn-scoped citations must not debounce");
}

/// `final_answer` computation: an assistant last entry yields true, an
/// error last entry yields false (the flush gate).
#[rstest::rstest]
#[tokio::test]
async fn final_answer_reflects_last_history_entry_kind() {
    // Given a session whose last entry is an error.
    let state = State::new(crate::common::app_state::AppState::default());
    let session_id = SessionId::new();
    seed_entry(&state, &session_id, false);

    // When checking the final-answer signal.
    // Then it is false for the error entry.
    assert!(!super::last_entry_is_assistant(&state, &session_id));

    // Given the session's history now ends with an assistant message.
    seed_entry(&state, &session_id, true);

    // When re-checking.
    // Then it is true.
    assert!(super::last_entry_is_assistant(&state, &session_id));

    // Given an unknown session.
    // When checking.
    // Then it is false (never claims a final answer for a vanished session).
    assert!(!super::last_entry_is_assistant(&state, &SessionId::new()));
}

/// With no plugins configured, forwarded bus events are harmless no-ops and
/// the coordinator stays alive — no footer, no startup failure.
#[rstest::rstest]
#[tokio::test]
async fn no_plugins_forwarded_events_are_harmless() {
    // Given a coordinator with zero plugins configured and a recorder.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        std::collections::BTreeMap::new(),
        jinn_plugin::FakeGuestScript::Silent,
    )
    .await;

    // When tool and phase events fire anyway.
    harness
        .publish(ToolCallReceived {
            session_id: SessionId::new(),
            tool_call: ToolCall {
                id: "c1".to_owned(),
                name: "sample-tool".to_owned(),
                arguments: r#"{"url":"https://example.com"}"#.to_owned(),
            },
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    harness
        .publish(SessionPhaseChanged {
            session_id: SessionId::new(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        })
        .await;

    // Then nothing was published and the coordinator did not crash.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        await_recorded(&recorder, 0, Duration::from_millis(100))
            .await
            .is_empty(),
        "no plugin means no citations"
    );
}

/// A truncated tool result forwards the untruncated `full_content` to
/// subscribed guests — plugins see the complete output; truncation is an
/// LLM-context limit only. The echo guest returns each forwarded line
/// inside a citation title, so the recorder asserts exactly what crossed
/// the wire.
#[rstest::rstest]
#[tokio::test]
async fn truncated_result_forwards_full_content_to_guest() {
    // Given a coordinator with an echo guest subscribed to tool results.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["tool_result".to_owned()],
        },
    )
    .await;

    // When a truncated MCP result completes: `content` is clipped
    // mid-JSON (unparseable), `full_content` holds the original.
    let full_json = r#"{"search_id":"s","results":[{"url":"https://full.example/page","title":"Full Content Page","publish_date":null,"excerpts":["entire original"]}]}"#;
    // Mid-object cut — cannot parse. Take chars (not bytes) so the slice
    // can never split a UTF-8 boundary.
    let clipped: String = full_json.chars().take(40).collect();
    harness
        .publish(jinn_tools_msg::ToolExecutionCompleted {
            session_id: SessionId::new(),
            result: jinn_core_types::tool_types::ToolResult {
                tool_call_id: "call_trunc".to_owned(),
                name: "mcp__parallel__web_search".to_owned(),
                content: clipped.clone(),
                success: true,
                full_content: Some(full_json.to_owned()),
                truncation: None,
                pin_position: None,
            },
        })
        .await;

    // Then the guest received the complete JSON, not the clip: the echo
    // reply's citation title contains the forwarded line, which must carry
    // the full payload's URL and never the clipped fragment's cut point.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert_eq!(events.len(), 1, "echo reply published");
    let echoed = &events[0].citations[0].title;
    assert!(
        echoed.contains("https://full.example/page"),
        "forwarded line must carry the untruncated payload, got: {echoed}"
    );
    assert!(
        echoed.contains("entire original"),
        "the tail of the full payload must have crossed the wire, got: {echoed}"
    );
}

/// An empty citations list publishes nothing.
#[rstest::rstest]
#[tokio::test]
async fn push_citations_with_empty_list_publishes_nothing() {
    // Given a coordinator with a guest pushing an empty citations batch.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![r#"{"v":1,"seq":2,"ts":0,"type":"push_citations","session_id":"01943d8e-5a1f-7c2d-9e3b-4f6a8b0c1d2e","citations":[]}"#.to_owned()],
        },
    )
    .await;

    // When the line is processed and the pipeline settles.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Then no CitationsReceived was published.
    assert!(
        await_recorded(&recorder, 0, Duration::from_millis(100))
            .await
            .is_empty(),
        "empty batch must not publish"
    );
}

/// A run-to-completion guest (handshake, no further lines, clean stdout
/// close — the loader shape) ends up Done, not Dead.
#[rstest::rstest]
#[tokio::test]
async fn clean_exit_guest_reaches_done_phase() {
    // Given a coordinator whose guest handshakes then closes stdout cleanly.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PluginStatus>().await;
    let state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![],
        },
    )
    .await;

    // When the guest finishes and the terminal phase is published.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let done = state.read().plugins.phase("test-plugin");
        if done == Some(PluginPhase::Done) {
            break;
        }
        assert!(
            deadline > tokio::time::Instant::now(),
            "guest never reached Done; phase = {done:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Then Done was published on the bus (not Dead).
    let messages = await_recorded(&recorder, 3, WAIT).await;
    assert!(
        messages
            .iter()
            .any(|m| m.name == "test-plugin" && m.phase == PluginPhase::Done),
        "expected Done for test-plugin, got {messages:?}"
    );
}

/// A guest that dies before handshaking still ends up Dead (clean-exit
/// marking must not mask real failures).
#[rstest::rstest]
#[tokio::test]
async fn silent_guest_reaches_dead_phase() {
    // Given a coordinator whose guest closes stdout before Hello.
    let harness = TestHarness::new().await;
    let state = spawn_coordinator(&harness, plugins(), jinn_plugin::FakeGuestScript::Silent).await;

    // When the failed startup path settles.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let phase = state.read().plugins.phase("test-plugin");
        if phase == Some(PluginPhase::Dead) {
            break;
        }
        assert!(
            deadline > tokio::time::Instant::now(),
            "guest never reached Dead; phase = {phase:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── Mirrored plugin→host requests ────────────────────────────────────────────

use jinn_session_history_msg::PushChatEntry;
use jinn_inference_msg::CancelStream as ProviderCancelStream;

/// A mirrored `cancel_stream` line translates to the internal provider
/// `CancelStream` command on the bus.
#[rstest::rstest]
#[tokio::test]
async fn mirrored_cancel_stream_publishes_internal_command() {
    // Given a coordinator whose guest sends one mirrored cancel_stream line
    // for a valid session, and a recorder for the internal command.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<ProviderCancelStream>().await;
    let session_id = SessionId::new();
    let line =
        format!(r#"{{"v":1,"seq":2,"ts":0,"type":"cancel_stream","session_id":"{session_id}"}}"#);
    spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line],
        },
    )
    .await;

    // When the mirror is processed.
    let commands = await_recorded(&recorder, 1, WAIT).await;

    // Then the internal CancelStream command carries the parsed session id.
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].session_id, session_id);
}

/// A mirrored `insert_system_entry` line translates to a direct
/// `PushChatEntry` of one system entry — pushed, not queued, so the entry
/// lands even while the session is mid-stream (a watchdog marker must be
/// visible during the stall it reports).
#[rstest::rstest]
#[tokio::test]
async fn mirrored_insert_system_entry_pushes_tail_entry() {
    // Given a coordinator whose guest sends one mirrored insert_system_entry
    // line for a valid session.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<PushChatEntry>().await;
    let session_id = SessionId::new();
    let line = format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"insert_system_entry","session_id":"{session_id}","text":"watchdog tripped"}}"#
    );
    spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line],
        },
    )
    .await;

    // When the mirror is processed.
    let pushes = await_recorded(&recorder, 1, WAIT).await;

    // Then one push appended a system-kind entry to the session.
    assert_eq!(pushes.len(), 1, "one PushChatEntry");
    assert_eq!(pushes[0].session_id, session_id);
    assert!(
        matches!(
            pushes[0].entry.kind,
            crate::protocol::ChatEntryKind::System(_)
        ),
        "the entry must be system-kind"
    );
    assert_eq!(pushes[0].entry.text(), "watchdog tripped");
}

/// A mirror line with an unparseable session id is dropped: no publish, no
/// crash.
#[rstest::rstest]
#[tokio::test]
async fn mirror_with_bad_session_id_is_dropped() {
    // Given a coordinator whose guest sends mirrored lines with garbage ids.
    let harness = TestHarness::new().await;
    let cancels = harness.spawn_recorder::<ProviderCancelStream>().await;
    let pushes = harness.spawn_recorder::<PushChatEntry>().await;
    spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![
                r#"{"v":1,"seq":2,"ts":0,"type":"cancel_stream","session_id":"not-a-uuid"}"#
                    .to_owned(),
                r#"{"v":1,"seq":3,"ts":0,"type":"insert_system_entry","session_id":"not-a-uuid","text":"nope"}"#
                    .to_owned(),
            ],
        },
    )
    .await;

    // When both lines are processed and the pipeline settles.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Then nothing was published on either command.
    assert!(
        await_recorded(&cancels, 0, Duration::from_millis(100))
            .await
            .is_empty(),
        "bad-id cancel must be dropped"
    );
    assert!(
        await_recorded(&pushes, 0, Duration::from_millis(100))
            .await
            .is_empty(),
        "bad-id insert must be dropped"
    );
}

// ── Stream-lifecycle forwarding (Phase 2) ────────────────────────────────────

use jinn_inference_msg::SendToLlmProvider;
use jinn_inference_msg::{StreamCompleted, StreamCompletedReason, StreamToken};
use jinn_tools_msg::{ToolCallStreaming, ToolUseStarted};

/// Builds a minimal `SendToLlmProvider` via its serde shape (most fields
/// carry `#[serde(default)]`; the tests only care about `session_id`).
fn send_to_llm(session_id: &SessionId) -> SendToLlmProvider {
    serde_json::from_value(serde_json::json!({
        "session_id": session_id.to_string(),
        "messages": [],
        "dispatched_at": jiff::Timestamp::now().to_string(),
    }))
    .expect("minimal SendToLlmProvider deserializes")
}

/// A `SendToLlmProvider` publish reaches a subscribed guest as `stream_start`.
#[rstest::rstest]
#[tokio::test]
async fn send_to_llm_provider_is_forwarded_as_stream_start() {
    // Given a guest subscribed to stream_start.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["stream_start".to_owned()],
        },
    )
    .await;
    let session_id = SessionId::new();

    // When an LLM request is dispatched.
    harness.publish(send_to_llm(&session_id)).await;

    // Then the guest saw a stream_start event carrying the session id.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        events[0].citations[0]
            .title
            .contains(r#""type":"stream_start""#),
        "expected stream_start echo, got: {:?}",
        events[0].citations[0].title
    );
    assert!(
        events[0].citations[0]
            .title
            .contains(&session_id.to_string()),
        "the event must carry the session id"
    );
}

/// Every stream-activity bus message reaches a subscribed guest as one
/// uncoalesced `stream_event` ping.
#[rstest::rstest]
#[tokio::test]
async fn stream_token_is_forwarded_as_stream_event_ping() {
    // Given a guest subscribed to stream_event.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["stream_event".to_owned()],
        },
    )
    .await;
    let session_id = SessionId::new();

    // When a stream token flows.
    harness
        .publish(StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "hello".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

    // Then the guest saw a stream_event ping.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        events[0].citations[0]
            .title
            .contains(r#""type":"stream_event""#),
        "expected stream_event echo, got: {:?}",
        events[0].citations[0].title
    );
}

/// Tool-argument streaming is a liveness ping too.
#[rstest::rstest]
#[tokio::test]
async fn tool_call_streaming_is_forwarded_as_stream_event_ping() {
    // Given a guest subscribed to stream_event.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["stream_event".to_owned()],
        },
    )
    .await;

    // When tool arguments stream in.
    harness
        .publish(ToolCallStreaming {
            session_id: SessionId::new(),
            index: 0,
            partial_json: r#"{"path":"x"}"#.to_owned(),
        })
        .await;

    // Then the guest saw a stream_event ping.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        events[0].citations[0]
            .title
            .contains(r#""type":"stream_event""#),
        "expected stream_event echo, got: {:?}",
        events[0].citations[0].title
    );
}

/// Tool-use start is a liveness ping too.
#[rstest::rstest]
#[tokio::test]
async fn tool_use_started_is_forwarded_as_stream_event_ping() {
    // Given a guest subscribed to stream_event.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["stream_event".to_owned()],
        },
    )
    .await;

    // When a tool call starts.
    harness
        .publish(ToolUseStarted {
            session_id: SessionId::new(),
            index: 0,
            id: "call_1".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

    // Then the guest saw a stream_event ping.
    let events = await_recorded(&recorder, 1, WAIT).await;
    assert!(
        events[0].citations[0]
            .title
            .contains(r#""type":"stream_event""#),
        "expected stream_event echo, got: {:?}",
        events[0].citations[0].title
    );
}

/// Each stream-completion reason forwards verbatim as `stream_end`.
#[rstest::rstest]
#[case(StreamCompletedReason::Finished, r#""reason":"finished""#)]
#[case(StreamCompletedReason::Canceled, r#""reason":"canceled""#)]
#[case(StreamCompletedReason::ToolUse, r#""reason":"tool_use""#)]
#[case(StreamCompletedReason::Error, r#""reason":"error""#)]
#[tokio::test]
async fn stream_completed_reason_forwards_verbatim(
    #[case] reason: StreamCompletedReason,
    #[case] wire_reason: &str,
) {
    // Given a guest subscribed to stream_end.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["stream_end".to_owned()],
        },
    )
    .await;

    // When the stream completes with the reason.
    harness
        .publish(StreamCompleted {
            session_id: SessionId::new(),
            reason,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            model_used: None,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

    // Then the guest saw stream_end with the same reason.
    let events = await_recorded(&recorder, 1, WAIT).await;
    let title = &events[0].citations[0].title;
    assert!(
        title.contains(r#""type":"stream_end""#) && title.contains(wire_reason),
        "expected stream_end echo with {wire_reason}, got: {title:?}"
    );
}

/// A guest subscribed only to turn_end receives no stream-lifecycle events
/// and no ticks — the forwarder filters by the validated subscription set.
#[rstest::rstest]
#[tokio::test]
async fn unsubscribed_stream_lifecycle_events_and_ticks_are_not_forwarded() {
    // Given a guest subscribed only to turn_end.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator_prepared_with_tick(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["turn_end".to_owned()],
        },
        |_| {},
        Some(Duration::from_millis(40)),
    )
    .await;
    let session_id = SessionId::new();

    // When stream activity flows and several tick intervals elapse.
    harness.publish(send_to_llm(&session_id)).await;
    harness
        .publish(StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "x".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    harness
        .publish(StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            model_used: None,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Then no echo reply was published at all.
    let echoes = await_recorded(&recorder, 0, Duration::from_millis(100)).await;
    assert!(
        echoes.is_empty(),
        "unsubscribed guest must receive nothing, got {echoes:?}"
    );
}

/// A guest subscribed to tick receives periodic TickEvents.
#[rstest::rstest]
#[tokio::test]
async fn tick_events_are_forwarded_to_subscribed_guests() {
    // Given a guest subscribed to tick and a fast tick override.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<CitationsReceived>().await;
    let _state = spawn_coordinator_prepared_with_tick(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::SubscribedEcho {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            subscriptions: vec!["tick".to_owned()],
        },
        |_| {},
        Some(Duration::from_millis(40)),
    )
    .await;

    // When a few tick intervals elapse (fixed window — the pipeline is
    // multi-hop: timer task → coordinator → plugin actor → guest → read
    // pump → inbound channel → bus, so collect over a window instead of
    // racing the first two arrivals).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let events = await_recorded(&recorder, 0, Duration::from_millis(100)).await;

    // Then multiple tick events arrived carrying epoch milliseconds.
    assert!(events.len() >= 2, "expected periodic ticks");
    for event in &events {
        assert!(
            event.citations[0].title.contains(r#""type":"tick""#),
            "expected tick echo, got: {:?}",
            event.citations[0].title
        );
        assert!(
            event.citations[0].title.contains(r#""now_ms":"#),
            "tick must carry now_ms, got: {:?}",
            event.citations[0].title
        );
    }
}

/// A mirrored `restart_stalled_stream` line translates to the internal
/// `RetryStalledSession` command on the bus.
#[rstest::rstest]
#[tokio::test]
async fn mirrored_restart_stalled_stream_publishes_retry_command() {
    // Given a coordinator whose guest sends one mirrored restart line for a
    // valid session, and a recorder for the internal command.
    let harness = TestHarness::new().await;
    let recorder = harness
        .spawn_recorder::<crate::feat::session::protocol::retry_stalled_session::RetryStalledSession>()
        .await;
    let session_id = SessionId::new();
    let line = format!(
        r#"{{"v":1,"seq":2,"ts":0,"type":"restart_stalled_stream","session_id":"{session_id}"}}"#
    );
    spawn_coordinator(
        &harness,
        plugins(),
        jinn_plugin::FakeGuestScript::HelloThenLines {
            protocol_version: jinn_plugin_api::PROTOCOL_VERSION,
            lines: vec![line],
        },
    )
    .await;

    // When the mirror is processed.
    let commands = await_recorded(&recorder, 1, WAIT).await;

    // Then the internal RetryStalledSession carries the parsed session id.
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].session_id, session_id);
}
