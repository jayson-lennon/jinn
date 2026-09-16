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

//! Queue actor - sole owner of turn dispatch queue consumption.
//!
//! Subscribes to [`SessionPhaseChanged`] events.
//! When a session transitions to `Idle`, pops the next item from the turn queue
//! and dispatches it.
//!
//! # Dispatch behavior
//!
//! - `UserMessage` → push entry, set title, begin sending, call `assemble_prompt()` + emit `SendToLlmProvider`
//! - `ToolContinuation` → call `assemble_prompt()` + emit `SendToLlmProvider`

use kameo::prelude::{Actor, ActorRef, Context, Message};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::state::State;
use crate::feat::chat_input::protocol::event::ChatEntrySubmitted;
use crate::feat::context::snapshot::{assemble_via_service, build_assembly_inputs};
use crate::feat::provider::protocol::command::{SendToLlmProvider, StreamOrigin};
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use crate::feat::session::queue_item::QueueItem;
use crate::feat::session_lifecycle::protocol::command::PersistSession;
use crate::protocol::SessionId;

/// The queue actor.
///
/// The sole consumer of the turn dispatch queue. Reacts to session phase
/// transitions to `Idle` by popping and dispatching queued items.
pub struct QueueActor {
    /// Shared application state (read/write access to session queue and data).
    state: State,
    /// Universal actor dependencies (bus, services, etc.).
    deps: ActorDeps,
    /// Authority to write the session capsule.
    cap: crate::common::tcaps::SessionCap,
}

/// Dependencies for [`QueueActor`].
#[derive(Clone)]
pub struct QueueActorDeps {
    /// Shared application state.
    pub state: State,
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// Authority to write the session capsule.
    pub cap: crate::common::tcaps::SessionCap,
}

impl Actor for QueueActor {
    type Args = QueueActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<SessionPhaseChanged>())
            .await;

        Ok(Self {
            state: args.state,
            deps: args.deps,
            cap: args.cap,
        })
    }
}

impl Message<SessionPhaseChanged> for QueueActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionPhaseChanged, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_session_phase_changed(&msg).await;
    }
}

impl BusPublish for QueueActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

impl QueueActor {
    /// Handle `SessionPhaseChanged` - dispatch on phase transitions.
    async fn handle_session_phase_changed(&self, payload: &SessionPhaseChanged) {
        match (payload.old_phase, payload.new_phase) {
            (_, PhaseKind::Idle) => {
                self.handle_idle_transition(&payload.session_id).await;
            }
            _ => {}
        }
    }

    /// Handle Idle transition - pop and dispatch the next queued item,
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
                self.dispatch_tool_continuation(session_id).await;
            }
        }
    }

    /// Evaluates the vision-capability gate for a queued user entry carrying
    /// attachments. Returns `Some(error_entry)` when the active model is not
    /// confirmed image-capable (known text-only or unknown); `None` otherwise
    /// (text-only entry, or a confirmed vision model).
    ///
    /// Mirrors [`SessionPersistenceActor::attachment_gate_blocks`] on the Idle
    /// path, ensuring queued messages are gated identically.
    fn evaluate_attachment_gate(
        &self,
        session_id: &SessionId,
        entry: &crate::protocol::ChatEntry,
    ) -> Option<crate::protocol::ChatEntry> {
        crate::feat::session::session_actor::evaluate_attachment_gate(
            &self.deps.services,
            &self.state,
            session_id,
            entry,
        )
    }

    /// Dispatch a user message: push to history, set title, begin sending,
    /// emit SessionPhaseChanged (if the phase actually changed), assemble
    /// prompt, emit SendToLlmProvider, emit ChatEntrySubmitted, emit PersistSession.
    async fn dispatch_user_message(
        &self,
        session_id: &SessionId,
        entry: &crate::protocol::ChatEntry,
        origin: StreamOrigin,
    ) {
        // Vision gate: if the entry carries attachments but the active model
        // is not confirmed image-capable, push entry + error and abort dispatch
        // (no begin_sending, no re-enqueue). Mirrors the Idle-path gate.
        if let Some(error_entry) = self.evaluate_attachment_gate(session_id, entry) {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                session.push_entry(entry.clone());
                session.push_entry(error_entry);
            });
            self.publish(
                crate::feat::session::protocol::history_appended::HistoryAppended {
                    session_id: session_id.clone(),
                },
            )
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
                        crate::protocol::ChatEntryKind::User { display, .. } => {
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

        let assembled = {
            let inputs = {
                let guard = self.state.read();
                build_assembly_inputs(&guard, session_id)
            };
            match assemble_via_service(&self.deps.services, inputs).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        session_id = %session_id,
                        "context assembly failed; dispatch aborted"
                    );
                    return;
                }
            }
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
        Option<crate::ReasoningEffort>,
        Option<String>,
    ) {
        self.state.with_session(&self.cap, |view| {
            let profile = view
                .session
                .map()
                .get_unchecked_mut(session_id)
                .profile_mut();
            let reasoning_effort = crate::resolve_effort(profile.reasoning_effort);
            // Endpoint pin applies only to a Single model; alloys rotate.
            let endpoint_tag = match (&profile.model, &profile.endpoint) {
                (crate::feat::session::model_selection::ModelSelection::Single(_), Some(ep)) => {
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

    /// Dispatch a tool continuation: assemble prompt and emit SendToLlmProvider.
    ///
    /// See [`Self::dispatch_resume`] for the shared dispatch body.
    async fn dispatch_tool_continuation(&self, session_id: &SessionId) {
        self.dispatch_resume(
            session_id,
            StreamOrigin::ToolContinuation,
            "tool continuation",
        )
        .await;
    }

    /// Shared dispatch body for tool-continuation and manual-resume paths:
    /// re-assemble prompt from current history and emit `SendToLlmProvider`.
    async fn dispatch_resume(&self, session_id: &SessionId, origin: StreamOrigin, label: &str) {
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
                        label = %label,
                        "drained steering entry into history at queue_actor::dispatch_resume"
                    );
                }
                session.edit_history().normalize_loop_layout();
            });
        }
        let assembled = {
            let inputs = {
                let guard = self.state.read();
                build_assembly_inputs(&guard, session_id)
            };
            match assemble_via_service(&self.deps.services, inputs).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        session_id = %session_id,
                        "context assembly failed; dispatch aborted"
                    );
                    return;
                }
            }
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
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::QueueActor;
    use crate::common::actor_deps::ActorDeps;
    use crate::common::app_state::AppState;
    use crate::common::services::Services;
    use crate::common::services::bus_service::BusAudit;
    use crate::common::state::State;
    use crate::feat::chat_input::protocol::event::ChatEntrySubmitted;
    use crate::feat::provider::protocol::command::{SendToLlmProvider, StreamOrigin};
    use crate::feat::session::chat_entry::ChatEntry;
    use crate::feat::session::phase_machine::PhaseKind;
    use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
    use crate::feat::session::queue_item::QueueItem;
    use crate::feat::session_lifecycle::protocol::command::PersistSession;
    use crate::protocol::SessionId;

    async fn create_actor() -> (QueueActor, BusAudit) {
        let (bus, audit) = crate::common::services::BusService::new_recording();
        let mut services = Services::new_fake_with_bus(bus).await;
        let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
        (
            QueueActor {
                state: State::new(AppState::default_with_scope_focus()),
                deps: ActorDeps { services },
                cap: crate::common::tcaps::mint::mint_session_cap(),
            },
            audit,
        )
    }

    fn session_id() -> SessionId {
        SessionId::new()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_transition_dispatches_user_message() {
        // Given a session with a queued user message.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
        }

        // When receiving SessionPhaseChanged → Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Sending,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then SendToLlmProvider was published.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        // And ChatEntrySubmitted was published.
        let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
        assert_eq!(submitted.len(), 1);
        // And PersistSession was published.
        let persists: Vec<PersistSession> = audit.of_type::<PersistSession>();
        assert_eq!(persists.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_transition_dispatches_tool_continuation() {
        // Given a session with a queued tool continuation.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.enqueue(QueueItem::ToolContinuation);
        }

        // When receiving SessionPhaseChanged → Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Sending,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then SendToLlmProvider was published.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        // And ChatEntrySubmitted was NOT published (tool continuations don't emit it).
        let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
        assert!(submitted.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn non_idle_transition_does_nothing() {
        // Given a session with a queued message.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
        }

        // When receiving SessionPhaseChanged → Sending (not Idle).
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Idle,
            new_phase: PhaseKind::Sending,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then nothing was published.
        assert!(audit.of_type::<SendToLlmProvider>().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_transition_with_empty_queue_does_nothing() {
        // Given a session with nothing queued.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When receiving SessionPhaseChanged → Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Sending,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then nothing was published.
        assert!(audit.of_type::<SendToLlmProvider>().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_emits_chat_entry_submitted() {
        // Given a queue actor.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        let entry = ChatEntry::user("hello");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then ChatEntrySubmitted was published.
        let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
        assert_eq!(submitted.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_sets_title_on_first_message() {
        // Given a session with no title.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        let entry = ChatEntry::user("first message here");

        // When dispatching the first user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then the session title was set to the first line.
        let state = actor.state.read();
        let session = state.session(&sid);
        assert_eq!(session.title(), Some("first message here"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_does_not_overwrite_existing_title() {
        // Given a session with a title already set.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.set_title("original title".to_owned());
        }
        let entry = ChatEntry::user("second message");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then the title is unchanged.
        let state = actor.state.read();
        let session = state.session(&sid);
        assert_eq!(session.title(), Some("original title"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_transitions_to_sending() {
        // Given a queue actor.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        let entry = ChatEntry::user("hello");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then the session is in Sending phase.
        let state = actor.state.read();
        let session = state.session(&sid);
        assert_eq!(session.phase(), PhaseKind::Sending);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_keeps_degraded_token_literal_through_re_expand() {
        // Given a resolved-but-queued entry carrying the degraded marker and a literal token.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        let token = "@/nonexistent/whatever";
        let mut entry = ChatEntry::user(format!("describe {token}"));
        // Simulate the post-resolution state: outcome set, expanded still containing the literal.
        if let crate::protocol::ChatEntryKind::User { outcome, .. } = &mut entry.kind {
            outcome
                .degraded
                .push(crate::feat::session::chat_entry::ResolvedToken {
                    raw: "/nonexistent/whatever".to_owned(),
                    abs: std::path::PathBuf::from("/nonexistent/whatever"),
                });
        }

        // When dispatching (which calls push_entry -> expand_user_entry, re-running the scan).
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then the AI-facing expanded text keeps the literal token (no file:// revert).
        let state = actor.state.read();
        let session = state.session(&sid);
        let expanded = session
            .history()
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                crate::protocol::ChatEntryKind::User { expanded, .. } => Some(expanded.clone()),
                _ => None,
            })
            .expect("user entry");
        assert!(
            expanded.contains(token),
            "queue drain must keep degraded token literal: {expanded}"
        );
        assert!(
            !expanded.contains("file://"),
            "queue drain must not revert to file://: {expanded}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_blocks_attachment_to_unknown_model() {
        // Given a queued entry carrying an image attachment and an unknown model.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.profile_mut().model =
                crate::feat::session::model_selection::ModelSelection::Single(
                    "my-uncatalogued-llama".to_owned(),
                );
        }
        // Build an entry that already carries an attachment (as if resolved).
        let mut entry = ChatEntry::user("describe this");
        if let crate::protocol::ChatEntryKind::User { attachments, .. } = &mut entry.kind {
            attachments.push(jinn_provider::Attachment::image("image/png", vec![1, 2, 3]));
        }

        // When dispatching (queue drain).
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then an Error entry was pushed and no SendToLlmProvider was emitted
        // (the turn is blocked, not re-dispatched).
        let state = actor.state.read();
        let session = state.session(&sid);
        let has_error = session
            .history()
            .iter()
            .any(|e| matches!(&e.kind, crate::protocol::ChatEntryKind::Error(_)));
        assert!(
            has_error,
            "unknown model must block the attachment on drain"
        );
        drop(state);
        assert!(
            audit.of_type::<SendToLlmProvider>().is_empty(),
            "blocked attachment must not dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_provider_id_is_none_when_no_provider() {
        // Given a queue actor with no model selected.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        let entry = ChatEntry::user("hello");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then SendToLlmProvider has provider_id = None.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].provider_id.is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_provider_id_is_some_when_model_set() {
        // Given a queue actor with a model selected.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.profile_mut().model =
                crate::feat::session::model_selection::ModelSelection::Single(
                    "test-provider/test-model".to_owned(),
                );
        }
        let entry = ChatEntry::user("hello");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then SendToLlmProvider has provider_id = Some.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].provider_id.is_some());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_tool_continuation_emits_send_to_llm_provider() {
        // Given a queue actor.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When dispatching a tool continuation.
        actor.dispatch_tool_continuation(&sid).await;

        // Then SendToLlmProvider was published.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        // And the command carries the assembled system prompt (date section
        // is unconditional, so it proves assembly populated the field).
        let prompt = sends[0]
            .system_prompt
            .as_deref()
            .expect("system prompt assembled");
        assert!(
            prompt.contains("Current date:"),
            "dispatch system prompt should come from assembly, got: {prompt:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_tool_continuation_does_not_emit_chat_entry_submitted() {
        // Given a queue actor.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When dispatching a tool continuation.
        actor.dispatch_tool_continuation(&sid).await;

        // Then ChatEntrySubmitted was NOT published.
        let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
        assert!(submitted.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_tool_continuation_does_not_emit_persist_session() {
        // Given a queue actor.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When dispatching a tool continuation.
        actor.dispatch_tool_continuation(&sid).await;

        // Then PersistSession was NOT published.
        let persists: Vec<PersistSession> = audit.of_type::<PersistSession>();
        assert!(persists.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_resume_emits_send_to_llm_provider() {
        // Given a queue actor.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When dispatching a resume.
        actor
            .dispatch_resume(&sid, StreamOrigin::User, "manual resume")
            .await;

        // Then SendToLlmProvider was published.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_drains_steering_buffer() {
        // Given a session with steering fragments.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("system note");
        }
        let entry = ChatEntry::user("hello");

        // When dispatching a user message.
        actor
            .dispatch_user_message(&sid, &entry, StreamOrigin::User)
            .await;

        // Then the steering buffer was drained into history.
        let state = actor.state.read();
        let session = state.session(&sid);
        // At minimum: steering entry + user entry.
        assert!(session.history().len() >= 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_resume_drains_steering_buffer() {
        // Given a session with steering fragments.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("system note");
        }

        // When dispatching a resume.
        actor
            .dispatch_resume(&sid, StreamOrigin::User, "resume")
            .await;

        // Then the steering buffer was drained into history.
        let state = actor.state.read();
        let session = state.session(&sid);
        assert!(!session.history().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_empty_queue_and_steering_dispatches_steering() {
        // Given a session with a steering fragment and an empty queue.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("stay focused");
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then SendToLlmProvider was published.
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1);
        // And the drained steering entry is in history.
        let state = actor.state.read();
        let session = state.session(&sid);
        let has_steering = session.history().iter().any(|e| {
            matches!(
                &e.kind,
                crate::protocol::ChatEntryKind::User { expanded, .. } if expanded == "stay focused"
            )
        });
        assert!(
            has_steering,
            "drained steering entry must appear in history; history = {:?}",
            session.history()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_empty_queue_and_steering_clears_buffer() {
        // Given a session with a steering fragment and an empty queue.
        let (actor, _audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("stay focused");
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then the steering buffer was drained (now empty).
        let state = actor.state.read();
        let session = state.session(&sid);
        assert!(
            session.steering_buffer().is_empty(),
            "steering buffer must be empty after Idle dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_empty_queue_and_steering_emits_chat_entry_submitted() {
        // Given a session with a steering fragment and an empty queue.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("stay focused");
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then ChatEntrySubmitted was published (same as a queued user message).
        let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
        assert_eq!(submitted.len(), 1,);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_empty_queue_and_steering_emits_persist_session() {
        // Given a session with a steering fragment and an empty queue.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.steering_buffer_mut().push_fragment("stay focused");
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then PersistSession was published (same as a queued user message).
        let persists: Vec<PersistSession> = audit.of_type::<PersistSession>();
        assert_eq!(
            persists.len(),
            1,
            "steering dispatch must emit PersistSession"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_empty_queue_and_empty_steering_does_nothing() {
        // Given a session with nothing queued and no steering fragment.
        let (actor, audit) = create_actor().await;
        let sid = session_id();

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then nothing was published.
        assert!(
            audit.of_type::<SendToLlmProvider>().is_empty(),
            "empty queue and steering must not dispatch"
        );
        assert!(
            audit.of_type::<ChatEntrySubmitted>().is_empty(),
            "empty queue and steering must not emit ChatEntrySubmitted"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn idle_with_queued_item_and_steering_dispatches_queue_item_first() {
        // Given a session with BOTH a queued user message and a steering fragment.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user(
                "queued msg",
            ))));
            session.steering_buffer_mut().push_fragment("stay focused");
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then the queue item won dispatch (SendToLlmProvider x1).
        let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
        assert_eq!(sends.len(), 1, "queue item must win dispatch");

        // And the queue is now empty.
        let state = actor.state.read();
        let session = state.session(&sid);
        assert!(
            session.queue().is_empty(),
            "queue must be drained after dispatch"
        );
        // And the steering fragment was co-injected (buffer empty).
        assert!(
            session.steering_buffer().is_empty(),
            "steering must be co-injected, not orphaned"
        );
        // And the steering text appears in history (co-injected into the same turn).
        let history_text: String = session
            .history()
            .iter()
            .map(ChatEntry::text)
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            history_text.contains("stay focused"),
            "steering fragment must appear in history, got: {history_text}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_user_message_emits_phase_changed_idle_to_sending() {
        // Given a session in Idle with a queued user message.
        let (actor, audit) = create_actor().await;
        let sid = session_id();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.session_mut_or_create(&sid);
            session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
        }

        // When receiving SessionPhaseChanged -> Idle.
        let msg = SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
        };
        actor.handle_session_phase_changed(&msg).await;

        // Then the Idle -> Sending transition was published (not just mutated silently).
        let phases: Vec<SessionPhaseChanged> = audit.of_type::<SessionPhaseChanged>();
        let sending = phases
            .iter()
            .find(|p| p.old_phase == PhaseKind::Idle && p.new_phase == PhaseKind::Sending);
        assert!(
            sending.is_some(),
            "Idle -> Sending transition must be published, got: {phases:?}"
        );
    }
}
