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
use jinn_session_lifecycle_msg::TeardownSessionTree;
use jinn_session_lifecycle_msg::builtin::{BuiltinHandler, BuiltinHandlerError, BuiltinRegistry};
use jinn_session_lifecycle_msg::{
    CancelLifecycleCommand, RunSessionSetup, SessionCwdChanged, SessionSetupCompleted,
    SetSessionCwd,
};
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::ArchiveSession;
use jinn_session_store_msg::ArchiveSessionTree;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

use crate::session_lifecycle_actor::{SessionLifecycleActor, SessionLifecycleActorDeps};

struct ActorFixture {
    harness: TestHarness,
    state: State,
}

async fn actor_fixture(builtin_registry: BuiltinRegistry) -> ActorFixture {
    let harness = TestHarness::new().await;
    let services = harness.services().await;
    let state = State::new(AppState::default());
    SessionLifecycleActor::spawn(
        harness.system(),
        SessionLifecycleActorDeps {
            state: state.clone(),
            services: services.clone(),
            builtin_registry,
            shell: "/bin/sh".to_owned(),
        },
    );
    ActorFixture { harness, state }
}

async fn plain_fixture() -> ActorFixture {
    actor_fixture(BuiltinRegistry::new()).await
}

async fn poll_until<F, Fut>(mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..80 {
        if condition().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition().await
}

struct SuccessfulBuiltin;

impl BuiltinHandler for SuccessfulBuiltin {
    fn name(&self) -> &'static str {
        "successful-builtin"
    }

    fn setup(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _args: &[String],
    ) -> Result<std::path::PathBuf, Report<BuiltinHandlerError>> {
        Ok(std::env::temp_dir())
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
    let cwd = std::env::temp_dir().join("lifecycle-cwd");

    // When SetSessionCwd is published.
    fixture
        .harness
        .publish(SetSessionCwd {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
        })
        .await;

    // Then the session uses the requested working directory.
    let updated = poll_until(|| async {
        fixture
            .state
            .read()
            .session
            .get(&session_id)
            .is_some_and(|session| session.cwd() == cwd)
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
    let cwd = std::env::temp_dir().join("lifecycle-event-cwd");

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
    tokio::time::sleep(Duration::from_millis(50)).await;

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
    let mut registry = BuiltinRegistry::new();
    registry.register(
        BuiltinId("test-builtin".to_owned()),
        Arc::new(SuccessfulBuiltin),
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
    let mut registry = BuiltinRegistry::new();
    registry.register(
        BuiltinId("test-builtin".to_owned()),
        Arc::new(SuccessfulBuiltin),
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
    assert_eq!(completions[0].session_id, session_id);
    assert!(completions[0].error.is_none());
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
