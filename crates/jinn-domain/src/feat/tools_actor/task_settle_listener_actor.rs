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

//! One-shot discovery settlement listener for the `task` tool.
//!
//! Bridges the child session's discovery bus traffic (context-files scan,
//! skills scan, prompt-template scan, MCP server status) to the awaiting
//! `task` future through a oneshot channel. Spawned *before* `SessionCreated`
//! is published so no discovery event can slip past the subscription.
//!
//! The ledger settles when all three scan events have arrived for the child
//! — regardless of their `error` field, a finished scan is resolved — and
//! every expected MCP server has reached a terminal connection state
//! ([`McpConnectionStatus::Running`] or [`McpConnectionStatus::Dead`]).
//! `Starting` is not terminal and never settles the ledger.

use std::collections::BTreeSet;

use kameo::prelude::{Actor, ActorRef, Context, Message};

use crate::common::services::bus_service::BusService;
use crate::feat::context::protocol::event::ContextFilesLoaded;
use crate::feat::provider::protocol::event::PromptTemplatesLoaded;
use crate::feat::skills::SkillsLoaded;
use crate::protocol::SessionId;
use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};

/// Dependencies for spawning a [`TaskSettleListenerActor`].
#[derive(Debug)]
pub struct TaskSettleListenerDeps {
    /// The bus to subscribe to for discovery events.
    pub bus: BusService,
    /// The child session whose discovery is awaited.
    pub child_id: SessionId,
    /// MCP servers the ledger waits on, copied from the child at spawn time.
    /// Each must reach `Running` or `Dead` for settlement.
    pub expected_servers: BTreeSet<String>,
    /// Sender half of the settlement channel; forwarded once the quorum is
    /// met. A closed channel (dropped receiver) stops the actor instead.
    pub settled: tokio::sync::oneshot::Sender<()>,
}

/// A one-shot actor that awaits a single child session's discovery events.
/// See the [module docs](self) for the settlement semantics.
#[derive(Debug)]
pub struct TaskSettleListenerActor {
    child_id: SessionId,
    /// Servers still awaiting a terminal status; shrinks to empty.
    pending_servers: BTreeSet<String>,
    settled: Option<tokio::sync::oneshot::Sender<()>>,
    context_files_done: bool,
    skills_done: bool,
    prompt_templates_done: bool,
}

impl Actor for TaskSettleListenerActor {
    type Args = TaskSettleListenerDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        // Subscribe before returning: the spawn's `wait_for_startup` in the
        // `task` tool guarantees the subscriptions exist before
        // `SessionCreated` is published, closing the event-ordering race.
        args.bus
            .subscribe::<ContextFilesLoaded, _>(&actor_ref)
            .await;
        args.bus.subscribe::<SkillsLoaded, _>(&actor_ref).await;
        args.bus
            .subscribe::<PromptTemplatesLoaded, _>(&actor_ref)
            .await;
        args.bus.subscribe::<McpServerStatus, _>(&actor_ref).await;
        Ok(Self {
            child_id: args.child_id,
            pending_servers: args.expected_servers,
            settled: Some(args.settled),
            context_files_done: false,
            skills_done: false,
            prompt_templates_done: false,
        })
    }
}

impl Message<ContextFilesLoaded> for TaskSettleListenerActor {
    type Reply = ();

    async fn handle(&mut self, msg: ContextFilesLoaded, ctx: &mut Context<Self, Self::Reply>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.context_files_done = true;
        self.check(ctx);
    }
}

impl Message<SkillsLoaded> for TaskSettleListenerActor {
    type Reply = ();

    async fn handle(&mut self, msg: SkillsLoaded, ctx: &mut Context<Self, Self::Reply>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.skills_done = true;
        self.check(ctx);
    }
}

impl Message<PromptTemplatesLoaded> for TaskSettleListenerActor {
    type Reply = ();

    async fn handle(&mut self, msg: PromptTemplatesLoaded, ctx: &mut Context<Self, Self::Reply>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.prompt_templates_done = true;
        self.check(ctx);
    }
}

impl Message<McpServerStatus> for TaskSettleListenerActor {
    type Reply = ();

    async fn handle(&mut self, msg: McpServerStatus, ctx: &mut Context<Self, Self::Reply>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        // Only terminal states settle: a server that came up or one that
        // never will. `Starting` is an in-flight transition, not a result.
        // Removal is idempotent for unknown or duplicate statuses — a
        // settled ledger is never re-opened.
        if matches!(
            msg.status,
            McpConnectionStatus::Running | McpConnectionStatus::Dead
        ) {
            self.pending_servers.remove(&msg.server);
        }
        self.check(ctx);
    }
}

impl TaskSettleListenerActor {
    /// Abort path: the awaiting `task` future was dropped (parent tool batch
    /// cancelled), closing the channel. There is nothing left to signal —
    /// stop listening. Bus traffic gives us the chance to notice.
    fn aborted(&self, ctx: &mut Context<Self, ()>) -> bool {
        let closed = self
            .settled
            .as_ref()
            .is_none_or(tokio::sync::oneshot::Sender::is_closed);
        if closed {
            ctx.stop();
        }
        closed
    }

    /// Settles when every ledger entry is resolved: all three scans arrived
    /// and no server is still pending.
    fn check(&mut self, ctx: &mut Context<Self, ()>) {
        let quorum_met = self.context_files_done
            && self.skills_done
            && self.prompt_templates_done
            && self.pending_servers.is_empty();
        if quorum_met {
            if let Some(settled) = self.settled.take() {
                let _ = settled.send(());
            }
            ctx.stop();
        }
    }
}
