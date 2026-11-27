//! Observable behavior tests for the lifecycle-owned session actor.

#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use jinn_kernel::AppState;
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::state::State;
use jinn_preferences_config::schemas::{BuiltinId, LifecycleCommand};
use jinn_session_lifecycle_msg::CloseSession;
use jinn_session_lifecycle_msg::SessionTeardownFinished;
use jinn_session_lifecycle_msg::SessionTeardownStarted;
use jinn_session_lifecycle_msg::TeardownSessionTree;
use jinn_session_lifecycle_msg::builtin::{BuiltinHandler, BuiltinHandlerError, BuiltinRegistry};
use jinn_session_lifecycle_msg::{
    CancelLifecycleCommand, RunSessionSetup, RunSessionTeardown, SessionCwdChanged,
    SessionSetupCompleted, SetSessionCwd,
};
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::ArchiveSession;
use jinn_session_store_msg::ArchiveSessionTree;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

use crate::session_lifecycle_actor::{SessionLifecycleActor, SessionLifecycleActorDeps};
use marked_path::{Absolute, MarkedPathBuf};

/// The shell the actor under test runs commands through — the same shared
/// resolver production wiring uses, so the tests exercise the platform's
/// real POSIX shell. A machine without one fails the test naming the
/// missing dependency; there are no fallbacks.
fn shell_for_tests() -> String {
    jinn_testutil::shell_required()
}

struct ActorFixture {
    harness: TestHarness,
    state: State,
    /// The actor's path — a test that must know the actor has processed
    /// its inbox drains it through this.
    actor_path: trouper::actor::ActorPath,
}

async fn actor_fixture(builtin_registry: BuiltinRegistry) -> ActorFixture {
    let harness = TestHarness::new().await;
    let services = harness.services().await;
    let state = State::new(AppState::default());
    let actor_path = SessionLifecycleActor::spawn(
        harness.system(),
        SessionLifecycleActorDeps {
            state: state.clone(),
            services: services.clone(),
            builtin_registry,
            shell: shell_for_tests(),
        },
    );
    ActorFixture {
        harness,
        state,
        actor_path,
    }
}

async fn plain_fixture() -> ActorFixture {
    actor_fixture(BuiltinRegistry::new()).await
}

/// A successful builtin whose setup path the owning test provides: the test
/// creates the `TempDir`, derives the path from it, and keeps the `TempDir`
/// alive for the whole test.
struct SuccessfulBuiltin {
    setup_dir: MarkedPathBuf<Absolute>,
}

impl BuiltinHandler for SuccessfulBuiltin {
    fn name(&self) -> &'static str {
        "successful-builtin"
    }

    fn setup(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _args: &[String],
    ) -> Result<MarkedPathBuf<Absolute>, Report<BuiltinHandlerError>> {
        Ok(self.setup_dir.clone())
    }

    fn teardown(&self, _session_id: &jinn_core_types::SessionId, _args: &[String]) -> bool {
        true
    }
}

#[rstest::rstest]
#[tokio::test]
async fn set_session_cwd_updates_session_working_directory() {
    // Given a running lifecycle actor and a session.
    let fixture = plain_fixture().await;
    let session_id = fixture.state.read().session.active_session_id().clone();
    let dir = jinn_testutil::tempdir();
    let cwd = dir
        .path
        .join("lifecycle-cwd")
        .expect("temp root joins a bare name");

    // When SetSessionCwd is published.
    fixture
        .harness
        .publish(SetSessionCwd {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
        })
        .await;

    // Then the session uses the requested working directory.
    let updated = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|session| session.cwd() == &cwd)
    })
    .await;
    assert!(updated, "session cwd should be updated");
}

#[rstest::rstest]
#[tokio::test]
async fn set_session_cwd_publishes_session_cwd_changed() {
    // Given a running lifecycle actor and a recorder for cwd events.
    let fixture = plain_fixture().await;
    let changed = fixture.harness.spawn_recorder::<SessionCwdChanged>().await;
    let session_id = fixture.state.read().session.active_session_id().clone();
    let dir = jinn_testutil::tempdir();
    let cwd = dir
        .path
        .join("lifecycle-event-cwd")
        .expect("temp root joins a bare name");

    // When SetSessionCwd is published.
    fixture
        .harness
        .publish(SetSessionCwd {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
        })
        .await;

    // Then SessionCwdChanged carries the same session and directory.
    let events = await_recorded(&changed, 1, Duration::from_secs(1)).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_id, session_id);
    assert_eq!(events[0].cwd, cwd);
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_lifecycle_command_is_noop_without_running_child() {
    // Given a running lifecycle actor with no lifecycle child process.
    let fixture = plain_fixture().await;
    let session_id = fixture.state.read().session.active_session_id().clone();
    let before = fixture.state.read().session.active_session().phase();

    // When CancelLifecycleCommand is published.
    fixture
        .harness
        .publish(CancelLifecycleCommand {
            session_id: session_id.clone(),
        })
        .await;

    // The actor must have processed the cancel before the session is read
    // back. Draining its inbox is that guarantee: a fixed sleep would let
    // the read happen before the actor was even scheduled.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // Then the session remains unchanged and idle.
    let state = fixture.state.read();
    assert!(state.session.contains(&session_id));
    assert_eq!(
        state.session.get(&session_id).expect("session").phase(),
        before
    );
}

#[rstest::rstest]
#[tokio::test]
async fn builtin_setup_completes_with_setup_completed() {
    // Given a running lifecycle actor with a successful builtin setup.
    let dir = jinn_testutil::tempdir();
    let mut registry = BuiltinRegistry::new();
    registry.register(
        BuiltinId("test-builtin".to_owned()),
        Arc::new(SuccessfulBuiltin {
            setup_dir: dir.path.clone(),
        }),
    );
    let fixture = actor_fixture(registry).await;
    let session_id = fixture.state.read().session.active_session_id().clone();
    let completed_recorder = fixture
        .harness
        .spawn_recorder::<SessionSetupCompleted>()
        .await;
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: session_id.clone(),
            command: String::new(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Builtin(BuiltinId(
                "test-builtin".to_owned(),
            ))),
        })
        .await;

    // When the builtin setup command finishes.
    let completed = await_recorded(&completed_recorder, 1, Duration::from_secs(1)).await;

    // Then the completion event carried the session.
    assert!(
        completed.iter().any(|m| m.session_id == session_id),
        "builtin setup should report SessionSetupCompleted for its session"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn builtin_setup_publishes_setup_completion_event() {
    // Given a running lifecycle actor with a successful builtin setup.
    let dir = jinn_testutil::tempdir();
    let mut registry = BuiltinRegistry::new();
    registry.register(
        BuiltinId("test-builtin".to_owned()),
        Arc::new(SuccessfulBuiltin {
            setup_dir: dir.path.clone(),
        }),
    );
    let fixture = actor_fixture(registry).await;
    let session_id = fixture.state.read().session.active_session_id().clone();
    let completed_recorder = fixture
        .harness
        .spawn_recorder::<SessionSetupCompleted>()
        .await;

    // When RunSessionSetup is published for the builtin.
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: session_id.clone(),
            command: String::new(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Builtin(BuiltinId(
                "test-builtin".to_owned(),
            ))),
        })
        .await;

    // Then the setup completion event is published without an error.
    let completions = await_recorded(&completed_recorder, 1, Duration::from_secs(1)).await;
    assert_eq!(completions.len(), 1);
    let completion = completions
        .first()
        .expect("exactly one completion was asserted above");
    assert_eq!(completion.session_id, session_id);
    assert!(completion.error.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn close_session_without_pending_teardown_publishes_archive_session() {
    // Given a running lifecycle actor and a session with no pending teardown.
    let fixture = plain_fixture().await;
    let archive = fixture.harness.spawn_recorder::<ArchiveSession>().await;
    let session_id = fixture.state.read().session.active_session_id().clone();

    // When CloseSession is published.
    fixture
        .harness
        .publish(CloseSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then ArchiveSession is published for the closed session.
    let archives = await_recorded(&archive, 1, Duration::from_secs(1)).await;
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_tree_without_pending_teardown_publishes_archive_session_tree() {
    // Given a running lifecycle actor and an idle root with no pending teardown.
    let fixture = plain_fixture().await;
    let archive = fixture.harness.spawn_recorder::<ArchiveSessionTree>().await;
    let root = fixture.state.read().session.active_session_id().clone();

    // When TeardownSessionTree is published.
    fixture
        .harness
        .publish(TeardownSessionTree { root: root.clone() })
        .await;

    // Then ArchiveSessionTree is published for the root.
    let archives = await_recorded(&archive, 1, Duration::from_secs(1)).await;
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].root, root);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_tree_aborted_by_busy_member_reports_failure_for_every_member() {
    // Given a running actor and a parent with one busy child, both live.
    let fixture = plain_fixture().await;
    let parent = ChatSessionState::new();
    let root = parent.session_id().clone();
    let mut child = ChatSessionState::new();
    let child_id = child.session_id().clone();
    child.set_parent_session(root.clone());
    child.begin_streaming();
    {
        let mut state = fixture.state.write();
        state.session.insert(parent);
        state.session.insert(child);
    }
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;

    // When tearing down the tree, whose own guard rejects the busy member.
    fixture
        .harness
        .publish(TeardownSessionTree { root: root.clone() })
        .await;
    let reported = await_recorded(&teardowns, 2, Duration::from_secs(1)).await;

    // Then every member reports a teardown failure, so both tints clear.
    let mut ids = reported
        .iter()
        .map(|msg| msg.session_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    let mut expected = vec![root, child_id];
    expected.sort();
    assert_eq!(ids, expected, "every tinted member must be cleared");
    // And each failure is reported as an error, not a success.
    assert!(
        reported.iter().all(|msg| msg.error.is_some()),
        "an aborted tree must not report a successful teardown"
    );
}

// ── Concurrency and the per-session in-flight slot ────────────────────
//
// Sessions are independent: any number of lifecycle commands may run at
// once, never two for the same session. A completion or a cancel touches
// exactly its own session's entry. The actor publishes the visible
// outcomes as events; readiness between publish and spawn is paced with
// short sleeps, matching the harness's established pattern.

/// A config layer carrying one shell lifecycle whose teardown succeeds —
/// what a teardown replay resolves through the actor's services.config.
fn shell_config() -> jinn_config::ConfigLayer {
    jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"shell\"\n\
         setup_command = \"echo .\"\n\
         teardown_command = \"true\"\n",
    )
}

/// A teardown whose placeholders are deliberately ordered differently from
/// the session's recorded coordinates — the autodev incident's shape. Each
/// `test` succeeds only when the placeholder carries its recorded value, so
/// a positional substitution fails the command.
fn reordered_teardown_config() -> jinn_config::ConfigLayer {
    jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"shell\"\n\
         setup_command = \"echo .\"\n\
         teardown_command = \"test '<parent>' = 'FEATURE' && test '<branch>' = 'BRANCHY' && test '<dir>' = 'DIRTY'\"\n",
    )
}

/// A teardown with a positional placeholder, replayed by a legacy session.
fn positional_teardown_config() -> jinn_config::ConfigLayer {
    jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"shell\"\n\
         setup_command = \"echo .\"\n\
         teardown_command = \"test $1 = 'legacy-first'\"\n",
    )
}

/// A config whose teardown is a builtin reference.
fn builtin_teardown_config() -> jinn_config::ConfigLayer {
    jinn_config::testutil::config_layer(
        "[[session_lifecycle.script]]\nname = \"shell\"\n\
         setup_command = \"echo .\"\n\
         teardown_command = { builtin = \"recording-teardown\" }\n",
    )
}

/// Builds an actor fixture with an explicit config layer in services.
async fn config_fixture(config: jinn_config::ConfigLayer) -> ActorFixture {
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.config = config;
    let state = State::new(AppState::default());
    let actor_path = SessionLifecycleActor::spawn(
        harness.system(),
        SessionLifecycleActorDeps {
            state: state.clone(),
            services,
            builtin_registry: BuiltinRegistry::new(),
            shell: shell_for_tests(),
        },
    );
    ActorFixture {
        harness,
        state,
        actor_path,
    }
}

/// Builds an actor fixture with the shell lifecycle config in services.
async fn shell_fixture() -> ActorFixture {
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.config = shell_config();
    let state = State::new(AppState::default());
    let actor_path = SessionLifecycleActor::spawn(
        harness.system(),
        SessionLifecycleActorDeps {
            state: state.clone(),
            services,
            builtin_registry: BuiltinRegistry::new(),
            shell: shell_for_tests(),
        },
    );
    ActorFixture {
        harness,
        state,
        actor_path,
    }
}

/// Seeds one plain session into the fixture's state; returns its id.
fn seed_session(fixture: &ActorFixture) -> jinn_core_types::SessionId {
    let session = ChatSessionState::new();
    let id = session.session_id().clone();
    fixture.state.write().session.insert(session);
    id
}

/// Advances the session past setup — the state a teardown gate accepts.
fn advance_past_setup(fixture: &ActorFixture, session_id: &jinn_core_types::SessionId) {
    fixture
        .state
        .write()
        .session
        .get_mut(session_id)
        .expect("session")
        .advance_lifecycle_after_setup();
}

/// Stamps the shell lifecycle coordinates on the session — the recorded
/// state a teardown replay resolves.
fn stamp_shell_lifecycle(fixture: &ActorFixture, session_id: &jinn_core_types::SessionId) {
    fixture
        .state
        .write()
        .session
        .get_mut(session_id)
        .expect("session")
        .set_lifecycle_name(Some("shell".to_owned()));
}

/// Stamps named lifecycle coordinates on the session.
fn stamp_named_args(
    fixture: &ActorFixture,
    session_id: &jinn_core_types::SessionId,
    pairs: &[(&str, &str)],
) {
    fixture
        .state
        .write()
        .session
        .get_mut(session_id)
        .expect("session")
        .set_lifecycle_args(jinn_session_state::fields::LifecycleArgs::from_pairs(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        ));
}

#[rstest::rstest]
#[tokio::test]
async fn setups_for_distinct_sessions_run_independently() {
    // Given an actor and two sessions.
    let fixture = shell_fixture().await;
    let setups = fixture
        .harness
        .spawn_recorder::<SessionSetupCompleted>()
        .await;
    let first = seed_session(&fixture);
    let second = seed_session(&fixture);

    // When a quick setup runs for each session.
    for id in [&first, &second] {
        fixture
            .harness
            .publish(RunSessionSetup {
                session_id: id.clone(),
                command: "echo .".to_owned(),
                args: Vec::new(),
                lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
            })
            .await;
    }

    // Then both complete with no error — neither command was dropped,
    // clobbered, nor refused because of the other.
    let completions = await_recorded(&setups, 2, Duration::from_secs(5)).await;
    let errors: Vec<_> = completions
        .iter()
        .filter_map(|m| m.error.as_deref())
        .collect();
    assert!(
        errors.is_empty(),
        "a concurrent setup was refused or failed: {errors:?}"
    );
    let mut ids: Vec<_> = completions.iter().map(|m| m.session_id.clone()).collect();
    ids.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(ids, expected, "every session's setup must complete");
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_setup_for_a_session_with_one_in_flight_is_refused() {
    // Given an actor with one slow setup in flight for a session.
    let fixture = shell_fixture().await;
    let setups = fixture
        .harness
        .spawn_recorder::<SessionSetupCompleted>()
        .await;
    let session_id = seed_session(&fixture);
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: session_id.clone(),
            command: "sleep 2; echo .".to_owned(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Shell("sleep 2; echo .".to_owned())),
        })
        .await;
    // The actor must have spawned the child before the second command
    // arrives, so the second one is refused for the right reason. The
    // actor's inbox is the guarantee: once it is drained, the first
    // command has been handled and the child spawned. A fixed sleep
    // would let the second command race the spawn.
    jinn_testutil::wait::await_inbox_drained(
        fixture.harness.system(),
        &fixture.actor_path,
        Duration::from_secs(10),
    )
    .await;

    // When a second setup arrives for the same session while it runs.
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: session_id.clone(),
            command: "echo .".to_owned(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
        })
        .await;

    // Then the second call is refused naming the session and the busy
    // slot — promptly, as a completion event, not a silent no-op.
    let completions = await_recorded(&setups, 2, Duration::from_secs(5)).await;
    let refusals: Vec<_> = completions
        .iter()
        .filter(|m| m.error.as_deref().is_some_and(|e| e.contains("in flight")))
        .collect();
    assert_eq!(refusals.len(), 1, "one refusal for the second call");
    assert_eq!(refusals[0].session_id, session_id);
    assert!(
        refusals[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains(&session_id.to_string()),
        "the refusal must name the session: {:?}",
        refusals[0].error
    );
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_kills_only_the_named_session_s_command() {
    // Given two sessions with setups in flight: the first slow enough to
    // still be running when the cancel lands, the second still in flight
    // at that moment but finishing quickly after — so its completion
    // proves the cancel never touched it.
    let (fixture, setups, first, second) = {
        let fixture = shell_fixture().await;
        let setups = fixture
            .harness
            .spawn_recorder::<SessionSetupCompleted>()
            .await;
        let first = seed_session(&fixture);
        let second = seed_session(&fixture);
        fixture
            .harness
            .publish(RunSessionSetup {
                session_id: first.clone(),
                command: "sleep 30; echo .".to_owned(),
                args: Vec::new(),
                lifecycle_command: Some(LifecycleCommand::Shell("sleep 30; echo .".to_owned())),
            })
            .await;
        fixture
            .harness
            .publish(RunSessionSetup {
                session_id: second.clone(),
                command: "sleep 2; echo .".to_owned(),
                args: Vec::new(),
                lifecycle_command: Some(LifecycleCommand::Shell("sleep 2; echo .".to_owned())),
            })
            .await;
        // The cancel lands while both are mid-flight. The actor must have
        // processed both setups before the cancel is published, so the cancel
        // genuinely races a running command rather than an unhandled one.
        jinn_testutil::wait::await_inbox_drained(
            fixture.harness.system(),
            &fixture.actor_path,
            Duration::from_secs(10),
        )
        .await;
        (fixture, setups, first, second)
    };

    // When cancelling one session's command.
    fixture
        .harness
        .publish(CancelLifecycleCommand {
            session_id: first.clone(),
        })
        .await;

    // Then the cancelled session reports cancellation while the other
    // still completes normally — the cancel took exactly one slot.
    let completions = await_recorded(&setups, 2, Duration::from_secs(6)).await;
    let cancelled = completions
        .iter()
        .find(|m| m.session_id == first)
        .expect("cancelled session must report");
    assert!(
        cancelled
            .error
            .as_deref()
            .is_some_and(|e| e.contains("cancelled")),
        "the cancelled session must report cancellation: {cancelled:?}"
    );
    let survivor = completions
        .iter()
        .find(|m| m.session_id == second)
        .expect("surviving session must report");
    assert!(
        survivor.error.is_none(),
        "the other session's command must survive the cancel: {survivor:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_finish_releases_only_its_own_session_s_slot() {
    // Given two sessions: the first running a slow setup, the second a
    // quick one that finishes first.
    let (fixture, setups, slow_id, quick_id) = {
        let fixture = shell_fixture().await;
        let setups = fixture
            .harness
            .spawn_recorder::<SessionSetupCompleted>()
            .await;
        let slow_id = seed_session(&fixture);
        let quick_id = seed_session(&fixture);
        fixture
            .harness
            .publish(RunSessionSetup {
                session_id: slow_id.clone(),
                command: "sleep 30; echo .".to_owned(),
                args: Vec::new(),
                lifecycle_command: Some(LifecycleCommand::Shell("sleep 30; echo .".to_owned())),
            })
            .await;
        fixture
            .harness
            .publish(RunSessionSetup {
                session_id: quick_id.clone(),
                command: "echo .".to_owned(),
                args: Vec::new(),
                lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
            })
            .await;
        (fixture, setups, slow_id, quick_id)
    };

    // When the quick one completes.
    let completions = await_recorded(&setups, 1, Duration::from_secs(5)).await;
    // The quick session's command finishes first, so it is the one that
    // completes. The assertion names the session rather than trusting the
    // recorder's arrival order — a slow one that finished first would
    // otherwise be silently accepted.
    let quick_completion = completions
        .iter()
        .find(|completion| completion.session_id == quick_id)
        .expect("the quick session's setup completes first");
    assert!(
        quick_completion.error.is_none(),
        "the quick one finished cleanly"
    );

    // Then the slow session's slot is still held: a follow-up setup for IT
    // is refused while the slot map still carries it — the finish removed
    // only the quick session's entry.
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: slow_id.clone(),
            command: "echo .".to_owned(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
        })
        .await;
    let later = await_recorded(&setups, 2, Duration::from_secs(5)).await;
    let refused = later
        .iter()
        .find(|m| {
            m.session_id == slow_id && m.error.as_deref().is_some_and(|e| e.contains("in flight"))
        })
        .expect("the slow session's slot must still be held");
    assert_eq!(refused.session_id, slow_id);
}

// ── State gates and visible failures ──────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn setup_for_a_session_that_already_ran_setup_is_refused() {
    // Given a session whose lifecycle state is setup_ran.
    let fixture = shell_fixture().await;
    let setups = fixture
        .harness
        .spawn_recorder::<SessionSetupCompleted>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);

    // When a setup arrives for it.
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: session_id.clone(),
            command: "echo .".to_owned(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
        })
        .await;

    // Then it is refused naming the state, and the state is untouched.
    let completions = await_recorded(&setups, 1, Duration::from_secs(2)).await;
    let refusal = completions
        .iter()
        .find(|completion| completion.session_id == session_id)
        .expect("the refusal names its session");
    assert!(
        refusal
            .error
            .as_deref()
            .is_some_and(|e| e.contains("setup requires nothing_ran")),
        "the refusal must name the state gate: {:?}",
        refusal.error
    );
    assert_eq!(
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .expect("session")
            .lifecycle_script_state(),
        jinn_session_lifecycle_msg::LifecycleScriptState::SetupRan,
        "a refused setup must not advance the state"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_before_setup_is_refused_naming_the_state() {
    // Given a session whose lifecycle state is still nothing_ran — even
    // with a lifecycle name recorded (coordinates without the run).
    let fixture = shell_fixture().await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    stamp_shell_lifecycle(&fixture, &session_id);

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "true".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then it is refused naming the state gate.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(2)).await;
    assert!(
        finished[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("teardown requires setup_ran")),
        "the refusal must name the state gate: {:?}",
        finished[0].error
    );
    assert_eq!(finished[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_with_nothing_recorded_is_refused_naming_the_session() {
    // Given a session past setup that recorded no lifecycle name.
    let fixture = shell_fixture().await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "true".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then it is refused naming the missing record.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(2)).await;
    assert!(
        finished[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("no lifecycle script recorded")),
        "the refusal must name the session: {:?}",
        finished[0].error
    );
    assert_eq!(finished[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_whose_recorded_source_vanished_fails_naming_the_source() {
    // Given a session past setup whose lifecycle was recorded from a
    // workflow file that no longer reads.
    let fixture = shell_fixture().await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);
    {
        let mut guard = fixture.state.write();
        let session = guard.session.get_mut(&session_id).expect("session");
        session.set_lifecycle_name(Some("stage".to_owned()));
        session.set_lifecycle_source(Some("/gone/workflow/lifecycle.toml".to_owned()));
    }

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "true".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then the failure is visible, naming the source — never a silent
    // skip, which would leak whatever the teardown guarded.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(2)).await;
    assert!(
        finished[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("/gone/workflow/lifecycle.toml")),
        "the failure must name the source: {:?}",
        finished[0].error
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_recorded_teardown_runs_and_releases_the_slot() {
    // Given a session past setup with the shell lifecycle recorded.
    let fixture = shell_fixture().await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);
    stamp_shell_lifecycle(&fixture, &session_id);

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "ignored-by-the-actor".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then it succeeds with no error — the actor resolved the recorded
    // coordinates and ran them, ignoring the published command.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(5)).await;
    assert!(
        finished[0].error.is_none(),
        "the recorded teardown must run: {:?}",
        finished[0].error
    );
    assert_eq!(finished[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn spawning_a_teardown_publishes_the_started_event() {
    // Given a session past setup with the shell lifecycle recorded.
    let fixture = shell_fixture().await;
    let starts = fixture
        .harness
        .spawn_recorder::<SessionTeardownStarted>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);
    stamp_shell_lifecycle(&fixture, &session_id);

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "ignored-by-the-actor".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then exactly one started event is published, naming the session.
    let started = await_recorded(&starts, 1, Duration::from_secs(2)).await;
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].session_id, session_id);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_renders_by_name_when_placeholder_orders_differ() {
    // Given a session whose recorded coordinates are ordered differently
    // from the teardown template's placeholders — the autodev incident's
    // shape, where a positional replay merged the wrong branch.
    let fixture = config_fixture(reordered_teardown_config()).await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);
    fixture
        .state
        .write()
        .session
        .get_mut(&session_id)
        .expect("session")
        .set_lifecycle_name(Some("shell".to_owned()));
    stamp_named_args(
        &fixture,
        &session_id,
        &[
            ("dir", "DIRTY"),
            ("repo", "REPO"),
            ("branch", "BRANCHY"),
            ("parent", "FEATURE"),
        ],
    );

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "ignored-by-the-actor".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then it succeeds: every placeholder received its own recorded value,
    // not the value at the same position.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(5)).await;
    assert!(
        finished[0].error.is_none(),
        "teardown must render by name, not position: {:?}",
        finished[0].error
    );
}

#[rstest::rstest]
#[tokio::test]
async fn legacy_positional_pairs_still_tear_down_positionally() {
    // Given a legacy session whose arguments were recorded as bare values
    // (now stored as $n-keyed pairs) and a positional teardown template.
    let fixture = config_fixture(positional_teardown_config()).await;
    let teardowns = fixture
        .harness
        .spawn_recorder::<SessionTeardownFinished>()
        .await;
    let session_id = seed_session(&fixture);
    advance_past_setup(&fixture, &session_id);
    fixture
        .state
        .write()
        .session
        .get_mut(&session_id)
        .expect("session")
        .set_lifecycle_name(Some("shell".to_owned()));
    stamp_named_args(&fixture, &session_id, &[("$1", "legacy-first")]);

    // When a teardown arrives for it.
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: session_id.clone(),
            command: "ignored-by-the-actor".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then it succeeds: the values replayed positionally, as they always did.
    let finished = await_recorded(&teardowns, 1, Duration::from_secs(5)).await;
    assert!(
        finished[0].error.is_none(),
        "legacy positional replay must keep working: {:?}",
        finished[0].error
    );
}

#[rstest::rstest]
#[tokio::test]
async fn builtin_teardown_receives_recorded_values_in_template_order() {
    use std::sync::Mutex;

    // Given a builtin teardown that records the values it was handed.
    struct RecordingTeardown {
        seen: Arc<Mutex<Vec<String>>>,
        setup_dir: MarkedPathBuf<Absolute>,
    }
    impl BuiltinHandler for RecordingTeardown {
        fn name(&self) -> &'static str {
            "recording-teardown"
        }
        fn setup(
            &self,
            _session_id: &jinn_core_types::SessionId,
            _args: &[String],
        ) -> Result<MarkedPathBuf<Absolute>, Report<BuiltinHandlerError>> {
            Ok(self.setup_dir.clone())
        }
        fn teardown(&self, _session_id: &jinn_core_types::SessionId, args: &[String]) -> bool {
            *self.seen.lock().expect("mutex") = args.to_vec();
            true
        }
    }

    let (fixture, received, session_id, _dir) = {
        let dir = jinn_testutil::tempdir();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut registry = BuiltinRegistry::new();
        registry.register(
            BuiltinId("recording-teardown".to_owned()),
            Arc::new(RecordingTeardown {
                seen: Arc::clone(&received),
                setup_dir: dir.path.clone(),
            }),
        );
        let harness = TestHarness::new().await;
        let mut services = harness.services().await;
        services.config = builtin_teardown_config();
        let state = State::new(AppState::default());
        let actor_path = SessionLifecycleActor::spawn(
            harness.system(),
            SessionLifecycleActorDeps {
                state: state.clone(),
                services,
                builtin_registry: registry,
                shell: shell_for_tests(),
            },
        );
        let fixture = ActorFixture {
            harness,
            state,
            actor_path,
        };
        let session_id = seed_session(&fixture);
        advance_past_setup(&fixture, &session_id);
        fixture
            .state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session")
            .set_lifecycle_name(Some("shell".to_owned()));
        stamp_named_args(&fixture, &session_id, &[("alpha", "ONE"), ("beta", "TWO")]);
        (fixture, received, session_id, dir)
    };

    // When the builtin teardown runs (a close carries the builtin teardown).
    fixture
        .harness
        .publish(jinn_session_lifecycle_msg::CloseSession {
            session_id: session_id.clone(),
        })
        .await;

    // Then the handler received the values in recorded (template) order,
    // keys dropped.
    let delivered = jinn_testutil::wait::poll_until_async_fn(Duration::from_secs(2), || async {
        !received.lock().expect("mutex").is_empty()
    })
    .await;
    assert!(delivered, "builtin teardown should have run");
    assert_eq!(
        received.lock().expect("mutex").as_slice(),
        &["ONE".to_owned(), "TWO".to_owned()]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn caller_refusals_push_no_child_transcript_entries() {
    // Given sessions in the two refusal shapes — setup already ran, and
    // teardown with nothing recorded — each with a recorder for the chat
    // entry pushes the child transcript would show.
    let fixture = shell_fixture().await;
    let entries = fixture
        .harness
        .spawn_recorder::<jinn_session_history_msg::PushChatEntry>()
        .await;
    let refused_setup = seed_session(&fixture);
    advance_past_setup(&fixture, &refused_setup);
    let refused_teardown = seed_session(&fixture);
    advance_past_setup(&fixture, &refused_teardown);

    // When both refused calls land.
    fixture
        .harness
        .publish(RunSessionSetup {
            session_id: refused_setup.clone(),
            command: "echo .".to_owned(),
            args: Vec::new(),
            lifecycle_command: Some(LifecycleCommand::Shell("echo .".to_owned())),
        })
        .await;
    fixture
        .harness
        .publish(RunSessionTeardown {
            session_id: refused_teardown.clone(),
            command: "true".to_owned(),
            args: Vec::new(),
        })
        .await;

    // Then no chat entry was pushed for either session: caller errors
    // ride the completion events, never the child transcript.
    // The entries recorder must have stopped changing before "no entry was
    // pushed" is an observation rather than a race with a publish that is
    // still in flight.
    let pushed = jinn_testutil::wait::await_settled_messages(
        &entries,
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .await;
    let leaky: Vec<_> = pushed
        .iter()
        .filter(|m| m.session_id == refused_setup || m.session_id == refused_teardown)
        .collect();
    assert!(
        leaky.is_empty(),
        "a refusal leaked into the child transcript: {leaky:?}"
    );
}
