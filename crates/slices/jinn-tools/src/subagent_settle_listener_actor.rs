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

//! One-shot discovery settlement listener for the `spawn_subagent` tool.
//!
//! Bridges the child session's discovery bus traffic (context-files scan,
//! skills scan, prompt-template scan, MCP server status) to the awaiting
//! `spawn_subagent` future through a oneshot channel. Spawned *before* `SessionCreated`
//! is published so no discovery event can slip past the subscription.
//!
//! The ledger settles when all three scan events have arrived for the child
//! — regardless of their `error` field, a finished scan is resolved — and
//! every expected MCP server has reached a terminal connection state
//! ([`McpConnectionStatus::Running`] or [`McpConnectionStatus::Dead`]).
//! `Starting` is not terminal and never settles the ledger.

use std::collections::BTreeSet;

use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_core_types::SessionId;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};
use jinn_session_init_msg::{ContextFilesLoaded, PromptTemplatesLoaded};
use jinn_skills_msg::SkillsLoaded;

/// Dependencies for spawning a [`SubagentSettleListenerActor`].
#[derive(Debug)]
pub struct SubagentSettleListenerDeps {
    /// The system to spawn onto (the same fabric `bus` publishes through).
    pub system: trouper::system::ActorSystem,
    /// The bus to subscribe with (topic + routed-topic resolution).
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
pub struct SubagentSettleListenerActor {
    child_id: SessionId,
    /// Servers still awaiting a terminal status; shrinks to empty.
    pending_servers: BTreeSet<String>,
    settled: Option<tokio::sync::oneshot::Sender<()>>,
    context_files_done: bool,
    skills_done: bool,
    prompt_templates_done: bool,
}

impl ServiceActor for SubagentSettleListenerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::init is async by trait contract; this impl never awaits"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("SubagentSettleListenerActor spawns via init_with"))
    }
}

impl SubagentSettleListenerActor {
    /// Spawns the listener onto the trouper system.
    ///
    /// Returns only after all four subscriptions are live: the `spawn_subagent` tool
    /// guarantees `SessionCreated` is published after this call, closing
    /// the event-ordering race.
    pub fn spawn(deps: SubagentSettleListenerDeps) -> ActorPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = ActorPath::new(format!(
            "jinn.tools.subagent-settle-listener.{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        trouper::builder::spawn_service_builder::<Self>(&deps.system)
            .at(path.clone())
            .init_with({
                let child_id = deps.child_id.clone();
                let expected_servers = deps.expected_servers.clone();
                let settled = deps.settled;
                move || {
                    let child_id = child_id.clone();
                    let expected_servers = expected_servers.clone();
                    Box::pin(async move {
                        Ok(Self {
                            child_id,
                            pending_servers: expected_servers,
                            settled: Some(settled),
                            context_files_done: false,
                            skills_done: false,
                            prompt_templates_done: false,
                        })
                    })
                }
            })
            .handles::<ContextFilesLoaded>()
            .handles::<SkillsLoaded>()
            .handles::<PromptTemplatesLoaded>()
            .handles::<McpServerStatus>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        // The declarations are the readiness point: the discovery
        // workers publish their results only after this returns.
        path
    }
}

impl MsgHandler<ContextFilesLoaded> for SubagentSettleListenerActor {
    async fn handle(&mut self, msg: &ContextFilesLoaded, ctx: &mut MsgCtx<'_>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.context_files_done = true;
        self.check(ctx);
    }
}

impl MsgHandler<SkillsLoaded> for SubagentSettleListenerActor {
    async fn handle(&mut self, msg: &SkillsLoaded, ctx: &mut MsgCtx<'_>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.skills_done = true;
        self.check(ctx);
    }
}

impl MsgHandler<PromptTemplatesLoaded> for SubagentSettleListenerActor {
    async fn handle(&mut self, msg: &PromptTemplatesLoaded, ctx: &mut MsgCtx<'_>) {
        if self.aborted(ctx) || msg.session_id != self.child_id {
            return;
        }
        self.prompt_templates_done = true;
        self.check(ctx);
    }
}

impl MsgHandler<McpServerStatus> for SubagentSettleListenerActor {
    async fn handle(&mut self, msg: &McpServerStatus, ctx: &mut MsgCtx<'_>) {
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

impl SubagentSettleListenerActor {
    /// Abort path: the awaiting `spawn_subagent` future was dropped (parent tool batch
    /// cancelled), closing the channel. There is nothing left to signal —
    /// stop listening. Bus traffic gives us the chance to notice.
    fn aborted(&self, ctx: &mut MsgCtx<'_>) -> bool {
        let closed = self
            .settled
            .as_ref()
            .is_none_or(tokio::sync::oneshot::Sender::is_closed);
        if closed {
            ctx.stop_self();
        }
        closed
    }

    /// Settles when every ledger entry is resolved: all three scans arrived
    /// and no server is still pending.
    fn check(&mut self, ctx: &mut MsgCtx<'_>) {
        let quorum_met = self.context_files_done
            && self.skills_done
            && self.prompt_templates_done
            && self.pending_servers.is_empty();
        if quorum_met {
            if let Some(settled) = self.settled.take() {
                let _ = settled.send(());
            }
            ctx.stop_self();
        }
    }
}
