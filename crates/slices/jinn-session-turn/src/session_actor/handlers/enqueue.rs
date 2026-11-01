//! Message enqueuing handlers - manage user message input, queuing, and dispatch.
//!
//! Handles the flow from user input through to dispatch: enqueuing messages
//! (with queueing when session is busy), updating the input buffer, pushing
//! arbitrary chat entries.
//!
//! The dispatch itself lives in the turn-dispatch slice: after this actor
//! has expanded templates, resolved image attachments, run the vision gate,
//! and pushed entries / mutated the phase, it publishes
//! [`DispatchTurn`](jinn_turn_dispatch_msg::DispatchTurn) and the queue
//! actor (trouper) assembles the prompt and publishes
//! `SendToLlmProvider`.

use jinn_chat_input_msg::{
    ChatEntrySubmitted, EnqueueResumeTurn, EnqueueUserMessage, SubmitSteeringMessage,
};
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::protocol::{ChatEntry, ChatEntryKind};
use jinn_session_history_msg::PushChatEntry;

use super::super::SessionPersistenceActor;
use jinn_context::PromptTemplateStore;
use jinn_context::attachment_path::{PathResolveContext, PendingPath};
use jinn_llm_support::image_convert::ResolveOutcome;
use jinn_slices::PhaseView;
use jinn_turn_dispatch_msg::DispatchTurn;

/// Decision returned after inspecting session state in `EnqueueUserMessage`.
enum EnqueueAction {
    /// Session is idle - the entry is pushed and the turn is dispatched
    /// via the turn-dispatch slice, behind a phase-admission ask.
    DispatchDirectly,
    /// Session is busy - message was queued.
    Queued,
}

/// Renders the drained canvas metric events as the flat numbered event
/// list prepended to the user's message:
///
/// ```text
/// <canvas-metrics artifact="quiz.html">
/// 1. clicked-foo
/// 2. flavored-click {"flavor":"vanilla"}
/// </canvas-metrics>
/// ```
///
/// One line per event — the page's own event name, plus its props as
/// compact JSON when the event carries a non-empty props object; bare
/// name otherwise. The tag carries the artifact basename once — the
/// newest event's, since a drain spanning a canvas re-target is rare.
/// Receive stamps stay on the wire ([`CanvasMetricEvent`]) but are not
/// rendered; the numbered order already says which click came later.
///
/// The caller only drains non-empty buffers, so the tag always names a
/// real artifact.
///
/// The block rides `display`, the only text `push_entry`'s
/// re-expansion preserves (it rebuilds `expanded` from `display`), so
/// the block is visible in the transcript as part of the user's own
/// message — the approved shape. On push the block text passes back
/// through template/`@path` scanning: only path-shaped or template-name
/// collisions inside a metric value could be rewritten, and template
/// names and absolute paths in click props are corner cases.
///
/// The lines are plain text (the block's XML delimiters carry the
/// structure; the lines carry the data), so an agent reads the whole
/// sequence at a glance without escaping concerns.
fn render_canvas_metrics_block(events: &[jinn_canvas_msg::CanvasMetricEvent]) -> String {
    let artifact = render_artifact_tag(events);
    let lines: Vec<String> = events
        .iter()
        .enumerate()
        .map(|(index, event)| render_metric_line(index + 1, event))
        .collect();
    format!(
        "<canvas-metrics artifact=\"{artifact}\">\n{}\n</canvas-metrics>",
        lines.join("\n")
    )
}

/// The artifact basename the block tag names: the newest event's
/// artifact. Falls back to the full string when the path carries no
/// file name (`file_name()` is `None` on empty and odd strings).
fn render_artifact_tag(events: &[jinn_canvas_msg::CanvasMetricEvent]) -> String {
    events
        .last()
        .map(|event| {
            std::path::Path::new(&event.artifact)
                .file_name()
                .map_or(event.artifact.clone(), |name| {
                    name.to_string_lossy().into_owned()
                })
        })
        .unwrap_or_default()
}

/// One numbered line, oldest first: `N. name`, plus the event's props
/// as compact JSON when it carries a non-empty props object. A missing,
/// `null`, non-object, or empty props renders as a bare name — the
/// `/metrics` endpoint is permissive, so the renderer tolerates any
/// payload shape (an unnamed event renders as `unnamed`).
fn render_metric_line(number: usize, event: &jinn_canvas_msg::CanvasMetricEvent) -> String {
    let name = event
        .event
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unnamed");
    match event.event.get("props") {
        Some(props) if props.as_object().is_some_and(|props| !props.is_empty()) => {
            let props_json = serde_json::to_string(props).unwrap_or_default();
            format!("{number}. {name} {props_json}")
        }
        _ => format!("{number}. {name}"),
    }
}

/// The first line of `display` with a leading canvas-metrics block
/// skipped — the line the session title mints from. Without the skip, a
/// first-ever message carrying metrics would title the session after
/// the XML tag instead of the user's words.
fn title_source_line(display: &str) -> &str {
    let rest = display
        .split_once("\n</canvas-metrics>\n")
        .map_or(display, |(_, after_block)| after_block);
    rest.lines().next().unwrap_or("")
}

impl SessionPersistenceActor {
    /// EnqueueUserMessage: if idle → assemble prompt; if busy → queue.
    pub(in crate::session_actor) async fn handle_enqueue_user_message(
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

        // Canvas metrics ride the user's message: whatever the served
        // page reported since the last send is drained and prepended as
        // an XML block, after the gates (a gate-aborted message must not
        // consume the buffer) and before the queue/dispatch decision, so
        // both branches carry the block. Template expansion has already
        // run, so a metric value like `#foo` can never be read as a
        // template token.
        if matches!(entry.kind, ChatEntryKind::User { .. }) {
            self.prepend_canvas_metrics(&mut entry).await;
        }

        let action = {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(&payload.session_id);
                match session.phase() {
                    PhaseView::Idle => EnqueueAction::DispatchDirectly,
                    PhaseView::Streaming | PhaseView::Working | PhaseView::Cancelling => {
                        session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                            entry.clone(),
                        )));
                        EnqueueAction::Queued
                    }
                }
            })
        };

        match action {
            EnqueueAction::DispatchDirectly => {
                // Admission before the entry lands: a user message mints a
                // fresh turn (`FreshTurn`), so the only refusal is a cancel
                // drain in progress — in which case history stays untouched
                // and the message is silently dropped with an error log.
                let Some(_) = jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
                    &self.services,
                    &self.state,
                    &payload.session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                    jiff::Timestamp::now(),
                )
                .await
                else {
                    tracing::error!(
                        session_id = %payload.session_id,
                        "user message dropped: the ledger refused a fresh mint"
                    );
                    return;
                };

                {
                    self.state.with_session(|view| {
                        let session = view.session.map().get_or_create(&payload.session_id);
                        // Title minting is keyed to genuine user entries: an
                        // inter-session delivery (or any other kind) leaves an
                        // untitled session untitled, so a real user message
                        // can still mint the title later. Stamping `""` here
                        // would flip the guard to `Some("")` and block every
                        // future mint — the sidebar would read an empty title.
                        // The minted line skips a leading canvas-metrics
                        // block, so a metrics-carrying first message titles
                        // from the user's words, not the XML tag.
                        if session.title().is_none()
                            && let ChatEntryKind::User { display, .. } = &entry.kind
                        {
                            let title = title_source_line(display).to_owned();
                            session.set_title(title);
                        }
                        session.push_entry(entry.clone());
                    });
                }
                super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

                self.publish(ChatEntrySubmitted {
                    session_id: payload.session_id.clone(),
                    entry: payload.entry.clone(),
                })
                .await;

                self.save_active_session(&payload.session_id).await;

                // Hand the prepared turn to the turn-dispatch slice: it
                // drains steering, normalizes loop layout, assembles, and
                // publishes `SendToLlmProvider`. The turn was already
                // admitted and minted above; the dispatch's own admission
                // joins it, and this path publishes no phase event of its
                // own.
                self.publish(DispatchTurn {
                    session_id: payload.session_id.clone(),
                })
                .await;
            }
            EnqueueAction::Queued => {}
        }
    }

    /// Drains the canvas actor's accumulated metric events and prepends
    /// them to `entry`'s display text as a `<canvas-metrics>` block —
    /// the shape the `<system-interrupt>` precedent uses.
    ///
    /// `display` because it is the only text `push_entry`'s
    /// re-expansion preserves (it rebuilds `expanded` from `display`);
    /// the block therefore shows in the transcript as part of the
    /// user's own message — the approved shape, and the title mint
    /// skips it (see [`title_source_line`]). A message with no pending
    /// activity — no handle, a transport error, or an empty drain — is
    /// left byte-for-byte unchanged; the user's words go out either way.
    async fn prepend_canvas_metrics(&self, entry: &mut ChatEntry) {
        let Some(handle) = self.services.canvas_server.get().cloned() else {
            return;
        };
        let outcome = match handle.drain_metrics().await {
            Ok(outcome) if !outcome.events.is_empty() => outcome,
            Ok(_) => return,
            Err(error) => {
                tracing::debug!(
                    ?error,
                    "canvas metrics drain failed; message goes out without a block"
                );
                return;
            }
        };
        let block = render_canvas_metrics_block(&outcome.events);
        if let ChatEntryKind::User {
            display, expanded, ..
        } = &mut entry.kind
        {
            // Both texts: `display` is what the push's re-expansion
            // preserves (it rebuilds `expanded` from `display`), and
            // syncing `expanded` keeps an already-expanded entry — the
            // queued branch stores it verbatim — coherent in the same
            // instant rather than only after the queue's re-expansion.
            *display = format!("{block}\n{display}");
            *expanded = format!("{block}\n{expanded}");
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
        session_id: &jinn_core_types::SessionId,
        entry: &ChatEntry,
    ) -> bool {
        let is_idle = {
            let guard = self.state.read();
            match guard.session.get(session_id) {
                Some(s) => s.phase() == PhaseView::Idle,
                // Absent session is created fresh (Idle) by get_or_create below.
                None => true,
            }
        };
        if !is_idle {
            return false;
        }

        let Some(error_entry) = jinn_provider_selection::attachment_gate::evaluate_attachment_gate(
            &self.services,
            &self.state,
            session_id,
            entry,
        ) else {
            return false;
        };

        // Blocked: push the user entry and the error entry, then persist.
        // The session stays Idle — no phase transition, no dispatch.
        self.state.with_session(|view| {
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
        session_id: &jinn_core_types::SessionId,
        entry: &mut ChatEntry,
    ) -> Vec<PendingPath> {
        use jinn_session_state::chat_session::expand_user_entry as expand;
        let (store, cwd) = {
            let guard = self.state.read();
            guard.session.get(session_id).map_or_else(
                || {
                    (
                        PromptTemplateStore::default(),
                        jinn_session_state::fields::default_cwd(),
                    )
                },
                |s| (s.discovered_prompt_templates().clone(), s.cwd().clone()),
            )
        };
        let home = self.services.paths.home_dir().clone();
        let (cwd_view, home_view) = (cwd.as_marked_path(), home.as_marked_path());
        let ctx = PathResolveContext::new(&cwd_view, &home_view);
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
        session_id: &jinn_core_types::SessionId,
        pending_paths: Vec<PendingPath>,
        entry: &mut ChatEntry,
    ) -> bool {
        if pending_paths.is_empty() {
            return true;
        }
        let converter = self.image_converter.clone();
        let result = tokio::task::spawn_blocking(move || {
            jinn_llm_support::image_convert::resolve_attachments_blocking(
                &pending_paths,
                &converter,
            )
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
                        jinn_kernel::protocol::AttachmentOutcome { attached, degraded };
                }
                true
            }
            Ok(Err(report)) => {
                let message = jinn_llm_support::image_convert::format_attachment_error(&report);
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
        session_id: &jinn_core_types::SessionId,
        user_entry: ChatEntry,
        message: String,
    ) {
        self.state.with_session(|view| {
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
    ///   marker, ask the phase actor to mint a fresh generation, and hand the
    ///   prepared turn to the turn-dispatch slice. Adds no `User` entry.
    /// - If the session is busy (`Sending`/`Streaming`), silently ignored. We do
    ///   not queue resumes — the existing stream is the source of truth.
    ///
    /// The System marker is excluded from LLM context by default
    /// (see `ChatEntryKind::is_included_by_default`), so only the UI sees it.
    pub(in crate::session_actor) async fn handle_enqueue_resume_turn(
        &self,
        payload: &EnqueueResumeTurn,
    ) {
        use jinn_core_types::ChatEntry;

        // Only dispatch from Idle. Busy sessions ignore resume (no queuing).
        let should_dispatch = {
            let state = self.state.read();
            let session = state.session(&payload.session_id);
            session.phase() == jinn_slices::PhaseView::Idle
        };
        if !should_dispatch {
            return;
        }

        // Admission first: a resume is a fresh turn (a turn the cancel
        // killed stays dead), so `FreshTurn`. Refused → the marker never
        // lands and nothing dispatches.
        let admitted = jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &self.services,
            &self.state,
            &payload.session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await
        .is_some();
        if !admitted {
            tracing::error!(
                session_id = %payload.session_id,
                "resume dropped: the ledger refused a fresh mint"
            );
            return;
        }

        // Push UI-only resume marker.
        let marker = ChatEntry::system("\u{21bb} session resumed");
        self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&payload.session_id)
                .push_entry(marker.clone());
        });

        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        self.publish(ChatEntrySubmitted {
            session_id: payload.session_id.clone(),
            entry: marker,
        })
        .await;

        self.save_active_session(&payload.session_id).await;

        // Hand the prepared turn to the turn-dispatch slice: it admits the
        // stream with the phase actor, drains steering, assembles, resolves
        // the model, and publishes `SendToLlmProvider`.
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
    pub(in crate::session_actor) fn handle_submit_steering_message(
        &self,
        payload: &SubmitSteeringMessage,
    ) {
        let fragment_len = payload.text.len();
        let new_depth = {
            self.state.with_session(|view| {
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
    pub(in crate::session_actor) async fn handle_push_chat_entry(&self, payload: &PushChatEntry) {
        tracing::debug!(
            session_id = %payload.session_id,
            kind = %payload.entry.kind_str(),
            preview = %payload.entry.text().chars().take(60).collect::<String>(),
            "handle_push_chat_entry"
        );
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            let entry_id = payload.entry.id.clone();
            session.push_entry(payload.entry.clone());
            // The pin rides along with the push, so it can never race ahead
            // of the entry it names.
            if let Some(position) = payload.pin {
                session.pin_entry(&entry_id, position);
            }
        });

        self.publish(ChatEntrySubmitted {
            session_id: payload.session_id.clone(),
            entry: payload.entry.clone(),
        })
        .await;

        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        self.save_active_session(&payload.session_id).await;
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

    use jinn_canvas_msg::{CanvasMetricEvent, CanvasMetricsDrainOutcome};

    use super::render_canvas_metrics_block;
    use std::sync::Arc;

    /// A `CanvasServerHandle` whose drain always returns the events it
    /// was seeded with — the stub the enqueue tests install into
    /// `Services.canvas_server`.
    struct SeededCanvasServer {
        events: Vec<CanvasMetricEvent>,
    }

    #[async_trait::async_trait]
    impl jinn_canvas_msg::CanvasServerHandle for SeededCanvasServer {
        async fn serve(
            &self,
            _path: String,
        ) -> Result<jinn_canvas_msg::CanvasServeOutcome, jinn_canvas_msg::CanvasServeTransportError>
        {
            Err(jinn_canvas_msg::CanvasServeTransportError::Mailbox)
        }

        async fn drain_metrics(
            &self,
        ) -> Result<CanvasMetricsDrainOutcome, jinn_canvas_msg::CanvasServeTransportError> {
            Ok(CanvasMetricsDrainOutcome {
                events: self.events.clone(),
            })
        }
    }

    /// Installs `handle` into the actor's `Services.canvas_server` slot.
    fn install_canvas_server(
        actor: &super::super::super::SessionPersistenceActor,
        events: Vec<CanvasMetricEvent>,
    ) {
        let _ = actor
            .services
            .canvas_server
            .set(Arc::new(SeededCanvasServer { events }));
    }

    /// One canonical drained event.
    fn metric_event(name: &str, choice: &str) -> CanvasMetricEvent {
        metric_event_with_artifact("/tmp/quiz.html", name, choice)
    }

    /// The same canonical event on a different artifact, for drains
    /// spanning a canvas re-target.
    fn metric_event_with_artifact(artifact: &str, name: &str, choice: &str) -> CanvasMetricEvent {
        CanvasMetricEvent {
            event: serde_json::json!({
                "name": name,
                "at": 1_730_000_000_000_i64,
                "props": { "choice": choice }
            }),
            received_at: "2026-11-01T12:00:00Z".to_owned(),
            artifact: artifact.to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn metrics_block_is_a_numbered_flat_event_list() {
        // Given two drained events — one carrying props, one bare.
        let events = vec![
            metric_event("quiz.start", "a"),
            CanvasMetricEvent {
                event: serde_json::json!({ "name": "quiz.answer" }),
                received_at: "2026-11-01T12:00:00Z".to_owned(),
                artifact: "/tmp/quiz.html".to_owned(),
            },
        ];

        // When rendering the block.
        let block = render_canvas_metrics_block(&events);

        // Then the block is the approved flat list: the artifact
        // basename once on the tag, one numbered `N. name {props}` line
        // per event with a bare name when props are absent, and no
        // count, receive stamps, or JSON wrapper objects.
        assert_eq!(
            block,
            concat!(
                "<canvas-metrics artifact=\"quiz.html\">\n",
                "1. quiz.start {\"choice\":\"a\"}\n",
                "2. quiz.answer\n",
                "</canvas-metrics>"
            )
        );
    }

    #[rstest::rstest]
    #[test]
    fn mixed_artifact_drain_tags_the_newest_artifact() {
        // Given a drain spanning two artifacts — the canvas was
        // re-targeted between the two clicks.
        let events = vec![
            metric_event("quiz.start", "a"),
            metric_event_with_artifact("/tmp/other.html", "banner.click", "a"),
        ];

        // When rendering the block.
        let block = render_canvas_metrics_block(&events);

        // Then the tag names the newest event's artifact, basenamed.
        assert!(
            block.starts_with("<canvas-metrics artifact=\"other.html\">"),
            "{block}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enqueue_prepends_metrics_block_to_the_user_message() {
        // Given an idle session whose canvas server holds two events.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        install_canvas_server(
            &actor,
            vec![
                metric_event("quiz.start", "a"),
                metric_event("quiz.answer", "b"),
            ],
        );

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("how did I do on the quiz"),
            })
            .await;

        // Then the history entry leads with the metrics block and the
        // user's own words follow intact — display is the only text the
        // push's re-expansion preserves, and it feeds the expanded text
        // the model reads.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.starts_with("<canvas-metrics artifact=\"quiz.html\">"),
            "{expanded}"
        );
        assert!(expanded.contains("quiz.answer"), "{expanded}");
        assert!(expanded.ends_with("how did I do on the quiz"), "{expanded}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_metrics_carrying_first_message_titles_from_the_users_words() {
        // Given a fresh untitled session whose canvas server holds an event.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        install_canvas_server(&actor, vec![metric_event("quiz.answer", "b")]);

        // When the session's first-ever message carries the block.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("what did I click"),
            })
            .await;

        // Then the title mints from the user's words, not the XML tag.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.title(), Some("what did I click"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enqueue_with_no_canvas_handle_leaves_the_message_untouched() {
        // Given an idle session with no canvas handle installed (the
        // dormant composition, or a jinn without the canvas slice).
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("plain words"),
            })
            .await;

        // Then the expanded text is the message alone — no empty block
        // ever appears.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(last_user_expanded(session).as_deref(), Some("plain words"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enqueue_with_a_quiet_canvas_leaves_the_message_untouched() {
        // Given an idle session whose canvas server has drained nothing.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        install_canvas_server(&actor, Vec::new());

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("no canvas activity"),
            })
            .await;

        // Then the expanded text is the message alone.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            last_user_expanded(session).as_deref(),
            Some("no canvas activity")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enqueue_prepends_metrics_block_to_a_queued_message_too() {
        // Given a busy session (Streaming) whose canvas server holds an event.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let session = guard.active_session_mut();
            session.begin_streaming();
            guard.session.active_session_id().clone()
        };
        install_canvas_server(&actor, vec![metric_event("canvas.scroll", "down")]);

        // When enqueuing a user message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("while you were streaming"),
            })
            .await;

        // Then the message queued carrying the block: the drain happened
        // before the queue/dispatch decision, so the queued branch gets
        // the metrics the same way the dispatch branch does.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseView::Streaming);
        assert_eq!(session.message_queue().len(), 1);
        let Some(jinn_turn_dispatch_msg::QueueItem::UserMessage(entry)) =
            session.message_queue().items().front()
        else {
            panic!("expected a queued user message");
        };
        let jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } = &entry.kind else {
            panic!("expected a user entry, got {:?}", entry.kind);
        };
        assert!(
            expanded.starts_with("<canvas-metrics artifact=\"quiz.html\">"),
            "{expanded}"
        );
        assert!(expanded.ends_with("while you were streaming"), "{expanded}");
    }

    use jinn_chat_input_msg::{EnqueueResumeTurn, EnqueueUserMessage};
    use jinn_core_types::SessionId;
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_kernel::common::services::BusAudit;
    use jinn_kernel::protocol::{ChatEntry, ChatEntryKind};
    use jinn_session_history_msg::PushChatEntry;
    use jinn_slices::PhaseView;
    use marked_path::MarkedPathAccess;

    async fn create_actor() -> (
        super::super::super::SessionPersistenceActor,
        jinn_kernel::common::state::State,
        BusAudit,
    ) {
        let (actor, audit) = super::super::super::helpers::test_actor_recording().await;
        let state = actor.state.clone();
        (actor, state, audit)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_hands_off_for_dispatch_when_idle() {
        // Given an idle session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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

        // Then the entry is in history and the phase moved to Streaming — the
        // admission ask applies the fused `Idle → Sending → Streaming` edge
        // (the turn is prepared and its generation minted; the turn-dispatch
        // slice publishes the provider request).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseView::Streaming);
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
    async fn direct_dispatch_announces_the_session_going_busy() {
        // Given an idle session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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

        // Then the session going busy is announced: the admission ask applies
        // the fused `Idle → Sending → Streaming` edge and the phase actor
        // publishes it, so a subscriber never watches the session go busy
        // with no event.
        let phases = audit.of_type::<jinn_session_msg::SessionPhaseChanged>();
        assert!(
            phases
                .iter()
                .any(|p| p.old_phase == PhaseView::Idle && p.new_phase == PhaseView::Streaming),
            "expected SessionPhaseChanged(Idle -> Streaming); got: {phases:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn direct_dispatch_publishes_the_session_starting_to_work() {
        // Given an idle session.
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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

        // Then the session is announced as working, which is the boundary the
        // working-time monitor opens its interval on.
        let work = audit.of_type::<jinn_session_msg::WorkStateChanged>();
        assert_eq!(work.len(), 1, "{work:?}");
        assert!(work[0].working, "{work:?}");
        assert_eq!(work[0].session_id, session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_sets_title_from_first_message() {
        // Given a new session with no title.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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
    async fn an_intersession_first_entry_leaves_an_untitled_session_untitled() {
        // Given a new session with no title.
        let (actor, state, _audit) = create_actor().await;
        let session_id = SessionId::new();

        // When enqueuing an inter-session delivery as the first entry.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::intersession(
                    SessionId::new(),
                    None,
                    session_id.clone(),
                    "[Message from session abc]: stage finished",
                ),
            })
            .await;

        // Then the title stays unset — a stamped empty string would flip the
        // minting guard to `Some("")` and block every future title.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.title(), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_later_user_message_still_mints_the_title_after_an_intersession_entry() {
        // Given a session whose first entry was an inter-session delivery.
        let (actor, state, _audit) = create_actor().await;
        let session_id = SessionId::new();
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::intersession(
                    SessionId::new(),
                    None,
                    session_id.clone(),
                    "[Message from session abc]: stage finished",
                ),
            })
            .await;

        // The first dispatch left the session Streaming; settle the turn
        // through the ledger so the user's message dispatches fresh.
        let settled = actor.services.turn_ledger.finish_streaming(&session_id);
        assert!(
            settled.is_some(),
            "a live turn's finished edge must settle: {settled:?}"
        );
        {
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session")
                .settle_to_idle();
        }

        // When the user sends a real message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("the real first question"),
            })
            .await;

        // Then the title mints from that user message — the intersession
        // entry consumed no mint.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.title(), Some("the real first question"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_queues_when_busy() {
        // Given a session in Streaming phase (busy).
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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
        assert_eq!(session.phase(), PhaseView::Streaming);
        // No history entry because the message was queued, not pushed.
        assert_eq!(session.history().len(), 0);
        // The queue should have the message.
        assert_eq!(session.message_queue().len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_enqueue_user_message_no_provider_sends_none_provider_id() {
        // Given a session with default model (NO_PROVIDER_ID).
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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
            let mut guard = state.write();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };

        // When pushing a chat entry.
        let entry = ChatEntry::user("pushed");
        actor
            .handle_push_chat_entry(&PushChatEntry {
                session_id: session_id.clone(),
                entry: entry.clone(),
                pin: None,
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
    async fn before_turn_no_attachments_dispatches_normally() {
        // Given an idle session with no attachments.
        let (actor, state, _audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
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

        // Then the turn was prepared (history has the entry, the admission
        // fused the phase to Streaming).
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseView::Streaming);
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
        let mut guard = actor.state.write();
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
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        // Write the image to a temp file and seed the capability table.
        let dir = jinn_testutil::tempdir();
        let png_path = dir
            .path
            .join("img.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        seed_models_dev(&actor, "text-only-model", false);
        let display = format!("describe this @{png_path}");

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
        assert_eq!(session.phase(), PhaseView::Idle);
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
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        let dir = jinn_testutil::tempdir();
        let png_path = dir
            .path
            .join("photo.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        seed_models_dev(&actor, "vision-model", true);
        let display = format!("describe this @{png_path}");

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
        assert_eq!(session.phase(), PhaseView::Streaming);
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
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        // Set a model id but write NO models.dev entry for it.
        {
            let mut guard = state.write();
            guard
                .active_session_mut()
                .set_model(ModelSelection::Single("my-uncatalogued-llama".to_owned()));
        }
        let dir = jinn_testutil::tempdir();
        let png_path = dir
            .path
            .join("img.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png_path, MULTIMODAL_TINY_PNG).expect("write png");
        let display = format!("describe this @{png_path}");

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
        assert_eq!(session.phase(), PhaseView::Idle);
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
            let mut guard = state.write();
            let _session = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        {
            let mut guard = state.write();
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
        assert_eq!(session.phase(), PhaseView::Streaming);
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
            let mut guard = state.write();
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
            let mut guard = state.write();
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

        // And the session is now in Streaming phase — the admission ask
        // fused `Idle → Sending → Streaming` when it minted the generation
        // behind the prepared handoff.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            session.phase(),
            PhaseView::Streaming,
            "phase should be Streaming after the prepared resume handoff"
        );

        // And no item was queued (we dispatched inline, not via the queue).
        assert!(
            session.message_queue().is_empty(),
            "resume from Idle should not enqueue; it dispatches inline"
        );

        // And exactly one System marker was pushed to history.
        let markers: Vec<_> = session
            .history()
            .iter()
            .filter(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::System { .. }))
            .collect();
        assert_eq!(markers.len(), 1, "expected one System marker pushed");
    }

    // Helper: seed a vision-capable model and return the idle session id.
    async fn idle_vision_session() -> (
        super::super::super::SessionPersistenceActor,
        jinn_kernel::common::state::State,
        BusAudit,
        jinn_core_types::SessionId,
    ) {
        let (actor, state, audit) = create_actor().await;
        let session_id = {
            let mut guard = state.write();
            let _ = guard.active_session_mut();
            guard.session.active_session_id().clone()
        };
        seed_models_dev(&actor, "vision-model", true);
        (actor, state, audit, session_id)
    }

    /// Extracts the `expanded` text of the most recent `User` entry in history.
    fn last_user_expanded(session: &jinn_session_state::ChatSessionState) -> Option<String> {
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
        assert_eq!(session.phase(), PhaseView::Streaming);
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
        let dir = jinn_testutil::tempdir();
        let notes = dir
            .path
            .join("notes.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&notes, b"not an image").expect("write");

        // When enqueuing a message with an @path to a non-image file.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("see @{notes}")),
            })
            .await;

        // Then the turn was prepared and handed off for dispatch.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(session.phase(), PhaseView::Streaming);
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
        let dir = jinn_testutil::tempdir();
        let notes = dir
            .path
            .join("notes.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&notes, b"not an image").expect("write");
        let token = format!("@{notes}");

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
        let dir = jinn_testutil::tempdir();
        let heic = dir
            .path
            .join("photo.heic")
            .expect("temp root joins a bare name");
        let mut bytes = vec![0x00, 0x00, 0x00, 0x18];
        bytes.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&heic, &bytes).expect("write");

        // When enqueuing a message with the HEIC @path.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user(format!("see @{heic}")),
            })
            .await;

        // Then an Error entry is pushed and the turn is blocked.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        assert_eq!(
            session.phase(),
            PhaseView::Idle,
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
        let dir = jinn_testutil::tempdir();
        let png = dir
            .path
            .join("real.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let display = format!("see @{png} and @/nonexistent/x");

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
        assert_eq!(session.phase(), PhaseView::Streaming);
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
        let dir = jinn_testutil::tempdir();
        let png = dir
            .path
            .join("real.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let notes = dir
            .path
            .join("notes.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&notes, b"text").expect("write");
        let display = format!("see @{png} and @{notes}");

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
        assert_eq!(session.phase(), PhaseView::Streaming);
        let expanded = last_user_expanded(session).expect("user entry");
        assert!(
            expanded.contains(&format!("@{notes}")),
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
        let dir = jinn_testutil::tempdir();
        let png_a = dir.path.join("a.png").expect("temp root joins a bare name");
        let png_b = dir.path.join("b.png").expect("temp root joins a bare name");
        std::fs::write(&png_a, MULTIMODAL_TINY_PNG).expect("write a");
        std::fs::write(&png_b, MULTIMODAL_TINY_PNG).expect("write b");
        let display = format!("see @{png_a} and @{png_b}");

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
        let dir = jinn_testutil::tempdir();
        let png = dir
            .path
            .join("real.png")
            .expect("temp root joins a bare name");
        std::fs::write(&png, MULTIMODAL_TINY_PNG).expect("write png");
        let heic = dir
            .path
            .join("photo.heic")
            .expect("temp root joins a bare name");
        let mut bytes = vec![0x00, 0x00, 0x00, 0x18];
        bytes.extend_from_slice(b"ftypheicpayload");
        std::fs::write(&heic, &bytes).expect("write");
        let display = format!("see @{png} and @{heic}");

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
            PhaseView::Idle,
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
