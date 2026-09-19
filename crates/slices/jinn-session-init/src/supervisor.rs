//! The session-init supervisor — the slice's one bus-facing router.
//!
//! Every crossing this slice consumes arrives on the shared
//! [`session_init_topic`](crate::session_init_topic): the four session
//! lifecycle trigger events and the three manual rescan commands,
//! forwarded from the kameo bus by the kernel bridge. The supervisor
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
//!
//! Gating (recorded behavior): a payload whose cwd is still the `.`
//! sentinel (a lifecycle setup is pending, the real cwd is unknown) is
//! skipped silently — no scan, no event, no warn.

use std::path::Path;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::envelope::Address;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_domain::feat::context::protocol::command::ScanContextFiles;
use jinn_domain::feat::provider::protocol::command::RescanPromptTemplates;
use jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use jinn_domain::feat::session_lifecycle::protocol::event::{SessionCreated, SessionCwdChanged};
use jinn_session_msg::SessionSetupCompleted;
use jinn_skills_msg::ScanSkills;

use crate::commands::{RescanContext, RescanPrompts, RescanSkills, RunDiscovery};

/// The cwd sentinel meaning "the real working directory is not known
/// yet" — a lifecycle setup is still pending. Scans gated on it are
/// skipped silently.
const CWD_SENTINEL: &str = ".";

/// The session-init supervisor: a pure payload router from the
/// trigger topic to the discovery partition set.
pub struct SessionInitSupervisor {
    /// The partition set's public path (a static path, not a handle:
    /// the kernel resolves the entity per key).
    discovery: ActorPath,
}

impl ServiceActor for SessionInitSupervisor {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Stateless: the partition set's path is a compile-time constant
        // of the slice, so no injected handles are needed.
        Ok(Self {
            discovery: ActorPath::new(crate::DISCOVERY_PATH),
        })
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
    pub fn spawn(system: &ActorSystem) -> ActorPath {
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(crate::SUPERVISOR_PATH))
            .mailbox(1024, trouper::inbox::OverloadPolicy::Block)
            .handles::<SessionCreated>()
            .handles::<SessionSetupCompleted>()
            .handles::<SessionLoadCompleted>()
            .handles::<SessionCwdChanged>()
            .handles::<ScanSkills>()
            .handles::<RescanPromptTemplates>()
            .handles::<ScanContextFiles>()
            .start();

        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug;                       the activate-before-first-trigger ordering relies on the cursor"
        )]
        system
            .subscribe(&path, &crate::session_init_topic(), None)
            .expect("session-init supervisor subscribes to the trigger topic");
        path
    }

    /// Sends a keyed command to the discovery partition set. The
    /// kernel extracts the shard key, derives the entity path, and
    /// activates the worker on demand.
    fn send_to_worker<M>(&self, ctx: &mut MsgCtx<'_>, msg: &M)
    where
        M: trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
    {
        ctx.send(Address::Path(self.discovery.clone()), msg, None);
    }

    /// Runs a full discovery for `session_id` when the trigger's cwd
    /// has resolved. The payload's cwd rides into the command so the
    /// worker scans the same directory the trigger observed.
    fn gated_run(&self, ctx: &mut MsgCtx<'_>, session_id: &SessionId, cwd: &Path) {
        if Self::gate_open(cwd) {
            self.send_to_worker(
                ctx,
                &RunDiscovery {
                    session_id: session_id.clone(),
                    cwd: cwd.to_path_buf(),
                },
            );
        }
    }

    /// The pending-cwd gate: a trigger whose cwd is still the `.`
    /// sentinel (lifecycle setup pending) gates closed, silently.
    fn gate_open(cwd: &Path) -> bool {
        cwd != Path::new(CWD_SENTINEL)
    }
}

// The four trigger events all run the same gated full discovery; the
// cwd rides each payload.
impl MsgHandler<SessionCreated> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionCreated, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

impl MsgHandler<SessionSetupCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionSetupCompleted, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

impl MsgHandler<SessionLoadCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionLoadCompleted, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, msg.session_id(), msg.session.cwd());
    }
}

impl MsgHandler<SessionCwdChanged> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionCwdChanged, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id, &msg.cwd);
    }
}

// The three manual rescans target one resource each; the payload's
// cwd rides into the command.
impl MsgHandler<ScanSkills> for SessionInitSupervisor {
    async fn handle(&mut self, msg: ScanSkills, ctx: &mut MsgCtx<'_>) {
        if Self::gate_open(&msg.cwd) {
            self.send_to_worker(
                ctx,
                &RescanSkills {
                    session_id: msg.session_id.clone(),
                    cwd: msg.cwd,
                },
            );
        }
    }
}

impl MsgHandler<RescanPromptTemplates> for SessionInitSupervisor {
    async fn handle(&mut self, msg: RescanPromptTemplates, ctx: &mut MsgCtx<'_>) {
        if Self::gate_open(&msg.cwd) {
            self.send_to_worker(
                ctx,
                &RescanPrompts {
                    session_id: msg.session_id.clone(),
                    cwd: msg.cwd,
                },
            );
        }
    }
}

impl MsgHandler<ScanContextFiles> for SessionInitSupervisor {
    async fn handle(&mut self, msg: ScanContextFiles, ctx: &mut MsgCtx<'_>) {
        if Self::gate_open(&msg.cwd) {
            self.send_to_worker(
                ctx,
                &RescanContext {
                    session_id: msg.session_id.clone(),
                    cwd: msg.cwd,
                },
            );
        }
    }
}
