//! Stall-retry handler — re-dispatches a turn whose LLM stream went silent.
//!
//! See [`SessionPersistenceActor::on_retry_stalled_session`]. The
//! `stall-watchdog` plugin detects silence on an in-flight provider stream
//! and pushes a mirrored `RestartStalledStream`, which the plugin
//! coordinator translates into
//! [`RetryStalledSession`](crate::feat::session::protocol::retry_stalled_session::RetryStalledSession).
//! A hung stream is treated like a hard provider error: partial streaming
//! entries are discarded and the turn is re-dispatched.

use crate::common::actor_deps::BusPublish;
use crate::feat::provider::protocol::command::SendToLlmProvider;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::retry_stalled_session::RetryStalledSession;
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
    /// events, which is exactly what the stale-generation drop in
    /// `apply_stream_completion` compares against.
    ///
    /// A second dispatch for the same session simply overwrites the guard:
    /// newest generation wins (the LLM actor aborts the superseded task),
    /// matching the stale-completion drop semantics.
    pub(in crate::feat::session::session_actor) fn on_send_to_llm_provider(
        &self,
        payload: &SendToLlmProvider,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            session.core.ephemeral.stream_dispatched_at = Some(payload.dispatched_at);
        });
        tracing::debug!(
            session_id = %payload.session_id,
            dispatched_at = %payload.dispatched_at,
            origin = ?payload.origin,
            "in-flight-stream guard armed at dispatch receipt"
        );
    }
}

impl SessionPersistenceActor {
    /// Re-dispatch a stalled turn: discard partial streaming entries and
    /// re-send the existing history.
    ///
    /// The visible retry marker is pushed by the `stall-watchdog` plugin
    /// (via `InsertSystemEntry`, alongside the restart request) — this
    /// handler only performs the history surgery and re-dispatch.
    ///
    /// The guard is *in-flight-stream*, not elapsed time: the handler acts
    /// only when the phase is `Sending`/`Streaming` **and**
    /// `stream_dispatched_at` is set — i.e. an LLM request is genuinely in
    /// flight. That timestamp is armed by the session actor's own
    /// `SendToLlmProvider` subscription ([`Self::on_send_to_llm_provider`] —
    /// the single write point, covering every dispatch path) and cleared when
    /// the generation's `StreamCompleted` is consumed, so:
    ///
    /// - a stream that self-resolved between the plugin's trip and this
    ///   handler running has a `None` timestamp → no-op (the self-resolved
    ///   race is closed by construction, not by timestamp comparison);
    /// - a session waiting on a tool batch has a `None` timestamp (the
    ///   generation completed with `ToolUse` before tools dispatch) → no-op:
    ///   a restart during tool execution is structurally impossible, even if
    ///   a guest misfires.
    pub(in crate::feat::session::session_actor) async fn on_retry_stalled_session(
        &self,
        payload: &RetryStalledSession,
    ) {
        // Discard partial streaming entries — but only while a stream is
        // genuinely in flight for this session.
        let acted = self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            if matches!(session.phase(), PhaseKind::Sending | PhaseKind::Streaming)
                && session.core.ephemeral.stream_dispatched_at.is_some()
            {
                let removed = session.reset_streaming_entries_for_retry();
                // Partial tool calls left by a starved/errored stream
                // must be excluded from the retried request, otherwise
                // the next provider call carries structurally invalid
                // (truncated-arguments) entries. Mirrors the `Canceled`
                // path.
                let excluded = session.force_exclude_dangling_tool_calls();
                tracing::warn!(
                    session_id = %payload.session_id,
                    removed_entries = removed,
                    excluded_dangling = excluded.len(),
                    attempt = payload.attempt,
                    "retrying stalled turn"
                );
                // The re-dispatch below emits a fresh `SendToLlmProvider`,
                // which the plugin host forwards as `stream_start` — the
                // watchdog re-arms for the new generation automatically.
                true
            } else {
                // A rejection is worth one warn line: silent no-ops here
                // cost hours when the watchdog and the session disagree
                // about whether a stream is in flight.
                tracing::warn!(
                    session_id = %payload.session_id,
                    phase = ?session.phase(),
                    stream_in_flight = session.core.ephemeral.stream_dispatched_at.is_some(),
                    "stalled-stream restart refused: no in-flight stream"
                );
                false
            }
        });

        if !acted {
            return;
        }

        // Phase is already Streaming (we didn't change it above); emit a
        // no-op-safe phase-changed event for consistency with other dispatch
        // paths, then re-send the assembled history.
        super::super::helpers::emit_history_appended(self.bus(), &payload.session_id).await;

        // Hand the prepared turn to the turn-dispatch slice: it assembles
        // the prompt (summarized into the slice's warn so a misretried turn
        // is decidable from logs), resolves the model, and publishes the
        // fresh `SendToLlmProvider` — which the session actor's own receipt
        // arms into the in-flight-stream guard, re-arming the watchdog for
        // the new generation automatically.
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
    use crate::common::services::BusAudit;
    use crate::feat::provider::protocol::command::SendToLlmProvider;

    use crate::feat::session::protocol::retry_stalled_session::RetryStalledSession;
    use crate::feat::session::session_actor::SessionPersistenceActor;
    use crate::protocol::ChatEntryKind;
    use crate::protocol::SessionId;

    /// A session in `Streaming` with a partial assistant entry, a dangling
    /// partial tool call slot free, and an in-flight stream generation
    /// registered — the exact shape a stalled stream presents.
    async fn stall_setup() -> (SessionPersistenceActor, BusAudit, RetryStalledSession) {
        let (actor, audit) = test_actor_recording().await;
        // The retry path assembles through the trouper service; spawn it
        // on this actor's system (production wiring does this at boot).
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            // A partial assistant entry created via the streaming path so it
            // registers a streaming index and is discarded on retry.
            session
                .append_stream_token("partial", jiff::Timestamp::now())
                .expect("append first token");
            // Register the in-flight stream generation — the guard's source
            // of truth.
            session.core.ephemeral.stream_dispatched_at = Some(jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };
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
    async fn handler_discards_partial_entries_and_redispatches() {
        // Given a stalled Streaming session holding a partial assistant entry
        // and an in-flight stream generation.
        let (actor, audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then the partial assistant entry is gone.
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            let has_partial = session
                .core
                .history
                .iter()
                .any(|e| matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial"));
            assert!(!has_partial, "partial assistant entry must be discarded");
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
    async fn handler_noops_when_no_stream_is_in_flight() {
        // Given a session whose stream generation was consumed between the
        // plugin's trip and this handler running (the self-resolved shape:
        // `StreamCompleted` cleared `stream_dispatched_at`).
        let (actor, _audit, payload) = stall_setup().await;
        let session_id = payload.session_id.clone();
        {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.core.ephemeral.stream_dispatched_at = None;
        }

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then nothing was discarded and no marker was pushed: the partial
        // assistant entry is still present.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.core.history.iter().any(|e| matches!(
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
            use crate::feat::session::phase_machine::PhaseTransitions;
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            let _ = session
                .core
                .ephemeral
                .machine
                .on_stream_completed_finished();
        }

        // When the retry handler runs.
        actor.on_retry_stalled_session(&payload).await;

        // Then nothing was discarded: history is untouched.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.core.history.iter().any(|e| matches!(
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
            let mut state = actor.state.write_test_no_cap();
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
        let has_active_partial = session.core.history.iter().any(|e| {
            matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-partial")
                && !matches!(
                    e.context_override(),
                    crate::protocol::ContextOverride::ForcedExclude
                )
        });
        assert!(
            !has_active_partial,
            "dangling partial tool call must be excluded from the retried request"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_command_arms_the_stall_guard() {
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

        // Then the session's in-flight-stream guard is armed at the command's
        // dispatch timestamp.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.core.ephemeral.stream_dispatched_at,
            Some(dispatched_at),
            "SendToLlmProvider receipt must arm the in-flight-stream guard"
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
        // And the partial assistant entry was discarded.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session.core.history.iter().any(|e| matches!(
                e.kind, ChatEntryKind::Assistant(ref t) if t == "partial"
            )),
            "retry after dispatch receipt must discard partial entries"
        );
    }
}
