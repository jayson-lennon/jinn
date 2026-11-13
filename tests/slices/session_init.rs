//! End-to-end crossing test for the session-init slice: kernel publishes
//! `SessionCreated` → trouper supervisor → keyed worker → per-resource
//! `*Loaded` events on the slice's topics.
//!
//! The subscriber here is a harness recorder tapping the exact schema
//! topic, so only a publish of the *exact* kernel event type can satisfy
//! the assertions — the same types `SessionPersistenceActor` and
//! `TaskSettleListenerActor` subscribe to.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_kernel::common::bridge::Bridge;
use jinn_session_init_msg::{ContextFilesLoaded, PromptTemplatesLoaded};
use jinn_session_lifecycle_msg::SessionCreated;
use jinn_session_state::ChatSessionState;
use jinn_testutil::bus_harness::{Recorder, await_recorded};
use jinn_tui::TuiApp;

use crate::common::test_app;

/// A composed app plus a VCS-rooted project tree with one skill, one
/// prompt, and one context file. No cwd is seeded into state — the
/// supervisor routes from payload cwd, so tests publish triggers that
/// carry the project dir.
///
/// The project root is a [`tempfile::TempDir`] owned by the caller: it
/// stays alive for the whole test and removes the tree on drop.
async fn composed_app_with_project() -> (TuiApp, tempfile::TempDir, SessionId) {
    let app = test_app().await;
    let project = jinn_testutil::tempdir_with_prefix("jinn-test-session-init-e2e");
    let skills_dir = project
        .path
        .join_all([".agents", "skills", "e2e-skill"])
        .expect("typed parent joins a bare name");
    std::fs::create_dir_all(skills_dir.as_path()).expect("skill dir");
    let skill_file = skills_dir
        .join("SKILL.md")
        .expect("typed parent joins a bare name");
    std::fs::write(
        skill_file.as_path(),
        "---\nname: e2e-skill\ndescription: end-to-end\n---\nbody",
    )
    .expect("SKILL.md");
    let prompts_dir = project
        .path
        .join_all([".agents", "prompts"])
        .expect("typed parent joins a bare name");
    std::fs::create_dir_all(prompts_dir.as_path()).expect("prompts dir");
    let hello = prompts_dir
        .join("hello.md")
        .expect("typed parent joins a bare name");
    std::fs::write(
        hello.as_path(),
        "+++\nname = \"hello\"\ndescription = \"Say hello\"\n+++\nHello!",
    )
    .expect("prompt template");
    let agents = project
        .path
        .join("AGENTS.md")
        .expect("typed parent joins a bare name");
    std::fs::write(agents.as_path(), "context body").expect("AGENTS.md");
    let session_id = app.core.state.read().session.active_session_id().clone();
    (app, project.dir, session_id)
}

/// Spawns a [`Recorder`] for `M` tapped on the app's fabric topic.
async fn recorder_for<M>(app: &TuiApp) -> Recorder<M>
where
    M: Clone
        + Send
        + 'static
        + jinn_slices::BusMessage
        + trouper::schema::Schema
        + serde::Serialize
        + serde::de::DeserializeOwned
        + trouper::envelope::PayloadValue,
{
    jinn_testutil::bus_harness::TestHarness::from_parts(
        app.services.bus.clone(),
        app.services.trouper_system.clone(),
    )
    .spawn_recorder::<M>()
    .await
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn session_created_triggers_discovery_and_loaded_events_land_on_slice_topics() {
    // Given a composed app with a real project tree, a resolved cwd,
    // and recorders on the bus for the three kernel event types.
    let (app, project, session_id) = composed_app_with_project().await;
    let skills_recorder = recorder_for::<jinn_skills_msg::SkillsLoaded>(&app).await;
    let prompts_recorder = recorder_for::<PromptTemplatesLoaded>(&app).await;
    let context_recorder = recorder_for::<ContextFilesLoaded>(&app).await;

    // When the kernel publishes `SessionCreated` (the lifecycle event
    // that drives discovery): supervisor → keyed worker → Loaded
    // events → this bus.
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(SessionCreated {
            session_id: session_id.clone(),
            cwd: MarkedPathBuf::<Absolute>::new(project.path()).expect("temp dir is absolute"),
        }));

    // Then the kernel `SkillsLoaded` type crosses back — and its
    // payload names this session's discovered skill.
    let skills = await_recorded(&skills_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        skills
            .iter()
            .any(|e| e.session_id == session_id && !e.skills.is_empty()),
        "SkillsLoaded for the scanned session: {skills:?}"
    );

    // And the other two resources' events cross as their own types.
    let prompts = await_recorded(&prompts_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        prompts.iter().any(|e| e.session_id == session_id),
        "PromptTemplatesLoaded for the session: {prompts:?}"
    );
    let context = await_recorded(&context_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        context.iter().any(|e| e.session_id == session_id),
        "ContextFilesLoaded for the session: {context:?}"
    );

    // And the worker's state writes landed through the slice projections.
    let guard = app.core.state.read();
    assert!(
        guard
            .session
            .get(&session_id)
            .is_some_and(|s| !s.discovered_skills().is_empty()),
        "session state carries the discovered skill"
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn boot_cwd_changed_triggers_initial_discovery() {
    // Given a composed app with a real project tree; state carries no
    // cwd — exactly like boot, where only the composition tail knows
    // the initial session's cwd.
    let (app, project, session_id) = composed_app_with_project().await;
    let skills_recorder = recorder_for::<jinn_skills_msg::SkillsLoaded>(&app).await;

    // When the kernel publishes `SessionCwdChanged` for the boot
    // session (the tail's publish beside `EnvironmentLoaded`): forward
    // relay → supervisor → keyed worker → Loaded events → reverse
    // relays → this bus.
    let _ = app.core.bridge.send(Bridge::publish_closure(
        jinn_session_lifecycle_msg::SessionCwdChanged {
            session_id: session_id.clone(),
            cwd: MarkedPathBuf::<Absolute>::new(project.path()).expect("temp dir is absolute"),
        },
    ));

    // Then the resolved session's discovery ran to completion.
    let skills = await_recorded(&skills_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        skills
            .iter()
            .any(|e| e.session_id == session_id && !e.skills.is_empty()),
        "SkillsLoaded resolved to the boot session via the cwd payload: {skills:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn environment_loaded_is_no_longer_a_slice_trigger() {
    // Given a composed app with a resolved project tree; the slice's
    // forward relays no longer include `EnvironmentLoaded`.
    let (app, _project, session_id) = composed_app_with_project().await;
    let skills_recorder = recorder_for::<jinn_skills_msg::SkillsLoaded>(&app).await;

    // When the kernel publishes `EnvironmentLoaded` (still a live event:
    // the provider init, browser scan, and session-actor seed flows
    // subscribe to it).
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(jinn_boot_msg::EnvironmentLoaded {
            config: jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            },
        }));

    // Then no discovery ran: the event never reaches the supervisor,
    // so no `SkillsLoaded` crosses back for any session.
    let skills = tokio::time::timeout(Duration::from_millis(500), async {
        let mut all = Vec::new();
        while all.is_empty() {
            all = await_recorded(&skills_recorder, 1, Duration::from_millis(100)).await;
        }
        all
    })
    .await;
    assert!(
        skills.is_err(),
        "EnvironmentLoaded must not trigger slice discovery: {skills:?}"
    );
    // And the assertion is meaningful only if the session id is real.
    assert_ne!(session_id.to_string(), "");
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn manual_scan_reaches_only_the_addressed_session() {
    // Given a composed app with a resolved project tree and a second
    // session pointing at the same tree.
    let (app, project, first) = composed_app_with_project().await;
    let second = {
        let mut session = ChatSessionState::new();
        session
            .set_cwd(MarkedPathBuf::<Absolute>::new(project.path()).expect("temp dir is absolute"));
        let id = session.session_id().clone();
        app.core.state.write().session.insert(session);
        id
    };
    let skills_recorder = recorder_for::<jinn_skills_msg::SkillsLoaded>(&app).await;

    // When the kernel publishes the manual `ScanSkills` command (the
    // intent handler's publish path) addressed to the FIRST session.
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(jinn_skills_msg::ScanSkills {
            session_id: first.clone(),
            cwd: MarkedPathBuf::<Absolute>::new(project.path()).expect("temp dir is absolute"),
        }));

    // Then exactly the addressed session's event crosses.
    let skills = await_recorded(&skills_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        skills.iter().any(|e| e.session_id == first),
        "first session scanned: {skills:?}"
    );
    assert!(
        !skills.iter().any(|e| e.session_id == second),
        "second session must not be scanned: {skills:?}"
    );
}
