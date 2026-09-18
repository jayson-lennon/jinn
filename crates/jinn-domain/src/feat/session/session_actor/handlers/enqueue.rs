//! Message enqueuing handlers - manage user message input, queuing, and dispatch.
//!
//! Handles the flow from user input through to dispatch: enqueuing messages
//! (with queueing when session is busy), updating the input buffer, pushing
//! arbitrary chat entries, and the legacy `SendMessage` compatibility shim.
//!
//! The dispatch itself lives in the turn-dispatch slice: after this actor
//! has expanded templates, resolved image attachments, run the vision gate,
//! and pushed entries / mutated the phase, it publishes
//! [`DispatchTurn`](jinn_turn_dispatch_msg::DispatchTurn) and the queue
//! actor (trouper) assembles the prompt and publishes
//! `SendToLlmProvider`.

use crate::common::actor_deps::BusPublish;
use crate::feat::chat_input::protocol::command::{
    EnqueueResumeTurn, EnqueueUserMessage, PushChatEntry, SubmitSteeringMessage,
};
use crate::feat::chat_input::protocol::event::ChatEntrySubmitted;
use crate::feat::provider::protocol::command::SendMessage;
use crate::protocol::{ChatEntry, ChatEntryKind};

use super::super::SessionPersistenceActor;
use super::image_resolve::ResolveOutcome;
use crate::feat::context::prompt_template::PendingPath;
use crate::feat::session::phase_machine::PhaseKind;
use jinn_turn_dispatch_msg::DispatchTurn;

/// Decision returned after inspecting session state in `EnqueueUserMessage`.
enum EnqueueAction {
    /// Session is idle - the entry is pushed and the turn is dispatched
    /// via the turn-dispatch slice.
    DispatchDirectly,
    /// Session is busy - message was queued.
    Queued,
}

impl SessionPersistenceActor {
    /// EnqueueUserMessage: if idle → assemble prompt; if busy → queue.
    #[expect(clippy::too_many_lines, reason = "1 line over limit")]
    pub(in crate::feat::session::session_actor) async fn handle_enqueue_user_message(
        &self,
        payload: &EnqueueUserMessage,
    ) {
        // Expand `#token` templates and `@/abs/path` image references on the
        // entry before any dispatch logic. This must happen *before* the vision
        // capability gate so the gate sees real attachments — `@path` tokens are
        // raw text in `payload.entry` until expanded. Expansion is idempotent;
        // `push_entry` re-runs it harmlessly.
        let mut entry = payload.entry.clone();
        let pending_paths = self.expand_user_entry(&payload.session_id, &mut entry);

        // Resolve `@path` image attachments: read bytes off the async runtime
        // (`spawn_blocking`), classify each as native / needs-conversion /
        // not-an-image, and fill `entry.kind.attachments`. Non-native images
        // are transcoded to PNG via ImageMagick; any failure produces a
        // visible `Error` entry and aborts dispatch. This runs *before* the
        // vision-capability gate so the gate sees real attachments.
        if !self
            .resolve_image_attachments(&payload.session_id, pending_paths, &mut entry)
            .await
        {
            return;
        }

        // Vision-capability gate (Idle dispatch path only). Block image
        // attachments on models known to lack image support before the entry is
        // pushed or the phase is mutated. Unknown models are allowed through.
        if self
            .attachment_gate_blocks(&payload.session_id, &entry)
            .await
        {
            return;
        }

        let action = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(&payload.session_id);
                match session.phase() {
                    PhaseKind::Idle => {
                        // Normal path: set title, push entry, begin_sending.
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
                        session.begin_sending();
                        EnqueueAction::DispatchDirectly
                    }
                    PhaseKind::Sending | PhaseKind::Streaming => {
                        session.enqueue(crate::feat::session::queue_item::QueueItem::UserMessage(
                            Box::new(entry.clone()),
                        ));
                        EnqueueAction::Queued
                    }
                }
            })
        };

        match action {
            EnqueueAction::DispatchDirectly => {
                super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

                self.publish(ChatEntrySubmitted {
                    session_id: payload.session_id.clone(),
                    entry: payload.entry.clone(),
                })
                .await;

                self.save_active_session(&payload.session_id).await;

                // Hand the prepared turn to the turn-dispatch slice: it
                // drains steering, normalizes loop layout, assembles, and
                // publishes SendToLlmProvider. The Idle→Sending transition
                // event is published there (from its own push/begin_sending
                // write); nothing to emit here.
                self.publish(DispatchTurn {
                    session_id: payload.session_id.clone(),
                })
                .await;
            }
            EnqueueAction::Queued => {}
        }
    }

    /// Checks whether a user entry's image attachments are blocked by the active
    /// model's capabilities. When blocked, pushes the user entry plus an
    /// explanatory `Error` entry, emits `HistoryAppended`, persists, and returns
    /// `true` so the caller skips its normal dispatch path.
    ///
    /// Runs only on the `Idle` dispatch path. Returns `false` (no-op) when the
    /// entry carries no attachments, the model is vision-capable, or the model is
    /// unknown to the reference data.
    async fn attachment_gate_blocks(
        &self,
        session_id: &crate::SessionId,
        entry: &ChatEntry,
    ) -> bool {
        let is_idle = {
            let guard = self.state.read();
            match guard.session.get(session_id) {
                Some(s) => matches!(s.phase(), PhaseKind::Idle),
                // Absent session is created fresh (Idle) by get_or_create below.
                None => true,
            }
        };
        if !is_idle {
            return false;
        }

        let Some(error_entry) = super::multimodal_gate::evaluate_attachment_gate(
            &self.services,
            &self.state,
            session_id,
            entry,
        ) else {
            return false;
        };

        // Blocked: push the user entry and the error entry, then persist.
        // The session stays Idle — no phase transition, no dispatch.
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(session_id);
            session.push_entry(entry.clone());
            session.push_entry(error_entry);
        });
        super::super::helpers::emit_history_appended(self.bus(), session_id).await;
        self.save_active_session(session_id).await;
        true
    }

    /// Expands `#token` templates and `@/abs/path` image references on `entry`
    /// using the session's discovered prompt templates.
    ///
    /// If the session does not yet exist, expansion uses an empty template
    /// store (so `@path` scanning still runs, but `#token` lookup finds nothing).
    fn expand_user_entry(
        &self,
        session_id: &crate::SessionId,
        entry: &mut ChatEntry,
    ) -> Vec<PendingPath> {
        use crate::feat::context::prompt_template::PathResolveContext;
        use crate::feat::session::chat_session::expand_user_entry as expand;
        let (store, cwd) = {
            let guard = self.state.read();
            guard
                .session
                .get(session_id)
                .map(|s| {
                    (
                        s.discovered_prompt_templates().clone(),
                        s.cwd().to_path_buf(),
                    )
                })
                .unwrap_or_default()
        };
        let home = self.services.paths.home_dir().to_path_buf();
        let ctx = PathResolveContext::new(&cwd, &home);
        expand(entry, &store, &ctx)
    }

    /// Reads, classifies, and (if needed) converts `@path` image attachments
    /// off the async runtime, filling `entry.kind.attachments`.
    ///
    /// Returns `true` when all paths resolved successfully (or there were
    /// none), and `false` when a failure produced a visible `Error` entry and
    /// the caller must abort dispatch.
    ///
    /// The blocking file read + classification + ImageMagick spawn all run
    /// inside `spawn_blocking` so the async runtime is never stalled by a
    /// slow disk or a slow conversion.
    async fn resolve_image_attachments(
        &self,
        session_id: &crate::SessionId,
        pending_paths: Vec<PendingPath>,
        entry: &mut ChatEntry,
    ) -> bool {
        if pending_paths.is_empty() {
            return true;
        }
        let converter = self.image_converter.clone();
        let result = tokio::task::spawn_blocking(move || {
            super::image_resolve::resolve_attachments_blocking(&pending_paths, &converter)
        })
        .await;
        match result {
            // Spawn panicked / cancelled — surface a generic error.
            Err(join_err) => {
                self.push_entry_and_block(
                    session_id,
                    entry.clone(),
                    format!("Could not attach image: background task failed: {join_err}"),
                )
                .await;
                false
            }
            Ok(Ok(outcome)) => {
                let ResolveOutcome {
                    attachments,
                    attached,
                    degraded,
                } = outcome;
                if let ChatEntryKind::User {
                    attachments: entry_attachments,
                    outcome: entry_outcome,
                    ..
                } = &mut entry.kind
                {
                    *entry_attachments = attachments;
                    // Record both outcome sets so re-expansion keeps degraded
                    // tokens literal and the render can color attached vs
                    // degraded `@path` tokens. Set unconditionally — an empty
                    // (but non-default) marker keeps re-expansion idempotent for
                    // fully-attached messages.
                    *entry_outcome =
                        crate::feat::session::chat_entry::AttachmentOutcome { attached, degraded };
                }
                true
            }
            Ok(Err(report)) => {
                let message = super::image_resolve::format_attachment_error(&report);
                self.push_entry_and_block(session_id, entry.clone(), message)
                    .await;
                false
            }
        }
    }

    /// Pushes `user_entry` then an `Error` entry carrying `message`, emits
    /// `HistoryAppended`, persists, and leaves the session `Idle`. Mirrors the
    /// vision-capability gate's blocking path.
    async fn push_entry_and_block(
        &self,
        session_id: &crate::SessionId,
        user_entry: ChatEntry,
        message: String,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(session_id);
            session.push_entry(user_entry);
            session.push_entry(ChatEntry::error(message));
        });
        super::super::helpers::emit_history_appended(self.bus(), session_id).await;
        self.save_active_session(session_id).await;
    }

    /// EnqueueResumeTurn: re-send current history without adding a new user entry.
    ///
    /// - If the session is `Idle`, push a UI-only `System` "↻ session resumed"
    ///   marker, transition Idle → Sending, and hand the prepared turn to the
    ///   turn-dispatch slice. Adds no `User` entry.
    /// - If the session is busy (`Sending`/`Streaming`), silently ignored. We do
    ///   not queue resumes — the existing stream is the source of truth.
    ///
    /// The System marker is excluded from LLM context by default
    /// (see `ChatEntryKind::is_included_by_default`), so only the UI sees it.
    pub(in crate::feat::session::session_actor) async fn handle_enqueue_resume_turn(
        &self,
        payload: &EnqueueResumeTurn,
    ) {
        use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
        use crate::protocol::ChatEntry;

        // Only dispatch from Idle. Busy sessions ignore resume (no queuing).
        let should_dispatch = {
            let state = self.state.read();
            let session = state.session(&payload.session_id);
            matches!(session.phase(), PhaseKind::Idle)
        };
        if !should_dispatch {
            return;
        }

        // Push UI-only resume marker and transition Idle → Sending.
        let marker = ChatEntry::system("\u{21bb} session resumed");
        let (old_phase, new_phase) = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(&payload.session_id);
                session.push_entry(marker.clone());
                let old_phase = session.phase();
                session.begin_sending();
                (old_phase, session.phase())
            })
        };

        if old_phase != new_phase {
            self.publish(SessionPhaseChanged {
                session_id: payload.session_id.clone(),
                old_phase,
                new_phase,
            })
            .await;
        }

        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        self.publish(ChatEntrySubmitted {
            session_id: payload.session_id.clone(),
            entry: marker,
        })
        .await;

        self.save_active_session(&payload.session_id).await;

        // Hand the prepared turn to the turn-dispatch slice: it drains
        // steering, assembles, resolves the model, transitions → Streaming,
        // and publishes SendToLlmProvider.
        self.publish(DispatchTurn {
            session_id: payload.session_id.clone(),
        })
        .await;
    }

    /// SubmitSteeringMessage: append a fragment to the session's steering buffer.
    ///
    /// The buffer is drained into a `User` entry at the next prompt-assembly
    /// boundary. This handler performs no phase check - routing (queue vs steer)
    /// is the responsibility of the chat-input layer.
    pub(in crate::feat::session::session_actor) fn handle_submit_steering_message(
        &self,
        payload: &SubmitSteeringMessage,
    ) {
        let fragment_len = payload.text.len();
        let new_depth = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(&payload.session_id);
                session
                    .steering_buffer_mut()
                    .push_fragment(payload.text.clone());
                session.steering_buffer().len()
            })
        };
        tracing::debug!(
            session_id = %payload.session_id,
            fragment_len,
            new_depth,
            "steering fragment buffered"
        );
    }

    /// PushChatEntry: push entry to session history, emit ChatEntrySubmitted event,
    /// and persist the session to disk.
    pub(in crate::feat::session::session_actor) async fn handle_push_chat_entry(
        &self,
        payload: &PushChatEntry,
    ) {
        tracing::debug!(
            session_id = %payload.session_id,
            kind = %payload.entry.kind_str(),
            preview = %payload.entry.text().chars().take(60).collect::<String>(),
            "handle_push_chat_entry"
        );
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            session.push_entry(payload.entry.clone());
        });

        self.publish(ChatEntrySubmitted {
            session_id: payload.session_id.clone(),
            entry: payload.entry.clone(),
        })
        .await;

        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        self.save_active_session(&payload.session_id).await;
    }

    /// SendMessage: backward compat - emit EnqueueUserMessage.
    pub(in crate::feat::session::session_actor) async fn handle_send_message(
        &self,
        payload: &SendMessage,
    ) {
        self.publish(EnqueueUserMessage {
            session_id: payload.session_id.clone(),
            entry: ChatEntry::user(&payload.text),
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
        clippy::uninlined_format_args,
        reason = "test code"
    )]

    use crate::common::services::BusAudit;
    use crate::feat::chat_input::protocol::command::{
        EnqueueResumeTurn, EnqueueUserMessage, PushChatEntry,
    };
    use crate::feat::provider::protocol::command::SendMessage;
    use crate::feat::session::phase_machine::PhaseKind;
    use crate::protocol::{ChatEntry, ChatEntryKind};
    use jinn_core_types::model_selection::ModelSelection;

    async fn create_actor() -> (
        super::super::super::SessionPersistenceActor,
        crate::common::state::State,
        BusAudit,
    ) {
        let state = crate::common::state::State::new(crate::common::app_state::AppState::default());
        let (actor, audit) = super::super::super::helpers::test_actor_recording().await;
        let actor = super::super::super::SessionPersistenceActor {
            state: state.clone(),
            ..actor
        };
        (actor, state, audit)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_hands_off_for_dispatch_when_idle() {
        // Given an idle session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("hello world"),
            })
            .await;

        // Then the entry is in history and the phase moved to Sending (the
        // turn is prepared; the turn-dispatch slice completes it).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        assert_eq!(session.history().len(), 1);
        assert!(
            matches!(&session.history()[0].kind, ChatEntryKind::User { display, .. } if display == "hello world"),
            "expected user entry in history"
        );

        // And the prepared turn was handed to the turn-dispatch slice.
        let handoffs = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(handoffs.len(), 1, "expected one DispatchTurn handoff");
        assert_eq!(handoffs[0].session_id, session_id);
        // And no dispatch was published kernel-side (the slice owns it).
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "SendToLlmProvider belongs to the turn-dispatch slice"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_sets_title_from_first_message() {
        // Given a new session with no title.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When enqueuing the first user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("My First Question"),
            })
            .await;

        // Then the title is set from the first line of the message.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.title(), Some("My First Question"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_queues_when_busy() {
        // Given a session in Streaming phase (busy).
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let session = guard.active_session_mut();
            session.begin_streaming();
            guard.session.active_session_id().clone()
        };

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("queued msg"),
            })
            .await;

        // Then the message is queued (not dispatched - phase stays Streaming).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Streaming);
        // No history entry because the message was queued, not pushed.
        assert_eq!(session.history().len(), 0);
        // The queue should have the message.
        assert_eq!(session.queue().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_no_provider_sends_none_provider_id() {
        // Given a session with default model (NO_PROVIDER_ID).
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When enqueuing a message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("test"),
            })
            .await;

        // Then the turn was handed off for dispatch (the provider_id
        // resolution itself lives in the turn-dispatch slice's tests).
        let handoffs = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handoffs.len(),
            1,
            "expected one DispatchTurn handoff when idle"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_push_chat_entry_pushes_and_emits() {
        // Given a session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When pushing a chat entry.
        let entry = ChatEntry::user("pushed");
        actor
            .handle_push_chat_entry(&PushChatEntry {
                session_id: session_id.clone(),
                entry: entry.clone(),
            })
            .await;

        // Then the entry is in history.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.history().len(), 1);
        assert!(
            matches!(&session.history()[0].kind, ChatEntryKind::User { display, .. } if display == "pushed"),
            "expected pushed entry"
        );

        // And ChatEntrySubmitted event was emitted.
        assert!(
            audit.contains_name("ChatEntrySubmitted"),
            "expected ChatEntrySubmitted event"
        );

        // And HistoryAppended was emitted.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_send_message_emits_enqueue_user_message() {
        // Given a test context.
        let (actor, _state, audit) = create_actor().await;
        let session_id = crate::protocol::SessionId::new();

        // When calling handle_send_message.
        actor
            .handle_send_message(&SendMessage {
                session_id: session_id.clone(),
                text: "legacy message".to_owned(),
            })
            .await;

        // Then EnqueueUserMessage command was emitted.
        assert!(
            audit.contains_name("EnqueueUserMessage"),
            "expected EnqueueUserMessage command from SendMessage"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn before_turn_no_attachments_dispatches_normally() {
        // Given an idle session with no attachments.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("hello"),
            })
            .await;

        // Then the turn was prepared (history has the entry, phase is Sending).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
    }

    // A minimal PNG (8x8) used by the multimodal enqueue tests below.
    const MULTIMODAL_TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, // 8x8
        0x08, 0x06, 0x00, 0x00, 0x00, // RGBA, no compression
    ];

    /// Writes a models.dev JSON mapping the given model id to vision/text-only,
    /// and sets it as the session's active model.
    fn seed_models_dev(
        actor: &super::super::super::SessionPersistenceActor,
        model_id: &str,
        supports_image: bool,
    ) {
        let path = actor.services.paths.models_dev_user_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create cache dir");
        }
        let input = if supports_image { "image" } else { "text" };
        let json = serde_json::json!({
            "acme": {
                "models": {
                    model_id: {
                        "modalities": { "input": [input] }
                    }
                }
            }
        });
        std::fs::write(&path, json.to_string()).expect("write models.dev.json");
        // Set the session's active model to the seeded model id.
        let mut guard = actor.state.write_test_no_cap();
        guard
            .active_session_mut()
            .set_model(ModelSelection::Single(model_id.to_owned()));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn at_path_image_to_text_only_model_is_blocked_with_error_entry() {
        // Given an idle session whose active model is a known text-only model.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        // Write the image to a temp file and seed the capability table.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png_path = dir.path().join("img.png");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        seed_models_dev(&actor, "text-only-model", false);
        let display = format!("describe this @{}", png_path.to_string_lossy());

        // When enqueuing a user message with an @path image attachment.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then an Error entry appears in history and no SendToLlmProvider was emitted.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert!(
            session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Error(_))),
            "expected an Error entry when an image is sent to a text-only model"
        );
        // And the session stayed Idle (never dispatched).
        assert_eq!(session.phase(), PhaseKind::Idle);
        drop(guard);
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "text-only model must not receive the request"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn at_path_image_to_vision_model_dispatches_with_attachment() {
        // Given an idle session whose active model is vision-capable.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png_path = dir.path().join("photo.png");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        seed_models_dev(&actor, "vision-model", true);
        let display = format!("describe this @{}", png_path.to_string_lossy());

        // When enqueuing a user message with an @path image attachment.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then the turn was prepared for the vision model and handed off
        // for dispatch (the slice completes it).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "vision model turn must be handed off for dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn at_path_image_to_unknown_model_is_blocked_with_error_entry() {
        // Given an idle session whose active model is NOT in models.dev (unknown).
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        // Set a model id but write NO models.dev entry for it.
        {
            let mut guard = state.write_test_no_cap();
            guard
                .active_session_mut()
                .set_model(ModelSelection::Single("my-uncatalogued-llama".to_owned()));
        }
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png_path = dir.path().join("img.png");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        let display = format!("describe this @{}", png_path.to_string_lossy());

        // When enqueuing a user message with an @path image attachment.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then an Error entry appears (unknown models block) and no dispatch.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert!(
            session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Error(_))),
            "expected an Error entry when an image is sent to an unknown model"
        );
        assert_eq!(session.phase(), PhaseKind::Idle);
        drop(guard);
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "unknown model must not receive the request"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn text_only_message_to_unknown_model_dispatches_normally() {
        // Given an idle session whose active model is unknown AND a text-only message.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        {
            let mut guard = state.write_test_no_cap();
            guard
                .active_session_mut()
                .set_model(ModelSelection::Single("my-uncatalogued-llama".to_owned()));
        }

        // When enqueuing a text-only user message (no @path, no attachments).
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("just a plain message"),
            })
            .await;

        // Then it was prepared — the gate only fires for attachments, so a
        // text-only message dispatches even to an unknown model.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "text-only message must hand off for dispatch even to an unknown model"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_resume_turn_noop_when_streaming() {
        // Given a session already in Streaming phase.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let session = guard.active_session_mut();
            session.begin_streaming();
            guard.session.active_session_id().clone()
        };

        // When resume is requested.
        actor
            .handle_enqueue_resume_turn(&EnqueueResumeTurn {
                session_id: session_id.clone(),
            })
            .await;

        // Then no commands were emitted (silent no-op).
        assert!(
            audit.is_empty(),
            "expected no commands when resuming a streaming session"
        );
        // And no System marker was pushed to history.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert!(
            session.history().is_empty(),
            "history should remain empty when resume is ignored"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_resume_turn_idle_dispatches_directly() {
        // Given an idle session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When resume is requested.
        actor
            .handle_enqueue_resume_turn(&EnqueueResumeTurn {
                session_id: session_id.clone(),
            })
            .await;

        // Then the turn was handed off for dispatch (the slice owns the
        // SendToLlmProvider emission on the resume path).
        assert!(
            audit.contains_name("DispatchTurn"),
            "expected a DispatchTurn handoff for resume from Idle"
        );

        // And the session is now in Sending phase (prepared; the slice
        // completes the transition).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            session.phase(),
            PhaseKind::Sending,
            "phase should be Sending after the prepared resume handoff"
        );

        // And no item was queued (we dispatched inline, not via the queue).
        assert!(
            session.queue().is_empty(),
            "resume from Idle should not enqueue; it dispatches inline"
        );

        // And exactly one System marker was pushed to history.
        let markers: Vec<_> = session
            .history()
            .iter()
            .filter(|e| matches!(e.kind, crate::protocol::ChatEntryKind::System { .. }))
            .collect();
        assert_eq!(markers.len(), 1, "expected one System marker pushed");
    }

    // Helper: seed a vision-capable model and return the idle session id.
    async fn idle_vision_session() -> (
        super::super::super::SessionPersistenceActor,
        crate::common::state::State,
        BusAudit,
        crate::protocol::SessionId,
    ) {
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write_test_no_cap();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        seed_models_dev(&actor, "vision-model", true);
        (actor, state, audit, session_id)
    }

    /// Extracts the `expanded` text of the most recent `User` entry in history.
    fn last_user_expanded(
        session: &crate::feat::session::chat_session::ChatSessionState,
    ) -> Option<String> {
        session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { expanded, .. } => Some(expanded.clone()),
            _ => None,
        })
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn nonexistent_at_path_dispatches() {
        // Given an idle vision-model session.
        let (actor, state, audit, session_id) = idle_vision_session().await;

        // When enqueuing a message with a nonexistent @path.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("describe @/nonexistent/whatever"),
            })
            .await;

        // Then the turn was prepared and handed off for dispatch.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "nonexistent @path turn should be handed off"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn nonexistent_at_path_keeps_literal_expanded() {
        // Given an idle vision-model session.
        let (actor, state, _audit, session_id) = idle_vision_session().await;
        let token = "@/nonexistent/whatever";

        // When enqueuing a message with a nonexistent @path.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("describe {token}")),
            })
            .await;

        // Then the AI-facing expanded text keeps the literal token (no file:// rewrite).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains(token),
            "expanded should keep literal token: {expanded}"
        );
        assert!(
            !expanded.contains("file://"),
            "expanded must not contain file://: {expanded}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn existing_non_image_at_path_dispatches() {
        // Given an idle vision-model session and an existing non-image file.
        let (actor, state, audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, b"not an image").expect("write");

        // When enqueuing a message with an @path to a non-image file.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("see @{}", notes.to_string_lossy())),
            })
            .await;

        // Then the turn was prepared and handed off for dispatch.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "non-image @path turn should be handed off"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn existing_non_image_at_path_keeps_literal_expanded() {
        // Given an idle vision-model session and an existing non-image file.
        let (actor, state, _audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, b"not an image").expect("write");
        let token = format!("@{}", notes.to_string_lossy());

        // When enqueuing a message with an @path to a non-image file.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("see {token}")),
            })
            .await;

        // Then the AI-facing expanded text keeps the literal token (no file:// rewrite).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains(&token),
            "expanded should keep literal token: {expanded}"
        );
        assert!(
            !expanded.contains("file://"),
            "expanded must not contain file://: {expanded}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn recognizable_image_without_converter_blocks_with_error() {
        // Given an idle vision-model session and a recognizable HEIC file (test converter is unavailable).
        let (actor, state, audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let heic = dir.path().join("photo.heic");
        let mut bytes = vec![0x00, 0x00, 0x00, 0x18];
        bytes.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&heic, &bytes).expect("write");

        // When enqueuing a message with the HEIC @path.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("see @{}", heic.to_string_lossy())),
            })
            .await;

        // Then an Error entry is pushed and the turn is blocked.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            session.phase(),
            PhaseKind::Idle,
            "conversion failure must block"
        );
        assert!(
            session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Error(_))),
            "expected an Error entry for the conversion failure"
        );
        drop(guard);
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "conversion failure must not dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mixed_native_image_and_nonexistent_token_dispatches_with_one_attachment() {
        // Given an idle vision-model session, a real PNG, and a nonexistent path in one message.
        let (actor, state, audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png = dir.path().join("real.png");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let display = format!("see @{} and @/nonexistent/x", png.to_string_lossy());

        // When enqueuing the mixed message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then it prepares with exactly one attachment and the nonexistent token stays literal.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains("@/nonexistent/x"),
            "nonexistent token must stay literal: {expanded}"
        );
        assert!(
            expanded.contains("file://"),
            "real image token must be rewritten: {expanded}"
        );
        let attachments = session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { attachments, .. } => Some(attachments.len()),
            _ => None,
        });
        assert_eq!(attachments, Some(1), "exactly one attachment expected");
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "mixed message turn should be handed off"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mixed_native_image_and_existing_non_image_dispatches_with_one_attachment() {
        // Given an idle vision-model session, a real PNG, and an existing non-image file.
        let (actor, state, audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png = dir.path().join("real.png");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, b"text").expect("write");
        let display = format!(
            "see @{} and @{}",
            png.to_string_lossy(),
            notes.to_string_lossy()
        );

        // When enqueuing the mixed message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then it prepares with exactly one attachment and the non-image token stays literal.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseKind::Sending);
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains(&format!("@{}", notes.to_string_lossy())),
            "non-image token must stay literal: {expanded}"
        );
        assert!(
            expanded.contains("file://"),
            "real image token must be rewritten: {expanded}"
        );
        let attachments = session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { attachments, .. } => Some(attachments.len()),
            _ => None,
        });
        assert_eq!(attachments, Some(1), "exactly one attachment expected");
        drop(guard);
        assert!(
            audit.contains_name("DispatchTurn"),
            "mixed message turn should be handed off"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn multiple_nonexistent_tokens_all_stay_literal() {
        // Given an idle vision-model session and a message with several nonexistent @paths.
        let (actor, state, _audit, session_id) = idle_vision_session().await;

        // When enqueuing the message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("see @/nope/a and @/nope/b"),
            })
            .await;

        // Then both tokens stay literal and nothing is attached.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains("@/nope/a"),
            "first token literal: {expanded}"
        );
        assert!(
            expanded.contains("@/nope/b"),
            "second token literal: {expanded}"
        );
        assert!(
            !expanded.contains("file://"),
            "no file:// rewrite: {expanded}"
        );
        let attachments = session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { attachments, .. } => Some(attachments.len()),
            _ => None,
        });
        assert_eq!(attachments, Some(0), "no attachments expected");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn multiple_native_images_all_attach() {
        // Given an idle vision-model session and a message with two real PNGs.
        let (actor, state, _audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png_a = dir.path().join("a.png");
        let png_b = dir.path().join("b.png");
        std::fs::write(&png_a, MULTIMODAL_TINY_PNG).expect("write a");
        std::fs::write(&png_b, MULTIMODAL_TINY_PNG).expect("write b");
        let display = format!(
            "see @{} and @{}",
            png_a.to_string_lossy(),
            png_b.to_string_lossy()
        );

        // When enqueuing the message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then both images attach.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let attachments = session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { attachments, .. } => Some(attachments.len()),
            _ => None,
        });
        assert_eq!(attachments, Some(2), "both images should attach");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mixed_native_image_and_conversion_failing_image_blocks() {
        // Given an idle vision-model session, a real PNG, and a recognizable HEIC (converter unavailable).
        let (actor, state, audit, session_id) = idle_vision_session().await;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let png = dir.path().join("real.png");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let heic = dir.path().join("photo.heic");
        let mut bytes = vec![0x00, 0x00, 0x00, 0x18];
        bytes.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&heic, &bytes).expect("write");
        let display = format!(
            "see @{} and @{}",
            png.to_string_lossy(),
            heic.to_string_lossy()
        );

        // When enqueuing the mixed message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(&display),
            })
            .await;

        // Then the conversion failure hard-errors and blocks the whole turn.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            session.phase(),
            PhaseKind::Idle,
            "conversion failure must block the turn"
        );
        drop(guard);
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "conversion failure must not dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn email_at_path_is_not_matched() {
        // Given an idle vision-model session and a message with an email address.
        let (actor, state, _audit, session_id) = idle_vision_session().await;

        // When enqueuing the message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("contact foo@bar.com"),
            })
            .await;

        // Then the email is not treated as a path: text unchanged, no attachments.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let expanded = last_user_expanded(session).expect("user entry");
        assert_eq!(expanded, "contact foo@bar.com");
        let attachments = session.history().iter().rev().find_map(|e| match &e.kind {
            ChatEntryKind::User { attachments, .. } => Some(attachments.len()),
            _ => None,
        });
        assert_eq!(attachments, Some(0), "email must not produce an attachment");
    }
}
