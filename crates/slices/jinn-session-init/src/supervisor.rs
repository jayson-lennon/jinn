//! The session-init supervisor — the slice's one bus-facing router.
//!
//! Every crossing this slice consumes arrives on the shared
//! [`session_init_topic`](crate::session_init_topic): the five session
//! lifecycle trigger events and the three manual rescan commands,
//! forwarded from the kameo bus by the kernel bridge. The supervisor
//! resolves each trigger to a target session, applies the pending-cwd
//! gate, and sends a path-addressed command to the discovery partition
//! set's public path — the kernel extracts the shard key, derives
//! `jinn.discovery/<session_id>`, and activates the per-session worker
//! on demand.
//!
//! Gating (recorded behavior): a session whose cwd is still the `.`
//! sentinel (a lifecycle setup is pending, the real cwd is unknown) is
//! skipped silently — no scan, no event, no warn. `EnvironmentLoaded`
//! carries no session id and targets the active session.

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::envelope::Address;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_domain::common::app_paths::AppPaths;
use jinn_domain::common::state::State;
use jinn_domain::feat::context::protocol::command::ScanContextFiles;
use jinn_domain::feat::provider::protocol::command::RescanPromptTemplates;
use jinn_domain::feat::session_lifecycle::protocol::event::{SessionCreated, SessionCwdChanged};
use jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use jinn_domain::init::EnvironmentLoaded;
use jinn_domain::feat::skills::ScanSkills;
use jinn_session_msg::SessionSetupCompleted;

use crate::commands::{RescanContext, RescanPrompts, RescanSkills, RunDiscovery};

/// The session-init supervisor.
///
/// Holds shared state (the gate reads sessions from it) and the
/// discovery partition set's public path.
pub struct SessionInitSupervisor {
    /// Shared application state — the pending-cwd gate reads sessions.
    state: State,
    /// The partition set's public path (a static path, not a handle:
    /// the kernel resolves the entity per key).
    discovery: ActorPath,
    /// The launch's path configuration (home + resource dirs; kept for
    /// symmetry with the worker, which resolves its scan inputs from
    /// the same source).
    #[expect(dead_code, reason = "documentation of the launch-wide inputs; the gate reads State")]
    paths: AppPaths,
}

impl ServiceActor for SessionInitSupervisor {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // The state handle cannot ride JSON args; spawn injects it via
        // `start_with` (see `spawn`).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "SessionInitSupervisor is spawned via start_with; start requires the state handle",
            ),
        )
    }
}

impl SessionInitSupervisor {
    /// Spawns the supervisor at its static path and subscribes it to
    /// the shared trigger topic.
    ///
    /// A successful [`ActorSystem::subscribe`] is the ordering
    /// guarantee: activation must complete before the composition tail
    /// publishes `EnvironmentLoaded`, or the first trigger is missed.
    ///
    /// # Panics
    ///
    /// Panics if the topic subscription fails, which can only happen on
    /// a broken actor system; the activate-before-first-trigger
    /// ordering relies on the cursor being registered.
    pub fn spawn(system: &ActorSystem, state: State, paths: AppPaths) -> ActorPath {
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(crate::SUPERVISOR_PATH))
            .mailbox(1024, trouper::inbox::OverloadPolicy::Block)
            .start_with({
                move || {
                    let state = state.clone();
                    let paths = paths.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            discovery: ActorPath::new(crate::DISCOVERY_PATH),
                            paths,
                        })
                    })
                }
            })
            .handles::<EnvironmentLoaded>()
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
        ctx.send(
            Address::Path(self.discovery.clone()),
            msg,
            None,
        );
    }

    /// Runs a full discovery for `session_id` if its cwd has resolved.
    fn gated_run(&self, ctx: &mut MsgCtx<'_>, session_id: &SessionId) {
        if self.gate_open(session_id) {
            self.send_to_worker(ctx, &RunDiscovery {
                session_id: session_id.clone(),
            });
        }
    }

    /// The pending-cwd gate: scans fire only for sessions whose real
    /// cwd is known. A vanished session gates closed, silently.
    fn gate_open(&self, session_id: &SessionId) -> bool {
        let guard = self.state.read();
        guard
            .try_session(session_id)
            .is_some_and(|session| session.cwd() != std::path::Path::new("."))
    }
}

// The five trigger events all run the same gated full discovery.
impl MsgHandler<EnvironmentLoaded> for SessionInitSupervisor {
    async fn handle(&mut self, _msg: EnvironmentLoaded, ctx: &mut MsgCtx<'_>) {
        let active = self.state.read().session.active_session_id().clone();
        self.gated_run(ctx, &active);
    }
}

impl MsgHandler<SessionCreated> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionCreated, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id);
    }
}

impl MsgHandler<SessionSetupCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionSetupCompleted, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id);
    }
}

impl MsgHandler<SessionLoadCompleted> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionLoadCompleted, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, msg.session_id());
    }
}

impl MsgHandler<SessionCwdChanged> for SessionInitSupervisor {
    async fn handle(&mut self, msg: SessionCwdChanged, ctx: &mut MsgCtx<'_>) {
        self.gated_run(ctx, &msg.session_id);
    }
}

// The three manual rescans target one resource each.
impl MsgHandler<ScanSkills> for SessionInitSupervisor {
    async fn handle(&mut self, msg: ScanSkills, ctx: &mut MsgCtx<'_>) {
        if self.gate_open(&msg.session_id) {
            self.send_to_worker(ctx, &RescanSkills {
                session_id: msg.session_id.clone(),
            });
        }
    }
}

impl MsgHandler<RescanPromptTemplates> for SessionInitSupervisor {
    async fn handle(&mut self, msg: RescanPromptTemplates, ctx: &mut MsgCtx<'_>) {
        if self.gate_open(&msg.session_id) {
            self.send_to_worker(ctx, &RescanPrompts {
                session_id: msg.session_id.clone(),
            });
        }
    }
}

impl MsgHandler<ScanContextFiles> for SessionInitSupervisor {
    async fn handle(&mut self, msg: ScanContextFiles, ctx: &mut MsgCtx<'_>) {
        if self.gate_open(&msg.session_id) {
            self.send_to_worker(ctx, &RescanContext {
                session_id: msg.session_id.clone(),
            });
        }
    }
}
