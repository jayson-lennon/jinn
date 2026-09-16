//! The `context-assembly` trouper service actor.
//!
//! Stateless by construction: it holds nothing and reads nothing from
//! shared state — every `AssembleContext` carries the full snapshot its
//! caller snapshotted, and the reply is the assembled prompt.

use error_stack::Report;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_domain::feat::context::protocol::inputs::{AssembleContext, AssembledResponse};

use crate::assemble::assemble;

/// The static path the service registers at.
pub const CONTEXT_ASSEMBLY_PATH: &str = "context-assembly";

/// The stateless assembly service.
pub struct ContextAssemblyService;

impl ServiceActor for ContextAssemblyService {
    async fn start(_args: &serde_json::Value) -> Result<Self, Report<RegistryError>> {
        Ok(Self)
    }
}

impl MsgHandler<AssembleContext> for ContextAssemblyService {
    async fn handle(&mut self, msg: AssembleContext, ctx: &mut MsgCtx<'_>) {
        let counter =
            jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter::o200k_base();
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
        .mailbox(64, trouper::inbox::OverloadPolicy::Block)
        .start()
}

/// Spawns the service unless its path is already live. Test harnesses
/// compose the same system through several constructors; re-spawning an
/// identical manifest trips trouper's once-only path invariant, which
/// this tolerates by keeping the first registration.
#[must_use]
pub fn ensure_spawned(system: &trouper::system::ActorSystem) -> Option<ActorPath> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(system)));
    match result {
        Ok(path) => Some(path),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::state::State;
    use jinn_domain::feat::context::protocol::inputs::AssembleContext;
    use jinn_domain::feat::context::snapshot::build_assembly_inputs;
    use jinn_domain::protocol::ChatEntry;

    #[rstest::rstest]
    #[tokio::test]
    async fn ask_returns_assembled_prompt() {
        let mut services = jinn_domain::Services::new_fake().await;
        crate::service::spawn(&services.trouper_system);
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = state.read().session.active_session_id().clone();
        {
            let mut guard = state.write_test_no_cap();
            guard
                .active_session_mut()
                .push_entry(ChatEntry::user("hello world"));
        }
        let inputs = {
            let guard = state.read();
            build_assembly_inputs(&guard, &session_id)
        };
        let reply = services
            .trouper_system
            .ask(
                ActorPath::new(CONTEXT_ASSEMBLY_PATH),
                AssembleContext { inputs },
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("ask succeeds");
        let response: AssembledResponse = serde_json::from_value(reply).expect("reply decodes");
        assert_eq!(response.prompt.session_id, session_id);
        assert!(
            !response.prompt.messages.is_empty(),
            "history becomes messages"
        );
    }
}
