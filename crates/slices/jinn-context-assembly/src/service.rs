//! The `context-assembly` trouper service actor.
//!
//! Stateless by construction: it holds nothing and reads nothing from
//! shared state — every `AssembleContext` carries the full snapshot its
//! caller snapshotted, and the reply is the assembled prompt.

use error_stack::Report;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_context_assembly_msg::{AssembleContext, AssembledResponse};

use crate::assemble::assemble;

/// The static path the service registers at.
pub const CONTEXT_ASSEMBLY_PATH: &str = "context-assembly";

/// The stateless assembly service.
pub struct ContextAssemblyService;

impl ServiceActor for ContextAssemblyService {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "stateless service: start has nothing to await"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        Ok(Self)
    }
}

impl MsgHandler<AssembleContext> for ContextAssemblyService {
    async fn handle(&mut self, msg: &AssembleContext, ctx: &mut MsgCtx<'_>) {
        let counter = jinn_llm_support::token_estimator::TiktokenCounter::o200k_base();
        let prompt = assemble(&msg.inputs, &counter);
        ctx.reply(AssembledResponse {
            session_id: prompt.session_id.clone(),
            prompt,
        });
    }
}

/// Spawns the service at its static path.
#[must_use]
pub fn spawn(system: &trouper::system::ActorSystem) -> ActorPath {
    trouper::builder::spawn_service_builder::<ContextAssemblyService>(system)
        .at(ActorPath::new(CONTEXT_ASSEMBLY_PATH))
        .handles::<AssembleContext>()
        // Ask replies leave the handler through ctx.reply; the flush gate
        // drops any outbound type not declared here.
        .emits::<AssembledResponse>()
        .mailbox(64, trouper::inbox::OverloadPolicy::Block)
        .start()
}

/// Spawns the service unless its path is already live. Test harnesses
/// compose the same system through several constructors; re-spawning an
/// identical manifest trips trouper's once-only path invariant, which
/// this tolerates by keeping the first registration.
///
/// The check happens before spawning, not by containing a panic: trouper
/// registers the slot inside the actor task it spawns for `start`, so a
/// double spawn panics on another thread after `spawn` returns —
/// `catch_unwind` here would never see it.
#[must_use]
pub fn ensure_spawned(system: &trouper::system::ActorSystem) -> Option<ActorPath> {
    let path = ActorPath::new(CONTEXT_ASSEMBLY_PATH);
    let live = {
        let probe = system.export();
        // A multi-thread runtime can take `block_in_place`; anything else
        // (current-thread test runtimes, sync harnesses) gets a private
        // probe runtime on its own thread, so the caller's scheduler is
        // never blocked and never double-driven.
        let multi = tokio::runtime::Handle::try_current()
            .ok()
            .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
        match multi {
            Some(true) => {
                let handle = tokio::runtime::Handle::current();
                tokio::task::block_in_place(|| {
                    handle
                        .block_on(probe)
                        .actors
                        .iter()
                        .any(|actor| actor.path == path)
                })
            }
            _ => {
                let probed = std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .ok()
                                .map(|rt| {
                                    rt.block_on(probe)
                                        .actors
                                        .iter()
                                        .any(|actor| actor.path == path)
                                })
                        })
                        .join()
                });
                probed.unwrap_or_default().unwrap_or(false)
            }
        }
    };
    if live {
        return None;
    }
    Some(spawn(system))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::build_assembly_inputs;
    use jinn_context_assembly_msg::AssembleContext;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_kernel::protocol::ChatEntry;

    #[rstest::rstest]
    #[tokio::test]
    async fn ask_returns_assembled_prompt() {
        // Given a spawned service and a session holding one user entry.
        let services = jinn_kernel::Services::new_fake().await;
        let _ = crate::service::spawn(&services.trouper_system);
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = state.read().session.active_session_id().clone();
        {
            let mut guard = state.write();
            guard
                .active_session_mut()
                .push_entry(ChatEntry::user("hello world"));
        }
        let inputs = {
            let guard = state.read();
            build_assembly_inputs(&guard, jinn_config::empty_config_layer(), &session_id)
        };
        // When asking the service to assemble.
        let reply = services
            .trouper_system
            .ask(
                ActorPath::new(CONTEXT_ASSEMBLY_PATH),
                AssembleContext { inputs },
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("ask succeeds");
        let response: AssembledResponse = reply.decode().expect("reply decodes");

        // Then the assembled prompt belongs to the same session.
        assert_eq!(response.prompt.session_id, session_id);
        assert!(
            !response.prompt.messages.is_empty(),
            "history becomes messages"
        );
    }
}
