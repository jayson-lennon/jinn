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
//! - [`SessionPhaseChanged`] with a new phase of `Idle` — drains the
//!   steering buffer FIRST (steering always becomes the next turn; the
//!   queued item waits for the following idle slot), falling back to the
//!   turn queue only when no fragment is pending: `UserMessage` items run
//!   [`Self::dispatch_user_message`], `ToolContinuation` items run
//!   [`Self::dispatch_resume`].
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
//! - `ToolContinuation` (queued) → normalize loop layout, assemble,
//!   publish `SendToLlmProvider`
//! - prepared turn → admission ask (a queued resume whose generation was
//!   cancelled is refused at mint, publishing nothing), steering drain (a
//!   fragment landing between preparation and dispatch still makes the
//!   turn), normalize loop layout, assemble, resolve model (mutating the
//!   alloy round-robin index), push the outgoing token record,
//!   publish `SendToLlmProvider`, `PersistSession`
//!
//! Steering fragments are dispatched only from the idle transition and the
//! prepared-turn drain — a queued/resumed turn never absorbs buffered
//! fragments, so each submitted message gets its own turn and its own LLM
//! response. All paths normalize loop layout, so committed loops never
//! contain interstitials in the request.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_chat_input_msg::ChatEntrySubmitted;
use jinn_context_assembly::inputs::build_assembly_inputs;
use jinn_context_assembly::inputs_snapshot::assemble_via_service;
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::{ChatEntry, ChatEntryKind, ReasoningEffort, SessionId};
use jinn_inference_msg::{SendToLlmProvider, StreamOrigin};
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::services::Services;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::common::state::State;
use jinn_provider_selection::attachment_gate::evaluate_attachment_gate;
use jinn_provider_selection_msg::resolve_effort;
use jinn_session_history_msg::HistoryAppended;
use jinn_session_msg::PhaseKind;
use jinn_session_msg::SessionPhaseChanged;
use jinn_session_msg::phase_command::DispatchKind;
use jinn_session_store_msg::PersistSession;
use jinn_slices::AssembledPrompt;
use jinn_token_count_msg::TokenRecord;
use jinn_turn_dispatch_msg::DispatchTurn;
use jinn_turn_dispatch_msg::QueueItem;

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
    async fn start(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<RegistryError>> {
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
                    Box::pin(async move { Ok(Self { state, services }) })
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

    /// Handle Idle transition — drain the steering buffer first (steering
    /// always becomes the next turn), falling back to the queue only when
    /// no fragment is pending.
    async fn handle_idle_transition(&self, session_id: &SessionId) {
        let item = {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(session_id);
                // Steering takes priority: a steered fragment must become its
                // own turn (with its own LLM response), so the queued item
                // waits for the following idle slot. Fall back to the queue
                // only when the buffer is empty.
                session
                    .steering_buffer_mut()
                    .drain_into_entry()
                    .map(|entry| QueueItem::UserMessage(Box::new(entry)))
                    .or_else(|| session.dequeue())
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

    /// Dispatch a user message: admission ask (a fresh turn always mints,
    /// or the dispatch aborts with history untouched), vision gate, push
    /// to history, set title, assemble prompt, publish
    /// `SendToLlmProvider`, `ChatEntrySubmitted`, `PersistSession`. The
    /// phase transition itself is the admission — this path publishes no
    /// phase event of its own.
    async fn dispatch_user_message(
        &self,
        session_id: &SessionId,
        entry: &ChatEntry,
        origin: StreamOrigin,
    ) {
        // Admission first, before the entry is pushed: a refused mint
        // must leave history untouched (the caller re-queues the entry).
        // A user message always mints a fresh generation — `FreshTurn` —
        // so the only refusal is a dead actor, not a state refusal.
        let stamp = jiff::Timestamp::now();
        let decision = crate::dispatch::admit_begin_stream(
            &self.services,
            session_id,
            DispatchKind::FreshTurn,
            stamp,
        )
        .await;
        if !decision.admitted {
            tracing::warn!(
                session_id = %session_id,
                "queue could not reach the phase actor; dispatch aborted before the entry landed"
            );
            return;
        }

        // Vision gate: if the entry carries attachments but the active model
        // is not confirmed image-capable, push entry + error and abort dispatch
        // (no begin_sending, no re-enqueue). Mirrors the Idle-path gate.
        if let Some(error_entry) = self.evaluate_gate(session_id, entry) {
            self.state.with_session(|view| {
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

        {
            self.state.with_session(|view| {
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
                // Normalize loop layout so committed loops never contain
                // interstitials before assembly.
                session.edit_history().normalize_loop_layout();
            })
        };

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
            dispatched_at: stamp,
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
    /// routing endpoint for dispatch.
    ///
    /// The routing endpoint is a per-model default in `providers.toml`, not
    /// session state, so it is looked up in the registry the process booted
    /// with. A model with no `[[endpoint_defaults]]` row auto-routes, and an
    /// alloy never has a row consulted at all: its members rotate, so a row
    /// keyed to one of them would silently pin whichever member that turn
    /// happened to land on, which is not what a pin means.
    fn resolve_dispatch_model(
        &self,
        session_id: &SessionId,
    ) -> (
        Option<String>,
        Option<String>,
        Option<ReasoningEffort>,
        Option<String>,
    ) {
        let registry = self.services.provider_registry.clone();
        self.state.with_session(|view| {
            let profile = view
                .session
                .map()
                .get_unchecked_mut(session_id)
                .profile_mut();
            let reasoning_effort = resolve_effort(profile.reasoning_effort);
            if profile.model.is_no_provider() {
                (None, None, reasoning_effort, None)
            } else {
                // Read the shape before `resolve_model` consumes the alloy's
                // rotation, so the guard describes the selection the caller
                // asked for rather than whichever member it rotated to.
                let is_single = matches!(profile.model, ModelSelection::Single(_));
                let resolved = profile.model.resolve_model();
                let endpoint_tag = if is_single {
                    registry.pinned_endpoint_tag(&resolved)
                } else {
                    None
                };
                (
                    Some(resolved.clone()),
                    Some(resolved),
                    reasoning_effort,
                    endpoint_tag,
                )
            }
        })
    }

    /// Dispatch a queued tool continuation: normalize loop layout, assemble,
    /// publish `SendToLlmProvider`. No history writes (a queued continuation
    /// is a re-send of the current history from the Idle state), no phase
    /// writes, and no token record — the session actor's tool-loop path owns
    /// those for real continuations.
    async fn dispatch_resume(&self, session_id: &SessionId, origin: StreamOrigin) {
        // Admission first: a continuation for a cancelled generation is
        // refused here — the phase actor is the one writer, and a dead
        // generation minted no stream. Nothing reaches the provider.
        let stamp = jiff::Timestamp::now();
        let decision = crate::dispatch::admit_begin_stream(
            &self.services,
            session_id,
            DispatchKind::ToolContinuation,
            stamp,
        )
        .await;
        if !decision.admitted {
            tracing::info!(
                session_id = %session_id,
                "queue refused a tool continuation: its generation was cancelled"
            );
            return;
        }

        // Normalize loop layout so committed loops never contain
        // interstitials before assembly. Steering fragments are NOT drained
        // here — steering waits for its own turn at the next idle slot.
        {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(session_id);
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
            dispatched_at: stamp,
        })
        .await;
    }

    /// Dispatch the session's prepared turn (the [`DispatchTurn`] body):
    /// drain any steering fragment submitted in the window between the
    /// dispatching path's preparation and this command's execution (the
    /// primary steer-first drain for this path), assemble the prompt,
    /// resolve the model (mutating the alloy round-robin index under write
    /// lock), transition the phase to Streaming, push the outgoing token
    /// record, publish `SessionPhaseChanged` (no-op safe),
    /// `SendToLlmProvider`, and `PersistSession`.
    ///
    /// The in-flight-stream guard is armed by the session actor's own
    /// `SendToLlmProvider` subscription — the single write point — not
    /// here.
    async fn dispatch_prepared(&self, session_id: &SessionId) {
        // Admission first: this path dispatches a turn prepared before the
        // queue received it (a queued resume, or a steering handoff), so it
        // arrives as `ResumeTurn`. A cancel that landed while the turn sat
        // queued has already killed its generation — the ask refuses, and
        // nothing downstream of this point happens. This is the fix site
        // for the phase flap: the phase can no longer re-enter `Streaming`
        // behind a dead turn because the dispatch never starts.
        let stamp = jiff::Timestamp::now();
        let decision = crate::dispatch::admit_begin_stream(
            &self.services,
            session_id,
            DispatchKind::ResumeTurn,
            stamp,
        )
        .await;
        if !decision.admitted {
            tracing::warn!(
                session_id = %session_id,
                "queue refused a prepared dispatch: its generation was cancelled"
            );
            return;
        }

        // Steer-first drain: a fragment submitted in the window between the
        // dispatching path's history push and this command's execution must
        // still make this turn (it arrived while the turn was being
        // prepared, so the user intended it to steer the ongoing dispatch).
        {
            self.state.with_session(|view| {
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
        // record the outgoing token count against the stamp the phase
        // actor admitted. The transition itself already happened — the
        // admission above IS the phase write.
        let registry = self.services.provider_registry.clone();
        let (provider_id, model_used, reasoning_effort, endpoint_tag) = {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(session_id);
                let reasoning_effort = resolve_effort(session.profile().reasoning_effort);
                let model = &mut session.profile_mut().model;
                let (provider_id, model_used) = if model.is_no_provider() {
                    (None, None)
                } else {
                    let resolved = model.resolve_model();
                    (Some(resolved.clone()), Some(resolved))
                };
                // The routing endpoint is keyed by model, so it is resolved
                // from the member `resolve_model` just produced — but only
                // for a `Single` selection. An alloy rotates, and a row keyed
                // to one of its members would pin whichever member this turn
                // landed on rather than expressing a choice.
                let endpoint_tag = match (&session.profile().model, &model_used) {
                    (ModelSelection::Single(_), Some(id)) => registry.pinned_endpoint_tag(id),
                    _ => None,
                };
                session.push_token_record(TokenRecord {
                    model_used: model_used.clone(),
                    timestamp: stamp,
                    tokens_sent: estimated_tokens,
                    tokens_received: 0,
                    cost: None,
                    prompt_tokens: None,
                    cached_tokens: None,
                });
                (provider_id, model_used, reasoning_effort, endpoint_tag)
            })
        };

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
            dispatched_at: stamp,
        })
        .await;

        self.publish(PersistSession {
            session_id: session_id.clone(),
        })
        .await;
    }
}

impl MsgHandler<SessionPhaseChanged> for QueueActor {
    async fn handle(&mut self, msg: &SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_phase_changed(msg).await;
    }
}

impl MsgHandler<DispatchTurn> for QueueActor {
    async fn handle(&mut self, msg: &DispatchTurn, _ctx: &mut MsgCtx<'_>) {
        self.handle_dispatch_turn(msg).await;
    }
}

#[cfg(test)]
mod queue_actor_tests;
