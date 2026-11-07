//! The session-init supervisor — the slice's one bus-facing router.
//!
//! Every crossing this slice consumes arrives on the shared
//! [`session_init_topic`](crate::session_init_topic): the four session
//! lifecycle trigger events and the three manual rescan commands,
//! published onto the kernel topic. The supervisor
//! reads each trigger's payload — the session id and its cwd travel
//! with the event — applies the pending-cwd gate, and sends a
//! path-addressed command to the discovery partition set's public
//! path. The kernel extracts the shard key, derives
//! `jinn.discovery/<session_id>`, and activates the per-session worker
//! on demand.
//!
//! The supervisor holds no shared state. Every fact it acts on comes
//! from the payload: `SessionCreated` and the manual rescans carry the
//! cwd, the session-actor's setup/load events carry it, and the
//! composition tail publishes [`SessionCwdChanged`] for the boot
//! session — that publish, not `EnvironmentLoaded`, is what arms the
//! initial session's discovery.

use jinn_boot_msg::ResourcesChanged;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::envelope::Address;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_kernel::common::state::State;
use jinn_session_init_msg::{RescanPromptTemplates, ScanContextFiles};
use jinn_session_lifecycle_msg::SessionSetupCompleted;
use jinn_session_lifecycle_msg::{SessionCreated, SessionCwdChanged};
use jinn_session_store_msg::SessionLoadCompleted;
use jinn_skills_msg::ScanSkills;
use jinn_workflows_msg::ScanWorkflows;

use crate::commands::{RescanContext, RescanPrompts, RescanSkills, RescanWorkflows, RunDiscovery};
use marked_path::{Absolute, MarkedPathBuf};

/// The session-init supervisor: a pure payload router from the
/// trigger topic to the discovery partition set.
pub struct SessionInitSupervisor {
    /// Shared state used to resolve a completed session's working directory.
    state: State,
    /// The partition set's public path (a static path, not a handle:
    /// the kernel resolves the entity per key).
    discovery: ActorPath,
}

impl ServiceActor for SessionInitSupervisor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("SessionInitSupervisor is spawned via init_with"),
        )
    }
}

impl SessionInitSupervisor {
    /// Spawns the supervisor at its static path and subscribes it to
    /// the shared trigger topic.
    ///
    /// A successful [`ActorSystem::subscribe`] is the ordering
    /// guarantee: activation must complete before the composition tail
    /// publishes the boot session's [`SessionCwdChanged`], or the first
    /// trigger is missed.
    ///
    /// # Panics
    ///
    /// Panics if the topic subscription fails, which can only happen on
    /// a broken actor system; the activate-before-first-trigger
    /// ordering relies on the cursor being registered.
    pub fn spawn(system: &ActorSystem, state: State) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(crate::SUPERVISOR_PATH))
            .init_with({
                let state = state.clone();
                move || {
                    let state = state.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            discovery: ActorPath::new(crate::DISCOVERY_PATH),
                        })
                    })
                }
            })
            .mailbox(1024, trouper::inbox::OverloadPolicy::Block)
            .handles::<SessionCreated>()
            .handles::<SessionSetupCompleted>()
            .handles::<SessionLoadCompleted>()
            .handles::<SessionCwdChanged>()
            // Live resources: a theme/persona/skill edit re-runs every
            // live session's discovery.
            .handles::<ResourcesChanged>()
            .handles::<ScanSkills>()
            .handles::<RescanPromptTemplates>()
            .handles::<ScanContextFiles>()
            .handles::<ScanWorkflows>()
            // Discovery commands leave the handler through ctx.send; the
            // flush gate drops any outbound type not declared here.
            .emits::<RunDiscovery>()
            .emits::<RescanSkills>()
            .emits::<RescanPrompts>()
            .emits::<RescanContext>()
            .emits::<RescanWorkflows>()
            .start()
    }

    /// Sends a keyed command to the discovery partition set. The
    /// kernel extracts the shard key, derives the entity path, and
    /// activates the worker on demand.
    fn send_to_worker<M>(&self, ctx: &mut MsgCtx<'_>, msg: &M)
    where
        M: trouper::schema::Schema
            + serde::Serialize
            + serde::de::DeserializeOwned
            + Clone
            + trouper::envelope::PayloadValue,
    {
        ctx.send(Address::Path(self.discovery.clone()), msg.clone(), None);
    }

    /// Runs a full discovery for `session_id` when the trigger's cwd
    /// has resolved. The payload's cwd rides into the command so the
    /// worker scans the same directory the trigger observed.
    fn gated_run(
        &self,
        ctx: &mut MsgCtx<'_>,
        session_id: &SessionId,
        cwd: &MarkedPathBuf<Absolute>,
    ) {
        // The cwd is `Absolute` by construction, so there is no unresolved
        // state to gate on: every trigger dispatches discovery directly.
        self.send_to_worker(
            ctx,
            &RunDiscovery {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
            },
        );
    }
}

// The four trigger events all run the same gated full discovery; the
// cwd rides each payload.
impl MsgHandler<SessionCreated> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &SessionCreated, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

impl MsgHandler<SessionSetupCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &SessionSetupCompleted, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

impl MsgHandler<SessionLoadCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &SessionLoadCompleted, ctx: &mut MsgCtx<'_>) {
        let cwd = self
            .state
            .read()
            .session
            .get(msg.session_id())
            .map(|session| session.cwd().clone());
        if let Some(cwd) = cwd {
            self.gated_run(ctx, msg.session_id(), &cwd);
        }
    }
}

impl MsgHandler<SessionCwdChanged> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &SessionCwdChanged, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

// Live resources: skills, prompts, and workflows are files the user can
// edit while jinn runs. A `ResourcesChanged` event re-runs discovery for
// every live session so the next context assembly sees the new set —
// the same full scan a session's own triggers run, keyed per session,
// so an in-flight scan for one session never blocks another's.
impl MsgHandler<ResourcesChanged> for SessionInitSupervisor {
    async fn handle(&mut self, _msg: &ResourcesChanged, ctx: &mut MsgCtx<'_>) {
        let sessions: Vec<(SessionId, MarkedPathBuf<Absolute>)> = {
            let state = self.state.read();
            state
                .session
                .iter()
                .map(|(id, session)| (id.clone(), session.cwd().clone()))
                .collect()
        };
        // Given a resource directory whose contents just changed.
        for (session_id, cwd) in sessions {
            // Then each live session re-runs its full discovery.
            self.gated_run(ctx, &session_id, &cwd);
        }
    }
}

// The three manual rescans target one resource each; the payload's
// cwd rides into the command.
impl MsgHandler<ScanSkills> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &ScanSkills, ctx: &mut MsgCtx<'_>) {
        self.send_to_worker(
            ctx,
            &RescanSkills {
                session_id: msg.session_id.clone(),
                cwd: msg.cwd.clone(),
            },
        );
    }
}

impl MsgHandler<RescanPromptTemplates> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &RescanPromptTemplates, ctx: &mut MsgCtx<'_>) {
        self.send_to_worker(
            ctx,
            &RescanPrompts {
                session_id: msg.session_id.clone(),
                cwd: msg.cwd.clone(),
            },
        );
    }
}

impl MsgHandler<ScanContextFiles> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &ScanContextFiles, ctx: &mut MsgCtx<'_>) {
        self.send_to_worker(
            ctx,
            &RescanContext {
                session_id: msg.session_id.clone(),
                cwd: msg.cwd.clone(),
            },
        );
    }
}

impl MsgHandler<ScanWorkflows> for SessionInitSupervisor {
    async fn handle(&mut self, msg: &ScanWorkflows, ctx: &mut MsgCtx<'_>) {
        self.send_to_worker(
            ctx,
            &RescanWorkflows {
                session_id: msg.session_id.clone(),
                cwd: msg.cwd.clone(),
            },
        );
    }
}
