//! Prompt template scan actor - scans and reloads prompt templates.
//!
//! Two trigger paths:
//! - **Event-driven** (automatic): subscribes to session lifecycle events
//!   ([`EnvironmentLoaded`], [`SessionCreated`], [`SessionSetupCompleted`],
//!   [`SessionLoadCompleted`], [`SessionCwdChanged`]). Each event resolves a
//!   session id, applies the `"."`-sentinel gate via
//!   [`scan_cwd_for_session`](scan_cwd_for_session),
//!   and scans when the cwd is settled.
//! - **Command-driven** (manual reload): subscribes to
//!   [`RescanPromptTemplates`] commands.
//!
//! On either trigger, scans system, user, and project prompts directories for
//!   the session's cwd, writes the merged result into that session's ephemeral
//!   discovered set, and emits [`PromptTemplatesLoaded`] events.

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::state::State;
use crate::feat::context::prompt_template::PromptTemplateStore;
use crate::feat::discovery::project_prompts_dirs;
use crate::feat::provider::protocol::command::RescanPromptTemplates;
use crate::feat::provider::protocol::event::PromptTemplatesLoaded;
use crate::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use crate::feat::session_lifecycle::protocol::event::{
    SessionCreated, SessionCwdChanged, SessionSetupCompleted,
};
use crate::init::env_init_actor::EnvironmentLoaded;
use crate::protocol::SessionId;
use kameo::Actor;
use kameo::actor::ActorRef;
use kameo::message::{Context as MsgContext, Message};

fn scan_cwd_for_session(
    state: &crate::common::state::State,
    session_id: &crate::SessionId,
) -> Option<std::path::PathBuf> {
    let guard = state.read();
    let session = guard.try_session(session_id)?;
    let cwd = session.cwd();
    (cwd != std::path::Path::new(".")).then(|| cwd.to_path_buf())
}


/// Dependencies for [`PromptScanActor`].
#[derive(Clone)]
pub struct PromptScanActorDeps {
    /// Runtime deps (services, bus).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Authority to write discovered prompt templates into sessions.
    pub session_cap: crate::common::tcaps::session::SessionCap,
}

/// Scans and reloads prompt templates on `RescanPromptTemplates`.
///
/// On command, reads the session's cwd from shared state, scans system, user,
/// and project prompts directories (project templates override system/user on a
/// most-local-wins basis), writes the merged store into that session's
/// ephemeral discovered set, and emits `PromptTemplatesLoaded`.
pub struct PromptScanActor {
    /// Runtime deps (services, bus).
    deps: ActorDeps,
    /// Shared application state.
    state: State,
    /// Authority to write discovered prompt templates into sessions.
    session_cap: crate::common::tcaps::session::SessionCap,
}

impl BusPublish for PromptScanActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl Actor for PromptScanActor {
    type Args = PromptScanActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let deps = args.deps;
        deps.subscribe(actor_ref.clone().recipient::<RescanPromptTemplates>())
            .await;
        deps.subscribe(actor_ref.clone().recipient::<EnvironmentLoaded>())
            .await;
        deps.subscribe(actor_ref.clone().recipient::<SessionCreated>())
            .await;
        deps.subscribe(actor_ref.clone().recipient::<SessionSetupCompleted>())
            .await;
        deps.subscribe(actor_ref.clone().recipient::<SessionLoadCompleted>())
            .await;
        deps.subscribe(actor_ref.recipient::<SessionCwdChanged>())
            .await;

        Ok(Self {
            deps,
            state: args.state,
            session_cap: args.session_cap,
        })
    }
}

impl Message<RescanPromptTemplates> for PromptScanActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RescanPromptTemplates,
        _ctx: &mut MsgContext<Self, Self::Reply>,
    ) {
        self.run_scan(&msg.session_id).await;
    }
}

impl Message<EnvironmentLoaded> for PromptScanActor {
    type Reply = ();

    async fn handle(&mut self, _msg: EnvironmentLoaded, _ctx: &mut MsgContext<Self, Self::Reply>) {
        let session_id = self.state.read().session.active_session_id().clone();
        if scan_cwd_for_session(&self.state, &session_id)
            .is_some()
        {
            self.run_scan(&session_id).await;
        }
    }
}

impl Message<SessionCreated> for PromptScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCreated, _ctx: &mut MsgContext<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionSetupCompleted> for PromptScanActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SessionSetupCompleted,
        _ctx: &mut MsgContext<Self, Self::Reply>,
    ) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionLoadCompleted> for PromptScanActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SessionLoadCompleted,
        _ctx: &mut MsgContext<Self, Self::Reply>,
    ) {
        if scan_cwd_for_session(&self.state, msg.session_id())
            .is_some()
        {
            self.run_scan(msg.session_id()).await;
        }
    }
}

impl Message<SessionCwdChanged> for PromptScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCwdChanged, _ctx: &mut MsgContext<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl PromptScanActor {
    /// Runs the blocking scan for a session's cwd and emits the result.
    async fn run_scan(&self, session_id: &SessionId) {
        let Some((cwd, home, user_dir, system_dir)) = self.resolve_scan_inputs(session_id) else {
            tracing::warn!(%session_id, "RescanPromptTemplates: session not found, skipping");
            return;
        };

        let project_dirs = project_prompts_dirs(&cwd, &home);

        let result = tokio::task::spawn_blocking(move || {
            PromptTemplateStore::load_from_dirs_ordered(&user_dir, &system_dir, &project_dirs)
        })
        .await;

        match result {
            Ok(Ok(store)) => {
                tracing::info!(count = store.len(), "rescanned prompt templates");

                {
                    let session_id = session_id.clone();
                    self.state.with_session(&self.session_cap, |view| {
                        if let Some(session) = view.session.map().get_mut(&session_id) {
                            session.set_discovered_prompt_templates(store.clone());
                        }
                    });
                }

                self.publish(PromptTemplatesLoaded {
                    session_id: session_id.clone(),
                    templates: store.templates().to_vec(),
                    error: None,
                })
                .await;
            }
            Ok(Err(e)) => {
                tracing::warn!("failed to rescan prompt templates: {e:?}");
                self.publish(PromptTemplatesLoaded {
                    session_id: session_id.clone(),
                    templates: vec![],
                    error: Some(format!("{e:?}")),
                })
                .await;
            }
            Err(join_error) => {
                tracing::error!("rescan task panicked: {join_error}");
                self.publish(PromptTemplatesLoaded {
                    session_id: session_id.clone(),
                    templates: vec![],
                    error: Some(format!("rescan task failed: {join_error}")),
                })
                .await;
            }
        }
    }

    /// Reads the session's cwd and the prompt dirs for the scan.
    ///
    /// Returns `None` if the session is not present in state (it may have been
    /// closed concurrently). All values are cheap clones that can move into a
    /// `spawn_blocking` closure.
    fn resolve_scan_inputs(
        &self,
        session_id: &SessionId,
    ) -> Option<(
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    )> {
        let guard = self.state.read();
        let session = guard.try_session(session_id)?;
        let cwd = session.cwd().to_path_buf();
        let home = self.deps.services.paths.home_dir().to_path_buf();
        let user_dir = self.deps.services.paths.prompts_dir();
        let system_dir = self.deps.services.paths.system_prompts_dir();
        Some((cwd, home, user_dir, system_dir))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use crate::common::app_paths::AppPaths;
    use crate::common::app_state::AppState;
    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::common::state::State;
    use crate::feat::provider::protocol::command::RescanPromptTemplates;
    use crate::feat::provider::protocol::event::PromptTemplatesLoaded;

    use crate::feat::session_lifecycle::protocol::event::{
        SessionCreated, SessionCwdChanged, SessionSetupCompleted,
    };
    use crate::init::env_init_actor::EnvironmentLoaded;

    use super::*;

    /// Writes a project prompt `name.md` under `cwd/.agents/prompts`.
    fn write_project_prompt(cwd: &std::path::Path, name: &str) {
        let dir = cwd.join(".agents").join("prompts");
        std::fs::create_dir_all(&dir).expect("create prompts dir");
        std::fs::write(
            dir.join(format!("{name}.md")),
            format!("+++\nname = \"{name}\"\ndescription = \"\"\n+++\nYou are {name}."),
        )
        .expect("write prompt");
    }

    fn setup_actor(
        _harness: &TestHarness,
        cwd: &std::path::Path,
        home: &tempfile::TempDir,
    ) -> (State, SessionId) {
        let state = State::new(AppState::default());
        {
            let mut guard = state.write_test_no_cap();
            guard
                .session
                .active_session_mut()
                .set_cwd(cwd.to_path_buf());
        }
        let session_id = state.read().session.active_session_id().clone();
        let mut paths = AppPaths::new_in(home.path());
        paths.set_home_dir_for_test(home.path().to_path_buf());
        (state, session_id)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_prompts_writes_to_session_and_emits_session_tagged_event() {
        // Given a project prompt in a cwd that is a descendant of home.
        let dir = tempfile::tempdir().expect("create temp dir");
        let cwd = dir.path().join("work");
        let prompts_dir = cwd.join(".agents/prompts");
        std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
        std::fs::write(
            prompts_dir.join("code.md"),
            "+++\nname = \"code\"\ndescription = \"\"\n+++\nYou are a coder.",
        )
        .expect("write prompt");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &cwd, &dir);

        let recorder = harness.spawn_recorder::<PromptTemplatesLoaded>().await;

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When sending RescanPromptTemplates.
        harness
            .publish(RescanPromptTemplates {
                session_id: session_id.clone(),
            })
            .await;

        // Then the project prompt is in the session's discovered store.
        let loaded = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
        let found = loaded
            .iter()
            .find(|l| l.error.is_none())
            .expect("should have PromptTemplatesLoaded");
        assert_eq!(found.session_id, session_id);

        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        let names: Vec<&str> = session
            .discovered_prompt_templates()
            .templates()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            names.contains(&"code"),
            "project prompt discovered: {names:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_prompts_discovers_ancestor_template_from_nested_cwd() {
        // Given a prompt at home/repo/.agents/prompts but cwd is home/repo/subdir.
        let dir = tempfile::tempdir().expect("create temp dir");
        let repo = dir.path().join("repo");
        let subdir = repo.join("subdir");
        std::fs::create_dir_all(&subdir).expect("create nested dirs");
        let ancestor_prompts = repo.join(".agents/prompts");
        std::fs::create_dir_all(&ancestor_prompts).expect("create ancestor prompts dir");
        std::fs::write(
            ancestor_prompts.join("ancestor.md"),
            "+++\nname = \"ancestor\"\ndescription = \"\"\n+++\nYou are an ancestor prompt.",
        )
        .expect("write ancestor prompt");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &subdir, &dir);

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When sending RescanPromptTemplates.
        harness
            .publish(RescanPromptTemplates {
                session_id: session_id.clone(),
            })
            .await;

        // Then the ancestor prompt is discovered from the nested cwd.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        let names: Vec<&str> = session
            .discovered_prompt_templates()
            .templates()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            names.contains(&"ancestor"),
            "ancestor prompt discovered from nested cwd: {names:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_created_event_scans_prompts() {
        // Given an actor whose active session cwd contains a project prompt.
        let dir = tempfile::tempdir().expect("create temp dir");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).expect("create work dir");
        write_project_prompt(&cwd, "code");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &cwd, &dir);

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When publishing SessionCreated for that session.
        harness
            .publish(SessionCreated {
                session_id: session_id.clone(),
            })
            .await;

        // Then the prompt is written to the session's discovered set.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_prompt_templates().templates().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_created_event_skips_scan_when_cwd_is_sentinel() {
        // Given an actor whose active session cwd is the pending "." sentinel.
        let dir = tempfile::tempdir().expect("create temp dir");
        let stray = dir.path().join("stray");
        std::fs::create_dir_all(&stray).expect("create stray dir");
        write_project_prompt(&stray, "stray");

        let harness = TestHarness::new().await;
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When publishing SessionCreated for the sentinel-cwd session.
        harness
            .publish(SessionCreated {
                session_id: session_id.clone(),
            })
            .await;

        // Then no scan runs: the discovered set stays empty.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert!(session.discovered_prompt_templates().templates().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_setup_completed_event_scans_prompts() {
        // Given an actor whose active session cwd contains a project prompt.
        let dir = tempfile::tempdir().expect("create temp dir");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).expect("create work dir");
        write_project_prompt(&cwd, "code");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &cwd, &dir);

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When publishing SessionSetupCompleted.
        harness
            .publish(SessionSetupCompleted {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                error: None,
            })
            .await;

        // Then the prompt is written to the session's discovered set.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_prompt_templates().templates().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_cwd_changed_event_scans_prompts() {
        // Given an actor whose active session cwd contains a project prompt.
        let dir = tempfile::tempdir().expect("create temp dir");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).expect("create work dir");
        write_project_prompt(&cwd, "code");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &cwd, &dir);

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When publishing SessionCwdChanged.
        harness
            .publish(SessionCwdChanged {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
            })
            .await;

        // Then the prompt is written to the session's discovered set.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_prompt_templates().templates().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn environment_loaded_event_scans_active_session_prompts() {
        // Given an actor whose active session cwd contains a project prompt.
        let dir = tempfile::tempdir().expect("create temp dir");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).expect("create work dir");
        write_project_prompt(&cwd, "code");

        let harness = TestHarness::new().await;
        let (state, session_id) = setup_actor(&harness, &cwd, &dir);

        let _actor = harness
            .spawn_actor::<PromptScanActor>(PromptScanActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
                session_cap: crate::common::tcaps::mint::mint_session_cap(),
            })
            .await;

        // When publishing EnvironmentLoaded.
        harness
            .publish(EnvironmentLoaded {
                config: crate::ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                },
            })
            .await;

        // Then the active session's prompt is discovered.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_prompt_templates().templates().len(), 1);
    }
}
