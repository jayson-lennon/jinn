//! End-to-end crossing test for the session-init slice: kernel publishes
//! on the kameo bus → forward relays → trouper supervisor → keyed worker
//! → per-resource `*Loaded` events → reverse relays → kameo bus.
//!
//! This is the TypeId/topic-identity contract the whole design leans on:
//! a mirrored (shape-equal but distinct) type would silently drop every
//! event at the reverse relay. The subscriber here is a recording actor
//! registered on the real bus, so only a republish of the *exact* kernel
//! event type can satisfy the assertions — the same types
//! `SessionPersistenceActor` and `TaskSettleListenerActor` subscribe to.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::time::Duration;

use jinn_domain::common::bridge::Bridge;
use jinn_domain::common::bus::test_harness::{Recorder, await_recorded};
use jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated;
use jinn_tui::TuiApp;

use crate::common::test_app;

/// A composed app plus a VCS-rooted project tree with one skill, one
/// prompt, and one context file; the active session's cwd points there
/// (the supervisor's gate requires a resolved cwd).
async fn composed_app_with_project()
-> (TuiApp, std::path::PathBuf, jinn_domain::protocol::SessionId) {
    let app = test_app().await;
    let project = std::env::temp_dir().join(format!(
        "session-init-e2e-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let skills_dir = project.join(".agents").join("skills").join("e2e-skill");
    std::fs::create_dir_all(&skills_dir).expect("skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: e2e-skill\ndescription: end-to-end\n---\nbody",
    )
    .expect("SKILL.md");
    let prompts_dir = project.join(".agents").join("prompts");
    std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
    std::fs::write(
        prompts_dir.join("hello.md"),
        "+++\nname = \"hello\"\ndescription = \"Say hello\"\n+++\nHello!",
    )
    .expect("prompt template");
    std::fs::write(project.join("AGENTS.md"), "context body").expect("AGENTS.md");
    let session_id = app.core.state.read().session.active_session_id().clone();
    {
        let mut guard = app.core.state.write_test_no_cap();
        guard.session.active_session_mut().set_cwd(project.clone());
    }
    (app, project, session_id)
}

/// Spawns a [`Recorder`] for `M` and registers it on the app's bus.
async fn recorder_for<M: Clone + Send + 'static>(
    app: &TuiApp,
) -> kameo::actor::ActorRef<Recorder<M>> {
    let recorder = <Recorder<M> as kameo::actor::Spawn>::spawn(());
    app.services.bus.subscribe::<M, _>(&recorder).await;
    recorder
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn session_created_triggers_discovery_and_loaded_events_reach_the_kameo_bus() {
    // Given a composed app with a real project tree, a resolved cwd,
    // and recorders on the bus for the three kernel event types.
    let (app, _project, session_id) = composed_app_with_project().await;
    let skills_recorder = recorder_for::<jinn_domain::feat::skills::SkillsLoaded>(&app).await;
    let prompts_recorder =
        recorder_for::<jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded>(&app)
            .await;
    let context_recorder =
        recorder_for::<jinn_domain::feat::context::protocol::event::ContextFilesLoaded>(&app).await;

    // When the kernel publishes `SessionCreated` (the lifecycle event
    // the old kameo scan actors subscribed): forward relay → supervisor
    // → keyed worker → Loaded events → reverse relays → this bus.
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(SessionCreated {
            session_id: session_id.clone(),
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

    // And the worker's state writes landed through the slice's caps.
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
async fn environment_loaded_resolves_the_active_session() {
    // Given a composed app with a resolved cwd; `EnvironmentLoaded`
    // carries only the provider config — no session id.
    let (app, _project, session_id) = composed_app_with_project().await;
    let skills_recorder = recorder_for::<jinn_domain::feat::skills::SkillsLoaded>(&app).await;

    // When the kernel publishes `EnvironmentLoaded` (the launch-tail
    // trigger): the supervisor resolves the active session and keys
    // the discovery command itself.
    let _ = app.core.bridge.send(Bridge::publish_closure(
        jinn_domain::init::env_init_actor::EnvironmentLoaded {
            config: jinn_domain::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            },
        },
    ));

    // Then the resolved session's discovery ran to completion.
    let skills = await_recorded(&skills_recorder, 1, Duration::from_secs(15)).await;
    assert!(
        skills.iter().any(|e| e.session_id == session_id),
        "SkillsLoaded resolved to the active session: {skills:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn manual_scan_reaches_only_the_addressed_session() {
    // Given a composed app with a resolved project tree and a second
    // session pointing at the same tree.
    let (app, project, first) = composed_app_with_project().await;
    let second = {
        let mut session = jinn_domain::feat::session::ChatSessionState::new();
        session.set_cwd(project.clone());
        let id = session.session_id().clone();
        app.core.state.write_test_no_cap().session.insert(session);
        id
    };
    let skills_recorder = recorder_for::<jinn_domain::feat::skills::SkillsLoaded>(&app).await;

    // When the kernel publishes the manual `ScanSkills` command (the
    // intent handler's publish path) addressed to the FIRST session.
    let _ = app.core.bridge.send(Bridge::publish_closure(
        jinn_domain::feat::skills::ScanSkills {
            session_id: first.clone(),
        },
    ));

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
