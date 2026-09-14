//! Context-files scan actor — scans project context files.
//!
//! Two trigger paths:
//! - **Event-driven** (automatic): subscribes to session lifecycle events
//!   ([`EnvironmentLoaded`], [`SessionCreated`], [`SessionSetupCompleted`],
//!   [`SessionLoadCompleted`], [`SessionCwdChanged`]). Each event resolves a
//!   session id, applies the `"."`-sentinel gate via
//!   [`scan_cwd_for_session`](scan_cwd_for_session),
//!   and scans when the cwd is settled.
//! - **Command-driven** (manual reload): subscribes to
//!   [`ScanContextFiles`] commands.
//!
//! On either trigger, walks the bounded ancestor chain for the session's cwd
//!   (stopping at an exclusive `$HOME` or an inclusive VCS root, whichever
//!   comes first), reads the first existing candidate (AGENTS.md / CLAUDE.md)
//!   per walked dir, writes the result into that session's ephemeral
//!   discovered set, and emits [`ContextFilesLoaded`] events.

use kameo::actor::ActorRef;
use kameo::prelude::{Context, Message};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::state::State;
use crate::feat::context::env_context::ContextFile;
use crate::feat::context::protocol::command::ScanContextFiles;
use crate::feat::context::protocol::event::ContextFilesLoaded;
use crate::feat::discovery::project_context_files;
use crate::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use crate::feat::session_lifecycle::protocol::event::{
    SessionCreated, SessionCwdChanged, SessionSetupCompleted,
};
use crate::init::env_init_actor::EnvironmentLoaded;

fn scan_cwd_for_session(
    state: &crate::common::state::State,
    session_id: &crate::SessionId,
) -> Option<std::path::PathBuf> {
    let guard = state.read();
    let session = guard.try_session(session_id)?;
    let cwd = session.cwd();
    (cwd != std::path::Path::new(".")).then(|| cwd.to_path_buf())
}


/// Dependencies for [`ContextFilesScanActor`].
#[derive(Clone)]
pub struct ContextFilesScanActorDeps {
    /// Common actor dependencies (services + bus).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Authority to write discovered context files into sessions.
    pub session_cap: crate::common::tcaps::session::SessionCap,
}

/// Scans and loads project context files (AGENTS.md/CLAUDE.md).
///
/// On command, reads the session's cwd from shared state, walks the bounded
/// ancestor chain, reads each discovered context file, writes the result into
/// that session's ephemeral discovered set, and emits `ContextFilesLoaded`.
pub struct ContextFilesScanActor {
    /// Common actor dependencies.
    deps: ActorDeps,
    /// Shared application state.
    state: State,
    /// Authority to write discovered context files into sessions.
    session_cap: crate::common::tcaps::session::SessionCap,
}

impl kameo::Actor for ContextFilesScanActor {
    type Args = ContextFilesScanActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let deps = args.deps;
        deps.subscribe(actor_ref.clone().recipient::<ScanContextFiles>())
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

impl BusPublish for ContextFilesScanActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl Message<ScanContextFiles> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: ScanContextFiles, _ctx: &mut Context<Self, Self::Reply>) {
        self.run_scan(&msg.session_id).await;
    }
}

impl Message<EnvironmentLoaded> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, _msg: EnvironmentLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        let session_id = self.state.read().session.active_session_id().clone();
        if scan_cwd_for_session(&self.state, &session_id)
            .is_some()
        {
            self.run_scan(&session_id).await;
        }
    }
}

impl Message<SessionCreated> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCreated, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionSetupCompleted> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionSetupCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionLoadCompleted> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionLoadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, msg.session_id())
            .is_some()
        {
            self.run_scan(msg.session_id()).await;
        }
    }
}

impl Message<SessionCwdChanged> for ContextFilesScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCwdChanged, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id)
            .is_some()
        {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl ContextFilesScanActor {
    /// Runs the blocking scan for a session's cwd and emits the result.
    async fn run_scan(&self, session_id: &crate::SessionId) {
        let Some((cwd, home)) = self.resolve_scan_inputs(session_id) else {
            tracing::warn!(%session_id, "ScanContextFiles: session not found, skipping");
            return;
        };

        let result = tokio::task::spawn_blocking(move || read_context_files(&cwd, &home)).await;

        match result {
            Ok(files) => {
                tracing::info!(count = files.len(), "scanned project context files");

                {
                    let session_id = session_id.clone();
                    self.state.with_session(&self.session_cap, |view| {
                        if let Some(session) = view.session.map().get_mut(&session_id) {
                            session.set_discovered_context_files(files.clone());
                        }
                    });
                }

                self.publish(ContextFilesLoaded {
                    session_id: session_id.clone(),
                    files,
                    error: None,
                })
                .await;
            }
            Err(join_error) => {
                tracing::error!("context-files scan task panicked: {join_error}");
                self.publish(ContextFilesLoaded {
                    session_id: session_id.clone(),
                    files: vec![],
                    error: Some(format!("context-files scan task failed: {join_error}")),
                })
                .await;
            }
        }
    }

    /// Reads the session's cwd and the user's home dir for the scan.
    ///
    /// Returns `None` if the session is not present in state (it may have been
    /// closed concurrently). Both values are cheap clones that can move into a
    /// `spawn_blocking` closure.
    fn resolve_scan_inputs(
        &self,
        session_id: &crate::SessionId,
    ) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let guard = self.state.read();
        let session = guard.try_session(session_id)?;
        let cwd = session.cwd().to_path_buf();
        let home = self.deps.services.paths.home_dir().to_path_buf();
        Some((cwd, home))
    }
}

/// Reads the bounded-walk context files into `ContextFile` payloads.
///
/// Uses [`project_context_files`] to resolve candidate paths in bounded walk
/// order (least-local → cwd), then reads each file's contents. Files that
/// vanish between resolution and read are skipped silently.
fn read_context_files(cwd: &std::path::Path, home: &std::path::Path) -> Vec<ContextFile> {
    project_context_files(cwd, home)
        .into_iter()
        .filter_map(|path| read_one_context_file(&path))
        .collect()
}

/// Reads a single context file, returning `None` if it can no longer be read.
fn read_one_context_file(path: &std::path::Path) -> Option<ContextFile> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(ContextFile {
        path: path.to_path_buf(),
        content,
    })
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
    use super::*;
    use crate::common::app_paths::AppPaths;
    use crate::common::app_state::AppState;
    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::common::services::test_services::TestServices;
    use crate::common::state::State;
    use crate::feat::session_lifecycle::protocol::event::{
        SessionCreated, SessionCwdChanged, SessionSetupCompleted,
    };
    use crate::init::env_init_actor::EnvironmentLoaded;
    use kameo::actor::Spawn;

    fn create_actor_state(
        dir: &tempfile::TempDir,
        state: &State,
    ) -> (crate::SessionId, crate::Services) {
        {
            let mut guard = state.write_test_no_cap();
            guard
                .session
                .active_session_mut()
                .set_cwd(dir.path().to_path_buf());
        }
        let session_id = state.read().session.active_session_id().clone();

        let home = dir
            .path()
            .parent()
            .expect("temp dir has a parent")
            .to_path_buf();
        let services = TestServices::builder()
            .paths({
                let mut paths = AppPaths::new_in(dir.path());
                paths.set_home_dir_for_test(home);
                paths
            })
            .build();

        (session_id, services)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_context_files_command_writes_to_session_state() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Project rules").expect("write");

        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending ScanContextFiles command.
        harness
            .publish(ScanContextFiles {
                session_id: session_id.clone(),
            })
            .await;

        // Then the file is written to the session's ephemeral discovered set.
        let _recorded = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_context_files().len(), 1);
        assert_eq!(
            session.discovered_context_files()[0].content,
            "# Project rules"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_context_files_command_emits_loaded_event() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Body").expect("write");

        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state,
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending ScanContextFiles command.
        harness.publish(ScanContextFiles { session_id }).await;

        // Then ContextFilesLoaded is emitted with the file and no error.
        let messages = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(messages.len(), 1);
        assert!(messages[0].error.is_none());
        assert_eq!(messages[0].files.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_context_files_empty_dir_emits_empty_loaded() {
        // Given an actor whose session cwd has no context files.
        let dir = tempfile::tempdir().expect("create temp dir");
        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state,
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending ScanContextFiles command.
        harness.publish(ScanContextFiles { session_id }).await;

        // Then ContextFilesLoaded is emitted with an empty list.
        let messages = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(messages.len(), 1);
        assert!(messages[0].files.is_empty());
        assert!(messages[0].error.is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_context_files_skips_home_ancestor() {
        // Given a session cwd nested under a "home" dir that itself has an AGENTS.md,
        // and no VCS marker anywhere - so the walk would reach home if unbounded.
        let home = tempfile::tempdir().expect("create home dir");
        std::fs::write(home.path().join("AGENTS.md"), "home-level file").expect("write home file");
        let project = home.path().join("repo");
        std::fs::create_dir_all(&project).expect("create project");
        std::fs::write(project.join("AGENTS.md"), "project file").expect("write project file");

        let dir = tempfile::tempdir().expect("create temp dir for AppPaths");
        let state = State::new(AppState::default());
        {
            let mut guard = state.write_test_no_cap();
            guard.session.active_session_mut().set_cwd(project.clone());
        }
        let session_id = state.read().session.active_session_id().clone();

        let mut paths = AppPaths::new_in(dir.path());
        paths.set_home_dir_for_test(home.path().to_path_buf());
        let services = TestServices::builder().paths(paths).build();

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state,
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending ScanContextFiles command.
        harness.publish(ScanContextFiles { session_id }).await;

        // Then only the project file is loaded; the home-level file is excluded.
        let messages = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].files.len(), 1);
        assert_eq!(messages[0].files[0].content, "project file");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn scan_context_files_discovers_ancestor_file_from_nested_cwd() {
        // Given cwd = home/repo/subdir and an AGENTS.md at home/repo.
        let home = tempfile::tempdir().expect("create home dir");
        let repo = home.path().join("repo");
        let subdir = repo.join("subdir");
        std::fs::create_dir_all(&subdir).expect("create nested dirs");
        std::fs::write(repo.join("AGENTS.md"), "ancestor file").expect("write ancestor file");

        let dir = tempfile::tempdir().expect("create temp dir for AppPaths");
        let state = State::new(AppState::default());
        {
            let mut guard = state.write_test_no_cap();
            guard.session.active_session_mut().set_cwd(subdir.clone());
        }
        let session_id = state.read().session.active_session_id().clone();

        let mut paths = AppPaths::new_in(dir.path());
        paths.set_home_dir_for_test(home.path().to_path_buf());
        let services = TestServices::builder().paths(paths).build();

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state,
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;
        harness.publish(ScanContextFiles { session_id }).await;

        let messages = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(
            messages.len(),
            1,
            "ancestor file must be discovered from a descendant cwd"
        );
        assert_eq!(messages[0].files[0].content, "ancestor file");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_created_event_scans_context_files() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Project rules").expect("write");
        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        // When sending SessionCreated for that session.
        harness
            .publish(SessionCreated {
                session_id: session_id.clone(),
            })
            .await;

        // Then the file is written to the session's discovered set.
        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;
        let _ = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_context_files().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_created_event_skips_scan_when_cwd_is_sentinel() {
        // Given an actor whose active session cwd is the pending "." sentinel.
        let dir = tempfile::tempdir().expect("create temp dir");
        let stray = dir.path().join("stray");
        std::fs::create_dir_all(&stray).expect("create stray dir");
        std::fs::write(stray.join("AGENTS.md"), "# stray").expect("write");
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();

        let mut paths = AppPaths::new_in(dir.path());
        paths.set_home_dir_for_test(dir.path().to_path_buf());
        let services = TestServices::builder().paths(paths).build();

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending SessionCreated for the sentinel-cwd session.
        harness
            .publish(SessionCreated {
                session_id: session_id.clone(),
            })
            .await;

        // Then no scan runs: the discovered set stays empty.
        let messages = await_recorded(&recorder, 1, std::time::Duration::from_millis(100)).await;
        assert!(messages.is_empty(), "should not scan for sentinel cwd");
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert!(session.discovered_context_files().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_setup_completed_event_scans_context_files() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Project rules").expect("write");
        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        // When sending SessionSetupCompleted.
        harness
            .publish(SessionSetupCompleted {
                session_id: session_id.clone(),
                cwd: dir.path().to_path_buf(),
                error: None,
            })
            .await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;
        let _ = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_context_files().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_cwd_changed_event_scans_context_files() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Project rules").expect("write");
        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        // When sending SessionCwdChanged.
        harness
            .publish(SessionCwdChanged {
                session_id: session_id.clone(),
                cwd: dir.path().to_path_buf(),
            })
            .await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;
        let _ = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_context_files().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn environment_loaded_event_scans_active_session_context_files() {
        // Given an actor whose session cwd has an AGENTS.md.
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "# Project rules").expect("write");
        let state = State::new(AppState::default());
        let (session_id, services) = create_actor_state(&dir, &state);

        let harness = TestHarness::new().await;
        let mut services = services;
        services.bus = harness.bus();

        let actor = ContextFilesScanActor::spawn(ContextFilesScanActorDeps {
            deps: ActorDeps { services },
            state: state.clone(),
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;

        let recorder = harness.spawn_recorder::<ContextFilesLoaded>().await;

        // When sending EnvironmentLoaded.
        harness
            .publish(EnvironmentLoaded {
                config: crate::ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                },
            })
            .await;

        // Then the file is written to the session's discovered set.
        let _ = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert_eq!(session.discovered_context_files().len(), 1);
    }
}
