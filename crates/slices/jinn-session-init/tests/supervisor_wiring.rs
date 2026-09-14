//! End-to-end tests for the session-init slice: trigger events reach
//! the keyed worker through the real supervisor, the pending-cwd gate
//! suppresses scans, and a manual rescan runs the addressed session's
//! worker only. Sessions are driven purely by payloads — the tests
//! never seed a cwd into shared state, mirroring the supervisor's own
//! payload-only routing.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::time::Duration;

use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::state::State;
use jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated;
use jinn_domain::protocol::SessionId;

/// Polls `check` until it passes or the retry budget runs out.
async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held within the retry budget");
}

/// Writes one skill under `<base>/.agents/skills/<name>`.
fn write_skill(base: &std::path::Path, name: &str) {
    let dir = base.join(".agents").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n\nbody"),
    )
    .expect("write SKILL.md");
}

/// A wired supervisor + state + fabric over real temp paths. The
/// `TempDir` is boxed so the home outlives the harness.
struct Wired {
    fabric: jinn_testutil::TestFabric,
    state: State,
    session_id: SessionId,
    home: std::path::PathBuf,
    _dir: Box<tempfile::TempDir>,
}

impl Wired {
    /// Spawns the supervisor on a fresh fabric over a real temp home.
    /// The session's cwd lives only in test payloads — no state seeding.
    async fn wire() -> Self {
        let dir = Box::new(tempfile::tempdir().expect("temp dir"));
        let home = dir.path().to_path_buf();
        let paths = AppPaths::new_in(&home);
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let fabric = jinn_testutil::TestFabric::new();
        jinn_session_init::install_actors(fabric.system(), paths, state.clone())
            .expect("install session-init actors");
        Self {
            fabric,
            state,
            session_id,
            home,
            _dir: dir,
        }
    }

    /// Sends a `RunDiscovery` for `session_id` at `cwd` through the
    /// partition set's public path.
    async fn send_run_discovery(&self, session_id: &SessionId, cwd: &std::path::Path) {
        let sent = self
            .fabric
            .system()
            .send(self.fabric.system().envelope(
                <jinn_session_init::commands::RunDiscovery as trouper::schema::Schema>::schema_id(),
                trouper::actor::ActorPath::new(jinn_session_init::DISCOVERY_PATH),
                serde_json::json!({
                    "session_id": session_id.to_string(),
                    "cwd": cwd.to_string_lossy(),
                }),
            ))
            .await;
        assert!(sent.is_ok(), "partition send must resolve: {sent:?}");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn session_created_triggers_discovery_for_that_session() {
    // Given a wired supervisor whose payload cwd's home contains one skill.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "test-skill");

    // When SessionCreated carrying that cwd crosses on the trigger topic.
    wired
        .fabric
        .send_to_topic(
            &SessionCreated {
                session_id: wired.session_id.clone(),
                cwd: wired.home.clone(),
            },
            &jinn_session_init::session_init_topic(),
        )
        .await;

    // Then the worker discovers the skill into the session.
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty())
    })
    .await;
}

#[rstest::rstest]
#[tokio::test]
async fn pending_cwd_session_produces_no_scan() {
    // Given a wired supervisor whose home contains one skill.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "test-skill");

    // When SessionCreated crosses with the pending-cwd sentinel.
    wired
        .fabric
        .send_to_topic(
            &SessionCreated {
                session_id: wired.session_id.clone(),
                cwd: std::path::PathBuf::from("."),
            },
            &jinn_session_init::session_init_topic(),
        )
        .await;

    // Then no scan runs: the discovered set stays empty.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let guard = wired.state.read();
    let session = guard.session.get(&wired.session_id).expect("session");
    assert!(session.discovered_skills().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn scan_skills_command_threads_cwd_to_the_worker() {
    // Given a wired supervisor whose home contains one skill.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "threaded-skill");

    // When ScanSkills carrying that cwd crosses on the trigger topic.
    wired
        .fabric
        .send_to_topic(
            &jinn_domain::feat::skills::ScanSkills {
                session_id: wired.session_id.clone(),
                cwd: wired.home.clone(),
            },
            &jinn_session_init::session_init_topic(),
        )
        .await;

    // Then the manual rescan ran against the payload cwd: the skill is
    // discovered into the session.
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty())
    })
    .await;
}

#[rstest::rstest]
#[tokio::test]
async fn manual_rescan_reaches_only_the_addressed_session() {
    // Given a wired supervisor with a second session whose home has a skill.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "only-skill");
    let other = SessionId::new();
    {
        let mut guard = wired.state.write_test_no_cap();
        guard.session.get_or_create(&other);
    }

    // When a rescan command is addressed to the first session only.
    wired
        .send_run_discovery(&wired.session_id, &wired.home)
        .await;

    // Then only the addressed session discovers skills.
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty())
    })
    .await;
    let guard = wired.state.read();
    let other_session = guard.session.get(&other).expect("other session");
    assert!(other_session.discovered_skills().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn worker_settles_and_notifier_writes_the_summary_entry() {
    // Given a wired supervisor + notifier whose payload cwd's home
    // contains one skill.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "test-skill");

    // When a full discovery runs via the partition path.
    wired
        .send_run_discovery(&wired.session_id, &wired.home)
        .await;

    // Then the settle fires and the notifier writes one transient entry.
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.history().is_empty())
    })
    .await;
}

#[rstest::rstest]
#[tokio::test]
async fn rescan_into_empty_dir_clears_stale_discovered_skills() {
    // Given a wired supervisor whose session discovered a skill, and the
    // skill file then removed from disk.
    let wired = Wired::wire().await;
    write_skill(&wired.home, "stale-skill");
    wired
        .send_run_discovery(&wired.session_id, &wired.home)
        .await;
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty())
    })
    .await;
    let skill_dir = wired
        .home
        .join(".agents")
        .join("skills")
        .join("stale-skill");
    std::fs::remove_dir_all(&skill_dir).expect("remove skill dir");

    // When a second discovery runs against the now-empty tree.
    wired
        .send_run_discovery(&wired.session_id, &wired.home)
        .await;
    wait_for(|| {
        let guard = wired.state.read();
        guard
            .session
            .get(&wired.session_id)
            .is_some_and(|s| s.discovered_skills().is_empty())
    })
    .await;

    // Then the discovered set is empty — no stale-skill carryover.
    let guard = wired.state.read();
    let session = guard.session.get(&wired.session_id).expect("session");
    assert!(
        session.discovered_skills().is_empty(),
        "empty tree must clear previously discovered skills"
    );
}
