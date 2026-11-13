// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! One-shot completion listener for the `spawn_subagent` tool.
//!
//! Bridges `SessionPhaseChanged` bus traffic to the awaiting `spawn_subagent` future
//! through a oneshot channel. Spawned *before* the child session is published
//! so no completion event can slip past the subscription. Forwards the first
//! `Idle` signal for its child session and then stops itself.

use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_core_types::SessionId;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_session_msg::SessionPhaseChanged;
use jinn_slices::PhaseView;

/// Dependencies for spawning a [`SubagentPhaseListenerActor`].
#[derive(Debug)]
pub struct SubagentPhaseListenerDeps {
    /// The system to spawn onto (the same fabric `bus` publishes through).
    pub system: trouper::system::ActorSystem,
    /// The bus to subscribe with (topic + routed-topic resolution).
    pub bus: BusService,
    /// The child session whose `Idle` transition is awaited.
    pub child_id: SessionId,
    /// Sender half of the completion channel; forwarded on the first
    /// `Idle` transition for `child_id`.
    pub completion: tokio::sync::oneshot::Sender<()>,
}

/// A one-shot actor that awaits a single child session's transition to
/// [`PhaseView::Idle`]. See the [module docs](self) for the lifecycle.
#[derive(Debug)]
pub struct SubagentPhaseListenerActor {
    child_id: SessionId,
    completion: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ServiceActor for SubagentPhaseListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::init is async by trait contract; this impl never awaits"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("SubagentPhaseListenerActor spawns via init_with"))
    }
}

impl SubagentPhaseListenerActor {
    /// Spawns the listener onto the trouper system.
    ///
    /// Returns only after the subscription is live: the `spawn_subagent` tool
    /// guarantees `SessionCreated` is published after this call, closing
    /// the event-ordering race.
    pub fn spawn(deps: SubagentPhaseListenerDeps) -> ActorPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = ActorPath::new(format!(
            "jinn.tools.subagent-phase-listener.{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let child_id = deps.child_id.clone();
        let completion = deps.completion;
        trouper::builder::spawn_service_builder::<Self>(&deps.system)
            .at(path.clone())
            .init_with(move || {
                let child_id = child_id.clone();
                let completion = completion;
                Box::pin(async move {
                    Ok(Self {
                        child_id,
                        completion: Some(completion),
                    })
                })
            })
            .handles::<SessionPhaseChanged>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        // The declaration is the readiness point: the `spawn_subagent` tool
        // publishes phase events only after this returns.
        path
    }
}

impl MsgHandler<SessionPhaseChanged> for SubagentPhaseListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "MsgHandler::handle is async by trait contract; this impl never awaits"
    )]
    async fn handle(&mut self, msg: &SessionPhaseChanged, ctx: &mut MsgCtx<'_>) {
        // Abort path: the awaiting `spawn_subagent` future was dropped (parent tool
        // batch cancelled), closing the channel. There is nothing left to
        // signal — stop listening. Bus traffic gives us the chance to notice.
        if self
            .completion
            .as_ref()
            .is_none_or(tokio::sync::oneshot::Sender::is_closed)
        {
            ctx.stop_self();
            return;
        }

        // Match any transition into Idle regardless of the old phase: the
        // cancel path force-publishes Idle→Idle, and the listener must not
        // miss it.
        if msg.session_id == self.child_id && msg.new_phase == PhaseView::Idle {
            if let Some(completion) = self.completion.take() {
                let _ = completion.send(());
            }
            ctx.stop_self();
        }
    }
}
