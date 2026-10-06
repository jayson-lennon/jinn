//! Stall-retry handler — re-dispatches a turn whose LLM stream went silent.
//!
//! See [`SessionPersistenceActor::on_retry_stalled_session`]. The
//! stall-watchdog actor in the `jinn-watchdog` slice detects silence on an
//! in-flight provider stream and publishes
//! [`RetryStalledSession`](jinn_session_msg::RetryStalledSession)
//! (alongside the visible retry marker entry). A hung stream is treated like
//! a hard provider error: the stalled attempt's partial entries are taken out
//! of context but left visible in the chat log, the session phase is rewound
//! to `Sending`, and the turn is re-dispatched.

use jinn_inference_msg::SendToLlmProvider;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_msg::PhaseKind;
use jinn_session_msg::RetryStalledSession;
use jinn_turn_dispatch_msg::DispatchTurn;

use super::super::SessionPersistenceActor;

impl SessionPersistenceActor {
    /// Arm the in-flight-stream guard when an LLM dispatch reaches the actor.
    ///
    /// This is the guard's **single write point**: every `SendToLlmProvider`
    /// publisher (user message, queued/steered dispatch, direct dispatch,
    /// tool-loop continuation, stall retry) flows through the bus, so receipt
    /// here covers all dispatch paths — present and future. The stored
    /// timestamp is the same one the LLM actor embeds in downstream stream
    /// Logs dispatch receipt. The in-flight guard itself is armed by the
    /// phase actor at admission (`BeginStream`), not here — a dispatch
    /// that was refused never reaches the bus, and an armed guard with no
    /// stream behind it was exactly the wedge.
    pub(in crate::session_actor) fn on_send_to_llm_provider(&self, payload: &SendToLlmProvider) {
        tracing::debug!(
            session_id = %payload.session_id,
            dispatched_at = %payload.dispatched_at,
            origin = ?payload.origin,
            "dispatch receipt (guard armed by the phase actor at admission)"
        );
    }
}

impl SessionPersistenceActor {
    /// Re-dispatch a stalled turn: discard partial streaming entries and
    /// re-send the existing history.
    ///
    /// The visible retry marker is pushed by the stall-watchdog actor in
    /// the `jinn-watchdog` slice (via `PushChatEntry`, alongside the
    /// restart request) — this handler only performs the history surgery
    /// and re-dispatch.
    ///
    /// The guard is *in-flight-stream*, not elapsed time: the handler acts
    /// only when the phase is `Sending`/`Streaming` **and**
    /// `stream_dispatched_at` is set — i.e. an LLM request is genuinely in
    /// flight. That timestamp is armed by the session actor's own
    /// `SendToLlmProvider` subscription ([`Self::on_send_to_llm_provider`] —
    /// the single write point, covering every dispatch path) and cleared when
    /// the generation's `StreamCompleted` is consumed, so:
    ///
    /// - a stream that self-resolved between the watchdog's trip and this
    ///   handler running has a `None` timestamp → no-op (the self-resolved
    ///   race is closed by construction, not by timestamp comparison);
    /// - a session waiting on a tool batch has a `None` timestamp (the
    ///   generation completed with `ToolUse` before tools dispatch) → no-op:
    ///   a restart during tool execution is structurally impossible, even if
    ///   a misfire occurs.
    pub(in crate::session_actor) async fn on_retry_stalled_session(
        &self,
        payload: &RetryStalledSession,
    ) {
        // Entry surgery first, and only while a stream is genuinely in
        // flight: the rewind edge clears the machine's streaming indices,
        // and they are the only record of which entries this generation
        // owned. The gate doubles as the admission filter — a cancelled
        // session is `Idle` with no stamp, so a dead generation never
        // reaches the surgery.
        let stamp = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            if matches!(session.phase(), PhaseKind::Sending | PhaseKind::Streaming)
                && session.has_in_flight_stream()
            {
                // The stalled attempt's entries stay in history for the user
                // to read; they are only excluded from the retried request.
                let excluded = session.reset_streaming_entries_for_retry();
                // Belt-and-braces for a dangling loop the streaming indices
                // do not cover (e.g. one left by an earlier interrupted
                // generation). Mirrors the `Canceled` path.
                let dangling = session.force_exclude_dangling_tool_calls();
                tracing::warn!(
                    session_id = %payload.session_id,
                    excluded_entries = excluded.len(),
                    excluded_dangling = dangling.len(),
                    attempt = payload.attempt,
                    "retrying stalled turn"
                );
                session.stream_dispatched_at()
            } else {
                // A rejection is worth one warn line: silent no-ops here
                // cost hours when the watchdog and the session disagree
                // about whether a stream is in flight.
                tracing::warn!(
                    session_id = %payload.session_id,
                    phase = ?session.phase(),
                    stream_in_flight = session.has_in_flight_stream(),
                    "stalled-stream restart refused: no in-flight stream"
                );
                None
            }
        });

        // The gate passed but the stamp can still be `None` (a finished
        // turn or a tool-batch wait), so the `None` case falls through as
        // a no-op.
        let Some(stamp) = stamp else {
            return;
        };

        // The rewind edge is the phase actor's: `Streaming → Sending`,
        // resolving against the stamp read above. A generation a cancel
        // already killed is refused here, after surgery but before the
        // dispatch — the cancelled entries stay excluded, and nothing is
        // sent for a dead turn.
        let decision =
            crate::phase_actor::admit_rewind(&self.services, &payload.session_id, stamp).await;
        if !decision.admitted {
            tracing::warn!(
                session_id = %payload.session_id,
                "stalled-stream restart refused: its generation was cancelled"
            );
            return;
        }

        // The rewind edge settled the phase to `Sending`; re-send the
        // assembled history.
        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        // Hand the prepared turn to the turn-dispatch slice: it assembles
        // the prompt (summarized into the slice's warn so a misretried turn
        // is decidable from logs), resolves the model, and publishes the
        // fresh `SendToLlmProvider` — whose queue admission re-mints the
        // rewound generation, re-arming the stall watchdog for it
        // automatically.
        self.publish(DispatchTurn {
            session_id: payload.session_id.clone(),
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
    use super::super::super::helpers::test_actor_recording;
    use jinn_inference_msg::SendToLlmProvider;
    use jinn_kernel::common::services::BusAudit;

    use crate::session_actor::SessionPersistenceActor;
    use jinn_core_types::ChatEntryKind;
    use jinn_core_types::SessionId;
    use jinn_session_msg::PhaseKind;
    use jinn_session_msg::RetryStalledSession;

    /// A session in `Streaming` with a partial assistant entry, a dangling
    /// partial tool call slot free, and an in-flight stream generation
    /// registered — the exact shape a stalled stream presents.
    async fn stall_setup() -> (SessionPersistenceActor, BusAudit, RetryStalledSession) {
        let (actor, audit) = test_actor_recording().await;
        // The retry path assembles through the trouper service; spawn it
        // on this actor's system (production wiring does this at boot).
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = actor.state.read().session.active_session_id().clone();
        // Mint first: the live generation is the stall guard's source of
        // truth, and the fused mint edge puts the machine in Streaming so
        // the partial entries below can register their streaming indices.
        crate::phase_actor::admit_stream(
            &actor.services,
            &session_id,
            jinn_session_msg::phase_command::DispatchKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            // A partial assistant entry created via the streaming path so it
            // registers a streaming index and is discarded on retry.
            session
                .append_stream_token("partial", jiff::Timestamp::now())
                .expect("append first token");
        }
        (
            actor,
            audit,
            RetryStalledSession {
                session_id,
                attempt: 2,
                max_restarts: 3,
            },
        )
    }

    /// A stalled session whose failed attempt left the full three-entry shape
    /// a real stall produces — partial assistant text, a thinking entry, and a
    /// tool call — which is exactly the run length the chat log collapses.
    async fn stall_setup_with_full_attempt()
    -> (SessionPersistenceActor, BusAudit, RetryStalledSession) {
        let (actor, audit) = test_actor_recording().await;
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = actor.state.read().session.active_session_id().clone();
        crate::phase_actor::admit_stream(
            &actor.services,
            &session_id,
            jinn_session_msg::phase_command::DispatchKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session
                .append_stream_token("partial", jiff::Timestamp::now())
                .expect("append first token");
            session.begin_thinking(jiff::Timestamp::now());
            session
                .append_thinking_token("weighing options")
                .expect("append thinking token");
            if let Some(idx) = session.streaming_thinking_entry_index() {
                session.finish_thinking_entry(idx);
            }
            let tool_call_index = session.history().len();
            session.begin_tool_call(tool_call_index, "call_1", "read", jiff::Timestamp::now());
        }
        (
            actor,
            audit,
            RetryStalledSession {
                session_id,
                attempt: 2,
                max_restarts: 3,
            },
        )
    }

    /// A `SendToLlmProvider` dispatch for `session_id` at `dispatched_at`,
    /// built by deserializing the minimal payload shape (mirrors production:
    /// most fields default).
    fn dispatch_payload(
        session_id: &SessionId,
        dispatched_at: jiff::Timestamp,
    ) -> SendToLlmProvider {
        serde_json::from_value(serde_json::json!({
            "session_id": session_id.to_string(),
            "messages": [],
            "dispatched_at": dispatched_at.to_string(),
        }))
        .expect("minimal SendToLlmProvider deserializes")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_keeps_partial_entries_but_excludes_them_and_redispatches() {
        // Given a stalled Streaming session holding a partial assistant entry
        // and an in-flight stream generation.
        let (actor, audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the partial assistant entry is still in the chat log — the user
        // can see the attempt that was discarded.
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            assert!(
                session
                    .history()
                    .iter()
                    .any(|e| matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial")),
                "the discarded attempt must stay visible"
            );
            // And it is excluded from the retried request.
            let still_in_context = session.history().iter().any(|e| {
                matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial")
                    && e.is_in_context()
            });
            assert!(
                !still_in_context,
                "the discarded attempt must not reach the provider"
            );
        }
        // And the turn was handed to the turn-dispatch slice for
        // re-dispatch (the queue actor owns the `SendToLlmProvider`
        // emission; see the slice's DispatchTurn tests).
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert!(
            handed_off.iter().any(|s| s.session_id == session_id),
            "DispatchTurn must be published to re-dispatch the turn"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_registers_the_discarded_attempt_as_an_expanded_block() {
        // Given a stalled session whose attempt produced an assistant entry, a
        // thinking entry, and a tool call — three entries, exactly the run
        // length the chat log collapses by default.
        let (actor, _audit, payload) = stall_setup_with_full_attempt().await;
        let session_id = payload.session_id.clone();

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the whole attempt is excluded from context AND registered as a
        // shown (expanded) block.
        //
        // The registration is the half that keeps the attempt readable: the
        // chat log collapses any contiguous run of `!is_in_context()` entries
        // at or past its collapse threshold and further than its proximity
        // window from the tail, and it cannot tell a discarded stall from a
        // block the user chose to ignore. Asserting membership rather than a
        // rendered `VisualItem` list keeps this seam free of a dependency on
        // the view slice — `build_visual_items` reads exactly this set to
        // decide whether a block is shown.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let excluded: Vec<_> = session
            .history()
            .iter()
            .filter(|e| !e.is_in_context())
            .map(|e| e.id.clone())
            .collect();
        assert!(
            excluded.len() >= 3,
            "expected the whole attempt excluded, got {}",
            excluded.len()
        );

        let shown = session.shown_ignored_blocks_snapshot();
        for id in &excluded {
            assert!(
                shown.contains(id),
                "discarded attempt entry {id:?} must be registered as an expanded \
                 block, or it collapses into a hidden-entries line once the retried \
                 generation runs past the proximity window"
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_leaves_a_discarded_tool_call_looking_unfinished() {
        // Given a stalled session whose attempt left a tool call mid-arguments.
        let (actor, _audit, payload) = stall_setup_with_full_attempt().await;
        let session_id = payload.session_id.clone();

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the discarded tool call carries the same signature an abandoned
        // partial does — streamed, never finished, out of context — which is
        // what the chat log reads to keep rendering its arguments expanded
        // instead of collapsing them to a truncated one-liner.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let tool_call = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, ChatEntryKind::ToolCall { .. }))
            .expect("the attempt left a tool call");

        assert!(
            matches!(
                tool_call.timing,
                jinn_core_types::EntryTiming::Streamed { .. }
            ),
            "a tool call must keep Streamed timing through a discard: {:?}",
            tool_call.timing
        );
        assert!(
            tool_call.timing.finished_at().is_none(),
            "a discarded partial must never gain a finish stamp: {:?}",
            tool_call.timing
        );
        assert!(
            !tool_call.is_in_context(),
            "the discarded call must be out of context"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_rewinds_the_phase_so_the_retry_can_stream() {
        // Given a stalled Streaming session.
        let (actor, _audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the session is back in Sending — staying in Streaming made the
        // retried dispatch's first token an illegal transition.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.phase(),
            PhaseKind::Sending,
            "the rewind ask must settle the phase to Sending;              stream_dispatched_at = {:?}",
            session.stream_dispatched_at()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_noops_when_no_stream_is_in_flight() {
        // Given a session whose stream generation was consumed between the
        // watchdog's trip and this handler running (the self-resolved shape:
        // `StreamCompleted` cleared `stream_dispatched_at`).
        let (actor, _audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.clear_stream_generation();
        }

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then nothing was discarded and no marker was pushed: the partial
        // assistant entry is still present.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.history().iter().any(|e| matches!(
                e.kind, ChatEntryKind::Assistant(ref t) if t == "partial"
            )),
            "a self-resolved stream must not be discarded"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_noops_when_session_is_idle() {
        // Given an Idle session with a stale guard value (defensive: both a
        // finished turn and a tool-batch wait present `None`, but the guard
        // must not rely on phase alone either).
        let (actor, _audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.finalize_entries_for_finish(true, jiff::Timestamp::now());
            session.finish_streaming_via_machine();
        }

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then nothing was discarded: history is untouched.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.history().iter().any(|e| matches!(
                e.kind, ChatEntryKind::Assistant(ref t) if t == "partial"
            )),
            "an idle session must not be restarted"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handler_excludes_dangling_partial_tool_call() {
        // Given a stalled Streaming session holding a partial (dangling)
        // tool call with no matching ToolResult.
        let (actor, _audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            let now = jiff::Timestamp::now();
            session.begin_tool_call(0, "tc-partial", "bash", now);
            session
                .append_tool_call_delta(0, "{\"command\":\"cd /mnt")
                .expect("append partial delta");
        }

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the partial tool call entry is no longer included in the
        // request context — it is marked ForcedExclude.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let has_active_partial = session.history().iter().any(|e| {
            matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-partial")
                && !matches!(
                    e.context_override(),
                    jinn_core_types::ContextOverride::ForcedExclude
                )
        });
        assert!(
            !has_active_partial,
            "dangling partial tool call must be excluded from the retried request"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_receipt_is_a_noop_the_phase_actor_owns_the_guard() {
        // Given a session actor with no in-flight stream for the active session.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };
        let dispatched_at = jiff::Timestamp::now();
        let payload = dispatch_payload(&session_id, dispatched_at);

        // When the dispatch command reaches the session actor.
        actor.on_send_to_llm_provider(&payload);

        // Then nothing was armed by the receipt: the guard is the phase
        // actor's, armed at BeginStream admission, and a dispatch's
        // publisher never writes session state.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.stream_dispatched_at(),
            None,
            "SendToLlmProvider receipt must not arm the guard; \
             the phase actor owns it"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn retry_after_dispatch_receipt_acts() {
        // Given a Streaming session whose in-flight-stream guard was armed the
        // way production arms it — by the session actor receiving the turn's
        // `SendToLlmProvider` dispatch (any publisher path).
        let (actor, audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();
        let first_dispatch = jiff::Timestamp::now();
        actor.on_send_to_llm_provider(&dispatch_payload(&session_id, first_dispatch));
        let _ = first_dispatch; // guard arming is asserted by `dispatch_command_arms_the_stall_guard`

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the turn was handed to the turn-dispatch slice, which owns
        // the fresh `SendToLlmProvider` emission (it stamps its own fresh
        // `dispatched_at`; see the slice's DispatchTurn tests).
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handed_off.len(),
            1,
            "retry must hand off exactly one fresh dispatch"
        );
        // And the partial assistant entry is out of context.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session.history().iter().any(|e| {
                matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial")
                    && e.is_in_context()
            }),
            "retry after dispatch receipt must exclude partial entries from the request"
        );
    }
}
