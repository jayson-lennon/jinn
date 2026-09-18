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

//! The queue actor — sole owner of turn dispatch queue consumption.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's `jinn.turn-dispatch`
//! topic (fed by the kernel bridge's forward routes). Two triggers, three
//! dispatch bodies:
//!
//! - [`SessionPhaseChanged`] with a new phase of `Idle` — pops the next
//!   item from the turn queue (falling back to the steering buffer):
//!   `UserMessage` items run [`Self::dispatch_user_message`],
//!   `ToolContinuation` items run [`Self::dispatch_resume`].
//! - [`DispatchTurn`] — the session actor has prepared a turn (idle-direct
//!   send, resume tail, stall-retry re-dispatch: eligibility checked,
//!   entries pushed) and asks this slice to dispatch it:
//!   [`Self::dispatch_prepared`].
//!
//! # Dispatch behavior
//!
//! - `UserMessage` → vision gate, push entry, set title, begin sending,
//!   assemble via the context-assembly service, publish
//!   `SendToLlmProvider`, `ChatEntrySubmitted`, `PersistSession`
//! - `ToolContinuation` (queued) → steering drain, normalize loop layout,
//!   assemble, publish `SendToLlmProvider`
//! - prepared turn → steering drain, normalize loop layout, assemble,
//!   resolve model (mutating the alloy round-robin index), transition to
//!   Streaming, push the outgoing token record, publish
//!   `SendToLlmProvider`, `PersistSession`
//!
//! Every path drains pending steering fragments into history before
//! assembly and normalizes loop layout, so committed loops never contain
//! interstitials in the request.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_domain::common::actor_deps::BusPublish;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::common::state::State;
use jinn_domain::common::tcaps::SessionCap;
use jinn_domain::feat::chat_input::protocol::event::ChatEntrySubmitted;
use jinn_domain::feat::context::snapshot::{assemble_via_service, build_assembly_inputs};
use jinn_domain::feat::session::phase_machine::PhaseKind;
use jinn_domain::feat::session::protocol::history_appended::HistoryAppended;
use jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use jinn_domain::feat::session::queue_item::QueueItem;
use jinn_domain::feat::session::session_actor::evaluate_attachment_gate;
use jinn_domain::feat::session::token_stats::TokenRecord;
use jinn_domain::feat::session_lifecycle::protocol::command::PersistSession;
use jinn_domain::protocol::{ChatEntry, ChatEntryKind, SessionId};
use jinn_inference_msg::{SendToLlmProvider, StreamOrigin};
use jinn_slices::AssembledPrompt;
use jinn_turn_dispatch_msg::DispatchTurn;

/// The queue actor's static trouper path.
pub const QUEUE_PATH: &str = "queue";

/// The queue actor.
///
/// The sole consumer of the turn dispatch queue. Reacts to session phase
/// transitions to `Idle` by popping and dispatching queued items, and to
/// [`DispatchTurn`] commands by dispatching the session's prepared turn.
pub struct QueueActor {
    /// Shared application state (read/write access to session queue and data).
    state: State,
    /// Application-wide runtime services (bus publish, assembly ask, paths).
    services: Services,
    /// Authority to write the session capsule.
    cap: SessionCap,
}

impl BusPublish for QueueActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl ServiceActor for QueueActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects state and services via
        // `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("QueueActor is spawned via start_with"),
        )
    }
}

impl QueueActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the turn-dispatch topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, state: State, services: Services) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(QUEUE_PATH))
            .start_with({
                move || {
                    let state = state.clone();
                    let services = services.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            services,
                            cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                        })
                    })
                }
            })
            .handles::<SessionPhaseChanged>()
            .handles::<DispatchTurn>()
            .start()
    }

    /// Handle `SessionPhaseChanged` — dispatch on phase transitions.
    pub async fn handle_session_phase_changed(&self, payload: &SessionPhaseChanged) {
        if payload.new_phase == PhaseKind::Idle {
            self.handle_idle_transition(&payload.session_id).await;
        }
    }

    /// Handle [`DispatchTurn`] — dispatch the session's prepared turn.
    pub async fn handle_dispatch_turn(&self, payload: &DispatchTurn) {
        self.dispatch_prepared(&payload.session_id).await;
    }

    /// Handle Idle transition — pop and dispatch the next queued item,
    /// falling back to the steering buffer when the queue is empty.
    async fn handle_idle_transition(&self, session_id: &SessionId) {
        let item = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                // Queue takes priority. Fall back to the steering buffer so a fragment
                // submitted mid-turn dispatches itself when the turn completes with an
                // empty queue — same semantics as a queued user message.
                session.dequeue().or_else(|| {
                    session
                        .steering_buffer_mut()
                        .drain_into_entry()
                        .map(|entry| QueueItem::UserMessage(Box::new(entry)))
                })
            })
        };

        let Some(item) = item else { return };

        match item {
            QueueItem::UserMessage(entry) => {
                self.dispatch_user_message(session_id, &entry, StreamOrigin::User)
                    .await;
            }
            QueueItem::ToolContinuation => {
                self.dispatch_resume(session_id, StreamOrigin::ToolContinuation)
                    .await;
            }
        }
    }

    /// Evaluates the vision-capability gate for a queued user entry carrying
    /// attachments. Returns `Some(error_entry)` when the active model is not
    /// confirmed image-capable (known text-only or unknown); `None` otherwise
    /// (text-only entry, or a confirmed vision model).
    ///
    /// Mirrors the session actor's Idle-path gate, ensuring queued messages
    /// are gated identically.
    fn evaluate_gate(&self, session_id: &SessionId, entry: &ChatEntry) -> Option<ChatEntry> {
        evaluate_attachment_gate(&self.services, &self.state, session_id, entry)
    }

    /// Assembles the prompt for `session_id` via the context-assembly
    /// service. Returns `None` (and logs) when assembly fails — the caller
    /// must abort the dispatch without publishing `SendToLlmProvider`.
    async fn assemble(&self, session_id: &SessionId, label: &str) -> Option<AssembledPrompt> {
        let inputs = {
            let guard = self.state.read();
            build_assembly_inputs(&guard, session_id)
        };
        match assemble_via_service(&self.services, inputs).await {
            Ok(prompt) => Some(prompt),
            Err(error) => {
                tracing::error!(
                    error = ?error,
                    session_id = %session_id,
                    "context assembly failed; {label} dispatch aborted"
                );
                None
            }
        }
    }

    /// Dispatch a user message: vision gate, push to history, set title,
    /// begin sending, publish SessionPhaseChanged (if the phase actually
    /// changed), assemble prompt, publish SendToLlmProvider,
    /// ChatEntrySubmitted, PersistSession.
    async fn dispatch_user_message(
        &self,
        session_id: &SessionId,
        entry: &ChatEntry,
        origin: StreamOrigin,
    ) {
        // Vision gate: if the entry carries attachments but the active model
        // is not confirmed image-capable, push entry + error and abort dispatch
        // (no begin_sending, no re-enqueue). Mirrors the Idle-path gate.
        if let Some(error_entry) = self.evaluate_gate(session_id, entry) {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                session.push_entry(entry.clone());
                session.push_entry(error_entry);
            });
            self.publish(HistoryAppended {
                session_id: session_id.clone(),
            })
            .await;
            self.publish(PersistSession {
                session_id: session_id.clone(),
            })
            .await;
            return;
        }

        let (old_phase, new_phase) = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                if session.title().is_none() {
                    let title = match &entry.kind {
                        ChatEntryKind::User { display, .. } => {
                            display.lines().next().unwrap_or("").to_owned()
                        }
                        _ => String::new(),
                    };
                    session.set_title(title);
                }
                session.push_entry(entry.clone());
                // Drain any pending steering fragments into history before assembly.
                if let Some(steer_entry) = session.steering_buffer_mut().drain_into_entry() {
                    let entry_id = steer_entry.id.clone();
                    session.push_entry(steer_entry);
                    tracing::debug!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        "drained steering entry into history at queue_actor::dispatch_user_message"
                    );
                }
                // Normalize loop layout so committed loops never contain
                // interstitials before assembly.
                session.edit_history().normalize_loop_layout();
                let old_phase = session.phase();
                session.begin_sending();
                (old_phase, session.phase())
            })
        };

        if old_phase != new_phase {
            self.publish(SessionPhaseChanged {
                session_id: session_id.clone(),
                old_phase,
                new_phase,
            })
            .await;
        }

        let Some(assembled) = self.assemble(session_id, "user message").await else {
            return;
        };

        let (provider_id, model_used, reasoning_effort, endpoint_tag) =
            self.resolve_dispatch_model(session_id);

        let estimated_tokens = assembled.estimated_tokens();

        self.publish(SendToLlmProvider {
            origin,
            model_used,
            reasoning_effort,
            endpoint_tag,
            session_id: session_id.clone(),
            messages: assembled.messages,
            system_prompt: assembled.system_prompt,
            provider_id,
            estimated_tokens,
            tool_definitions: assembled.tool_definitions,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

        self.publish(ChatEntrySubmitted {
            session_id: session_id.clone(),
            entry: entry.clone(),
        })
        .await;

        self.publish(PersistSession {
            session_id: session_id.clone(),
        })
        .await;
    }

    /// Reads the active session's resolved model, reasoning effort, and
    /// endpoint pin for dispatch. The endpoint pin applies only to a Single
    /// model; alloys rotate members and never pin.
    fn resolve_dispatch_model(
        &self,
        session_id: &SessionId,
    ) -> (
        Option<String>,
        Option<String>,
        Option<jinn_domain::ReasoningEffort>,
        Option<String>,
    ) {
        self.state.with_session(&self.cap, |view| {
            let profile = view
                .session
                .map()
                .get_unchecked_mut(session_id)
                .profile_mut();
            let reasoning_effort = jinn_domain::resolve_effort(profile.reasoning_effort);
            // Endpoint pin applies only to a Single model; alloys rotate.
            let endpoint_tag = match (&profile.model, &profile.endpoint) {
                (jinn_core_types::model_selection::ModelSelection::Single(_), Some(ep)) => {
                    Some(ep.tag.clone())
                }
                _ => None,
            };
            if profile.model.is_no_provider() {
                (None, None, reasoning_effort, None)
            } else {
                let resolved = profile.model.resolve_model();
                (
                    Some(resolved.clone()),
                    Some(resolved),
                    reasoning_effort,
                    endpoint_tag,
                )
            }
        })
    }

    /// Dispatch a queued tool continuation: steering drain, normalize loop
    /// layout, assemble, publish `SendToLlmProvider`. No phase writes and
    /// no token record — the session actor's tool-loop path owns those for
    /// real continuations; a queued continuation is a re-send of the
    /// current history from the Idle state.
    async fn dispatch_resume(&self, session_id: &SessionId, origin: StreamOrigin) {
        // Drain any pending steering fragments into history before assembly,
        // then normalize loop layout so committed loops never contain
        // interstitials before assembly.
        {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                if let Some(entry) = session.steering_buffer_mut().drain_into_entry() {
                    let entry_id = entry.id.clone();
                    session.push_entry(entry);
                    tracing::debug!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        "drained steering entry into history at queue_actor::dispatch_resume"
                    );
                }
                session.edit_history().normalize_loop_layout();
            });
        }

        let Some(assembled) = self.assemble(session_id, "tool continuation").await else {
            return;
        };

        let (provider_id, model_used, reasoning_effort, endpoint_tag) =
            self.resolve_dispatch_model(session_id);

        let estimated_tokens = assembled.estimated_tokens();

        self.publish(SendToLlmProvider {
            origin,
            model_used,
            reasoning_effort,
            endpoint_tag,
            session_id: session_id.clone(),
            messages: assembled.messages,
            system_prompt: assembled.system_prompt,
            provider_id,
            estimated_tokens,
            tool_definitions: assembled.tool_definitions,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    }

    /// Dispatch the session's prepared turn (the [`DispatchTurn`] body):
    /// drain any pending steering fragments (defensive — normally already
    /// drained by an earlier transition, but a fragment can land between
    /// that drain and this dispatch), assemble the prompt, resolve the
    /// model (mutating the alloy round-robin index under write lock),
    /// transition the phase to Streaming, push the outgoing token record,
    /// publish `SessionPhaseChanged` (no-op safe), `SendToLlmProvider`,
    /// and `PersistSession`.
    ///
    /// The in-flight-stream guard is armed by the session actor's own
    /// `SendToLlmProvider` subscription — the single write point — not
    /// here.
    async fn dispatch_prepared(&self, session_id: &SessionId) {
        // Defensive steering drain: the session actor drains before
        // publishing on the paths that own history surgery, but a fragment
        // submitted in the window between that drain and this command's
        // execution must still make the turn. Same semantics as every
        // other dispatch path.
        {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                if let Some(entry) = session.steering_buffer_mut().drain_into_entry() {
                    let entry_id = entry.id.clone();
                    session.push_entry(entry);
                    tracing::debug!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        "drained steering entry into history at queue_actor::dispatch_prepared"
                    );
                }
                session.edit_history().normalize_loop_layout();
            });
        }

        let Some(assembled) = self.assemble(session_id, "prepared turn").await else {
            return;
        };
        let estimated_tokens = assembled.estimated_tokens();

        // Resolve model under write lock (round-robin mutates index), then
        // transition → Streaming and record the outgoing token count. The
        // record carries the resolved model (the direct-send path's former
        // push-then-`set_last_token_model` dance, converged).
        let (provider_id, model_used, reasoning_effort, endpoint_tag, old_phase, new_phase) = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                let reasoning_effort =
                    jinn_domain::resolve_effort(session.profile().reasoning_effort);
                // Snapshot the endpoint tag immutably before mutating the
                // model (alloy round-robin mutates index during
                // resolve_model).
                let endpoint_tag = match (&session.profile().model, &session.profile().endpoint) {
                    (jinn_core_types::model_selection::ModelSelection::Single(_), Some(ep)) => {
                        Some(ep.tag.clone())
                    }
                    _ => None,
                };
                let model = &mut session.profile_mut().model;
                let (provider_id, model_used) = if model.is_no_provider() {
                    (None, None)
                } else {
                    let resolved = model.resolve_model();
                    (Some(resolved.clone()), Some(resolved))
                };
                let old_phase = session.phase();
                session.begin_streaming();
                session.push_token_record(TokenRecord {
                    model_used: model_used.clone(),
                    timestamp: jiff::Timestamp::now(),
                    tokens_sent: estimated_tokens,
                    tokens_received: 0,
                    cost: None,
                    prompt_tokens: None,
                    cached_tokens: None,
                });
                (
                    provider_id,
                    model_used,
                    reasoning_effort,
                    endpoint_tag,
                    old_phase,
                    session.phase(),
                )
            })
        };

        if old_phase != new_phase {
            self.publish(SessionPhaseChanged {
                session_id: session_id.clone(),
                old_phase,
                new_phase,
            })
            .await;
        }

        self.publish(SendToLlmProvider {
            origin: StreamOrigin::User,
            model_used,
            reasoning_effort,
            endpoint_tag,
            session_id: session_id.clone(),
            messages: assembled.messages,
            system_prompt: assembled.system_prompt,
            provider_id,
            estimated_tokens,
            tool_definitions: assembled.tool_definitions,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;

        self.publish(PersistSession {
            session_id: session_id.clone(),
        })
        .await;
    }
}

impl MsgHandler<SessionPhaseChanged> for QueueActor {
    async fn handle(&mut self, msg: SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_phase_changed(&msg).await;
    }
}

impl MsgHandler<DispatchTurn> for QueueActor {
    async fn handle(&mut self, msg: DispatchTurn, _ctx: &mut MsgCtx<'_>) {
        self.handle_dispatch_turn(&msg).await;
    }
}

#[cfg(test)]
#[path = "queue_actor_tests.rs"]
mod tests;
