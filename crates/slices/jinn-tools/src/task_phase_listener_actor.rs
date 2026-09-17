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

use kameo::prelude::{Actor, ActorRef, Context, Message};

use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::feat::session::phase_machine::PhaseKind;
use jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use jinn_domain::protocol::SessionId;

/// Dependencies for spawning a [`TaskPhaseListenerActor`].
#[derive(Debug)]
pub struct TaskPhaseListenerDeps {
    /// The bus to subscribe to for `SessionPhaseChanged` events.
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

impl Actor for TaskPhaseListenerActor {
    type Args = TaskPhaseListenerDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        // Subscribe before returning: the spawn's `wait_for_startup` in the
        // `task` tool guarantees the subscription exists before
        // `SessionCreated` is published, closing the event-ordering race.
        args.bus
            .subscribe::<SessionPhaseChanged, _>(&actor_ref)
            .await;
        Ok(Self {
            child_id: args.child_id,
            completion: Some(args.completion),
        })
    }
}

impl Message<SessionPhaseChanged> for TaskPhaseListenerActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionPhaseChanged, ctx: &mut Context<Self, Self::Reply>) {
        // Abort path: the awaiting `task` future was dropped (parent tool
        // batch cancelled), closing the channel. There is nothing left to
        // signal — stop listening. Bus traffic gives us the chance to notice.
        if self
            .completion
            .as_ref()
            .is_none_or(tokio::sync::oneshot::Sender::is_closed)
        {
            ctx.stop();
            return;
        }

        // Match any transition into Idle regardless of the old phase: the
        // cancel path force-publishes Idle→Idle, and the listener must not
        // miss it.
        if msg.session_id == self.child_id && msg.new_phase == PhaseKind::Idle {
            if let Some(completion) = self.completion.take() {
                let _ = completion.send(());
            }
            ctx.stop();
        }
    }
}
