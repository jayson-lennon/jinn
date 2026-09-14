//! Skills scan actor - scans and loads agent skills.
//!
//! Two trigger paths:
//! - **Event-driven** (automatic): subscribes to session lifecycle events
//!   ([`EnvironmentLoaded`], [`SessionCreated`], [`SessionSetupCompleted`],
//!   [`SessionLoadCompleted`], [`SessionCwdChanged`]). Each event resolves a
//!   session id, applies the `"."`-sentinel gate via
//!   [`scan_cwd_for_session`](crate::common::actor::scan_actor::scan_cwd_for_session),
//!   and scans when the cwd is settled.
//! - **Command-driven** (manual reload): subscribes to
//!   [`ScanSkills`] commands.
//!
//! On either trigger, scans the skills directory on a blocking thread, writes
//!   results to shared [`State`](crate::common::state::State), and emits
//!   [`SkillsLoaded`] events.

use crate::common::services::bus_service::BusService;
use kameo::prelude::{Actor, ActorRef, Context, Message};
use serde::{Deserialize, Serialize};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::Services;
use crate::common::state::State;
use crate::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use crate::feat::session_lifecycle::protocol::event::{
    SessionCreated, SessionCwdChanged, SessionSetupCompleted,
};
use crate::feat::skills::scan::scan_skills_merged;
use crate::feat::skills::skill::Skill;
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


/// Dependencies for [`SkillsScanActor`].
#[derive(Clone)]
pub struct SkillsScanActorDeps {
    /// Runtime services and bus access.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Authority to write discovered skills into sessions.
    pub session_cap: crate::common::tcaps::session::SessionCap,
    /// Authority to write skill picker state in frontend.
    pub frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

/// Scans and loads agent skills on `ScanSkills`.
///
/// On command, scans the skills directory for `*/SKILL.md` files,
/// writes results to the active session's ephemeral discovered state, and emits
/// `SkillsLoaded`.
pub struct SkillsScanActor {
    /// Runtime services.
    services: Services,
    /// Bus service for publishing events.
    bus: BusService,
    /// Shared application state.
    state: State,
    /// Authority to write discovered skills into sessions.
    session_cap: crate::common::tcaps::session::SessionCap,
    /// Authority to write skill picker state in frontend.
    frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

impl BusPublish for SkillsScanActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl Actor for SkillsScanActor {
    type Args = SkillsScanActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let bus = args.deps.services.bus.clone();
        bus.subscribe::<ScanSkills, _>(&actor_ref).await;
        bus.subscribe::<EnvironmentLoaded, _>(&actor_ref).await;
        bus.subscribe::<SessionCreated, _>(&actor_ref).await;
        bus.subscribe::<SessionSetupCompleted, _>(&actor_ref).await;
        bus.subscribe::<SessionLoadCompleted, _>(&actor_ref).await;
        bus.subscribe::<SessionCwdChanged, _>(&actor_ref).await;

        Ok(Self {
            services: args.deps.services,
            bus,
            state: args.state,
            session_cap: args.session_cap,
            frontend_cap: args.frontend_cap,
        })
    }
}

impl Message<ScanSkills> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: ScanSkills, _ctx: &mut Context<Self, Self::Reply>) {
        self.run_scan(&msg.session_id).await;
    }
}

impl Message<EnvironmentLoaded> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, _msg: EnvironmentLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        let session_id = self.state.read().session.active_session_id().clone();
        if scan_cwd_for_session(&self.state, &session_id).is_some() {
            self.run_scan(&session_id).await;
        }
    }
}

impl Message<SessionCreated> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCreated, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id).is_some() {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionSetupCompleted> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionSetupCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id).is_some() {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl Message<SessionLoadCompleted> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionLoadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, msg.session_id()).is_some() {
            self.run_scan(msg.session_id()).await;
        }
    }
}

impl Message<SessionCwdChanged> for SkillsScanActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCwdChanged, _ctx: &mut Context<Self, Self::Reply>) {
        if scan_cwd_for_session(&self.state, &msg.session_id).is_some() {
            self.run_scan(&msg.session_id).await;
        }
    }
}

impl SkillsScanActor {
    /// Runs the blocking scan for a session's cwd and emits the result.
    async fn run_scan(&self, session_id: &crate::SessionId) {
        // Resolve the session's cwd and home once, up front. The cwd is
        // captured by clone so the blocking scan can move it across the
        // thread boundary without holding the state lock.
        let Some((cwd, home, global_skills_dir, system_skills_dir)) =
            self.resolve_scan_inputs(session_id)
        else {
            tracing::warn!(%session_id, "ScanSkills: session not found, skipping");
            return;
        };

        let project_dirs = crate::feat::discovery::project_skills_dirs(&cwd, &home);

        let result = tokio::task::spawn_blocking(move || {
            scan_skills_merged(&system_skills_dir, &global_skills_dir, &project_dirs)
        })
        .await;

        match result {
            Ok(skills) => {
                tracing::info!(count = skills.len(), "scanned agent skills");

                // Write skills to the session's ephemeral discovered set and
                // reload picker entries from the active session.
                {
                    let session_id = session_id.clone();
                    self.state.with_session(&self.session_cap, |view| {
                        if let Some(session) = view.session.map().get_mut(&session_id) {
                            session.set_discovered_skills(skills.clone());
                        }
                    });
                }

                // Clear stale previews and reload the picker from the now-updated
                // session data.
                let (discovered, disabled, sample_theme) = {
                    let r = self.state.read();
                    let session = r.session.get(session_id);
                    (
                        session
                            .map(|s| s.discovered_skills().to_vec())
                            .unwrap_or_default(),
                        session
                            .map(|s| s.disabled_skills().clone())
                            .unwrap_or_default(),
                        r.frontend.theme.clone(),
                    )
                };
                self.state.with_skills_frontend(&self.frontend_cap, |ops| {
                    ops.reload_picker(&discovered, &disabled, &sample_theme);
                });

                self.publish(SkillsLoaded {
                    session_id: session_id.clone(),
                    skills,
                    error: None,
                })
                .await;
            }
            Err(join_error) => {
                tracing::error!("skills scan task panicked: {join_error}");
                self.publish(SkillsLoaded {
                    session_id: session_id.clone(),
                    skills: vec![],
                    error: Some(format!("skills scan task failed: {join_error}")),
                })
                .await;
            }
        }
    }

    /// Resolve the four inputs needed for a scan: cwd, home, global skills dir, and system skills dir.
    ///
    /// Returns `None` if the session is not present in state (it may have been
    /// closed concurrently). All four values are cheap clones that can move
    /// into a `spawn_blocking` closure.
    fn resolve_scan_inputs(
        &self,
        session_id: &crate::SessionId,
    ) -> Option<(
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    )> {
        let guard = self.state.read();
        let session = guard.try_session(session_id)?;
        let cwd = session.cwd().to_path_buf();
        let home = self.services.paths.home_dir().to_path_buf();
        let global = self.services.paths.skills_dir();
        let system = self.services.paths.system_skills_dir();
        Some((cwd, home, global, system))
    }
}

/// Emitted when skills have been scanned and loaded.
///
/// On success, `skills` contains the discovered skills and `error` is `None`.
/// On failure, `skills` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: crate::SessionId,
    /// The discovered agent skills.
    pub skills: Vec<Skill>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Command to trigger a skills scan for a specific session.
///
/// The actor reads the session's cwd from state, scans global + project
/// dirs discovered via the bounded walk, and writes the merged result into
/// that session's ephemeral discovered-skills set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSkills {
    /// The session whose cwd drives the scan.
    pub session_id: crate::SessionId,
}

impl crate::common::bus::BusMessage for SkillsLoaded {}

jinn_slices::crossing_schema!(SkillsLoaded, "SkillsLoaded",
trouper::schema::SchemaKind::Event,
description: "Skills have been scanned and loaded for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "skills" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json)),
]);

impl crate::common::bus::BusMessage for ScanSkills {}

jinn_slices::crossing_schema!(ScanSkills, "ScanSkills",
trouper::schema::SchemaKind::Command,
description: "Trigger a skills scan for a session.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);

#[cfg(test)]
mod tests;
