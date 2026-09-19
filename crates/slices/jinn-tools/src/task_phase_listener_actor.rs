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

//! One-shot completion listener for the `task` tool.
//!
//! Bridges `SessionPhaseChanged` bus traffic to the awaiting `task` future
//! through a oneshot channel. Spawned *before* the child session is published
//! so no completion event can slip past the subscription. Forwards the first
//! `Idle` signal for its child session and then stops itself.

use trouper::actor::ActorPath;
use trouper::context::MsgCtx;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::registry::RegistryError;

use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::common::services::bus_service::jinn_domain_topic;
use jinn_domain::feat::session::phase_machine::PhaseKind;
use jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use jinn_domain::protocol::SessionId;

/// Dependencies for spawning a [`TaskPhaseListenerActor`].
#[derive(Debug)]
pub struct TaskPhaseListenerDeps {
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
/// [`PhaseKind::Idle`]. See the [module docs](self) for the lifecycle.
#[derive(Debug)]
pub struct TaskPhaseListenerActor {
    child_id: SessionId,
    completion: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ServiceActor for TaskPhaseListenerActor {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `start_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("TaskPhaseListenerActor spawns via start_with"))
    }
}

impl TaskPhaseListenerActor {
    /// Spawns the listener onto the trouper system.
    ///
    /// Returns only after the subscription is live: the `task` tool
    /// guarantees `SessionCreated` is published after this call, closing
    /// the event-ordering race.
    pub async fn spawn(deps: TaskPhaseListenerDeps) -> ActorPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = ActorPath::new(format!(
            "jinn.tools.task-phase-listener.{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let bus = deps.bus.clone();
        let child_id = deps.child_id.clone();
        let completion = deps.completion;
        trouper::builder::spawn_service_builder::<Self>(&deps.system)
            .at(path.clone())
            .start_with(move || {
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
        // Subscribe via the bus so routed topics stay the single source of
        // truth. The `task` tool publishes `SessionCreated` only after this
        // subscribe returns.
        bus.subscribe_topic::<SessionPhaseChanged>(&path, &jinn_domain_topic())
            .await;
        path
    }
}

impl MsgHandler<SessionPhaseChanged> for TaskPhaseListenerActor {
    async fn handle(&mut self, msg: SessionPhaseChanged, ctx: &mut MsgCtx<'_>) {
        // Abort path: the awaiting `task` future was dropped (parent tool
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
        if msg.session_id == self.child_id && msg.new_phase == PhaseKind::Idle {
            if let Some(completion) = self.completion.take() {
                let _ = completion.send(());
            }
            ctx.stop_self();
        }
    }
}
