//! One-shot completion listener for the `run_lifecycle` tool.
//!
//! Bridges `SessionSetupCompleted` / `SessionTeardownFinished` bus traffic
//! to the awaiting tool future through a oneshot channel, keyed on the
//! session the script ran for. Spawned *before* the run command publishes
//! so no completion event can slip past the subscription. Forwards the
//! first completion for its session and then stops itself.

use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_core_types::SessionId;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_session_lifecycle_msg::{SessionSetupCompleted, SessionTeardownFinished};
use marked_path::{Absolute, MarkedPathBuf};

/// Which single event this listener awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitKind {
    /// The session's setup completed.
    Setup,
    /// The session's teardown finished.
    Teardown,
}

/// The completion a listener forwards: which kind, and the outcome.
#[derive(Debug)]
pub struct LifecycleCompletion {
    /// Which script phase finished.
    pub kind: AwaitKind,
    /// The adopted cwd (setup only; `None` on the teardown kind).
    pub cwd: Option<MarkedPathBuf<Absolute>>,
    /// The failure description, when the script failed.
    pub error: Option<String>,
}

/// Dependencies for spawning a [`LifecycleCompletionListenerActor`].
#[derive(Debug)]
pub struct LifecycleCompletionListenerDeps {
    /// The system to spawn onto (the same fabric `bus` publishes through).
    pub system: trouper::system::ActorSystem,
    /// The bus to subscribe with.
    pub bus: BusService,
    /// The session the script runs for.
    pub session_id: SessionId,
    /// Which completion this listener awaits.
    pub kind: AwaitKind,
    /// Sender half of the completion channel.
    pub completion: tokio::sync::oneshot::Sender<LifecycleCompletion>,
}

/// A one-shot actor that awaits one lifecycle completion for one session.
#[derive(Debug)]
pub struct LifecycleCompletionListenerActor {
    session_id: SessionId,
    kind: AwaitKind,
    completion: Option<tokio::sync::oneshot::Sender<LifecycleCompletion>>,
}

impl ServiceActor for LifecycleCompletionListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::init is async by trait contract; this impl never awaits"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("LifecycleCompletionListenerActor spawns via init_with"))
    }
}

impl LifecycleCompletionListenerActor {
    /// Spawns the listener onto the trouper system, returning only after
    /// the subscription is live.
    pub fn spawn(deps: LifecycleCompletionListenerDeps) -> ActorPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = ActorPath::new(format!(
            "jinn.tools.lifecycle-completion-listener.{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let session_id = deps.session_id.clone();
        let kind = deps.kind;
        let completion = deps.completion;
        trouper::builder::spawn_service_builder::<Self>(&deps.system)
            .at(path.clone())
            .init_with(move || {
                let session_id = session_id.clone();
                let completion = completion;
                Box::pin(async move {
                    Ok(Self {
                        session_id,
                        kind,
                        completion: Some(completion),
                    })
                })
            })
            .handles::<SessionSetupCompleted>()
            .handles::<SessionTeardownFinished>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl LifecycleCompletionListenerActor {
    /// Forwards the completion and stops, when the channel is still open.
    fn complete(&mut self, completion: LifecycleCompletion, ctx: &mut MsgCtx<'_>) {
        // The awaiting tool future was dropped (its batch cancelled) closes
        // the channel: nothing left to signal, so stop listening either way.
        if let Some(sender) = self.completion.take() {
            let _ = sender.send(completion);
        }
        ctx.stop_self();
    }
}

impl MsgHandler<SessionSetupCompleted> for LifecycleCompletionListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "MsgHandler::handle is async by trait contract; this impl never awaits"
    )]
    async fn handle(&mut self, msg: &SessionSetupCompleted, ctx: &mut MsgCtx<'_>) {
        if msg.session_id == self.session_id && self.kind == AwaitKind::Setup {
            self.complete(
                LifecycleCompletion {
                    kind: AwaitKind::Setup,
                    cwd: Some(msg.cwd.clone()),
                    error: msg.error.clone(),
                },
                ctx,
            );
        }
    }
}

impl MsgHandler<SessionTeardownFinished> for LifecycleCompletionListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "MsgHandler::handle is async by trait contract; this impl never awaits"
    )]
    async fn handle(&mut self, msg: &SessionTeardownFinished, ctx: &mut MsgCtx<'_>) {
        if msg.session_id == self.session_id && self.kind == AwaitKind::Teardown {
            self.complete(
                LifecycleCompletion {
                    kind: AwaitKind::Teardown,
                    cwd: None,
                    error: msg.error.clone(),
                },
                ctx,
            );
        }
    }
}
