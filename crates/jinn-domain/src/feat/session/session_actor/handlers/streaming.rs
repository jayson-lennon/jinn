//! Streaming lifecycle handlers - manage token streaming and stream completion.
//!
//! Handles appending individual tokens to the assistant entry (including
//! reasoning/thinking tokens), and finalizing the stream with token accounting
//! and queue draining on `StreamCompleted`.

use std::collections::VecDeque;

use crate::common::actor_deps::BusPublish;
use crate::feat::context::protocol::event::ContextOverrideChanged;
use crate::feat::context::strategy::token_estimator::{TiktokenCounter, TokenCounter};
use crate::feat::provider::protocol::event::{StreamCompleted, StreamCompletedReason, StreamToken};
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::protocol::citations_received::CitationsReceived;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use crate::feat::session::queue_item::QueueItem;
use jinn_core_types::tool_types::ToolCall;
use crate::protocol::{ChatEntry, ChatEntryId, ChatEntryKind, SessionId};

use super::super::SessionPersistenceActor;
use crate::feat::session::phase_machine::PhaseKind;

impl SessionPersistenceActor {
    /// Appends a streaming token to the session's assistant entry,
    /// or to the thinking entry if the token is flagged as reasoning.
    pub(in crate::feat::session::session_actor) fn on_stream_token(&self, event: &StreamToken) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            match session.phase() {
                PhaseKind::Streaming => {}
                PhaseKind::Sending => {
                    // Defensive: stream token arrived without phase transition.
                    session.begin_streaming();
                }
                PhaseKind::Idle => {
                    tracing::warn!(
                        phase = ?session.phase(),
                        "StreamToken received in unexpected phase"
                    );
                }
            }
            if event.is_thinking {
                if session.streaming_thinking_entry_index().is_none() {
                    session.begin_thinking(event.dispatched_at);
                }
                if let Err(e) = session.append_thinking_token(&event.token) {
                    tracing::error!(err = ?e, "failed to append thinking token");
                }
            } else {
                // First non-thinking (content) token ends the reasoning phase:
                // finalize the thinking entry's duration before the content begins.
                if let Some(idx) = session.streaming_thinking_entry_index() {
                    session.finish_thinking_entry(idx);
                }
                if let Err(e) = session.append_stream_token(&event.token, event.dispatched_at) {
                    tracing::error!(err = ?e, "failed to append stream token");
                }
            }
        });
    }

    /// Marks the session's stream as finished, records output tokens, and drains
    /// any queued messages into a new turn.
    ///
    /// This is orchestration only: token counting, locked-state mutation, and
    /// override-change emission are each delegated to a dedicated helper so the
    /// handler reads as a step-by-step recipe. See [`Self::apply_stream_completion`]
    /// for the under-lock state transitions and [`resolve_output_tokens`] for the
    /// token-accounting policy.
    pub(in crate::feat::session::session_actor) async fn on_stream_completed(
        &self,
        event: &StreamCompleted,
    ) {
        let should_save = matches!(
            event.reason,
            StreamCompletedReason::Finished
                | StreamCompletedReason::Error
                | StreamCompletedReason::Canceled,
        );

        // Count output tokens outside the lock (may spawn_blocking).
        let output_tokens = resolve_output_tokens(self.counter, event).await;

        // Mutate session state under the write lock, capturing what changed.
        // A `None` return means the completion was from a superseded stream
        // generation and was dropped — emit nothing.
        let Some(state_change) = self.apply_stream_completion(event, output_tokens) else {
            tracing::debug!(
                session_id = %event.session_id,
                "StreamCompleted dropped (stale generation); skipping downstream events"
            );
            return;
        };

        // Emit ContextOverrideChanged for entries swept by dangling-tool-call
        // exclusion or pending worker mutations. Outside the write lock.
        self.emit_override_changes(&event.session_id, state_change.changed_overrides)
            .await;

        super::super::helpers::emit_phase_changed(
            self.bus(),
            &event.session_id,
            state_change.old_phase,
            state_change.new_phase,
        )
        .await;

        // Cancel-consumed-by-frontend: the synchronous ESC-cancel path
        // (`cancel_stream_and_drain` in the intent handler) transitions the phase
        // `Streaming → Idle` directly in the shared `State` without emitting a
        // bus event. By the time this `StreamCompleted(Canceled)` arrives, the
        // phase is already `Idle`, so `emit_phase_changed` above correctly
        // skipped the `Idle → Idle` no-op. But subscribers (discord bridge, queue
        // actor, history workers) still need a turn-end signal — and history is now
        // complete with the `Error("Cancelled")` entry pushed by
        // `apply_completion_entries`. Force-publish so they learn the turn ended.
        if state_change.reason == StreamCompletedReason::Canceled
            && state_change.old_phase == PhaseKind::Idle
            && state_change.new_phase == PhaseKind::Idle
        {
            self.bus()
                .publish(SessionPhaseChanged {
                    session_id: event.session_id.clone(),
                    old_phase: PhaseKind::Idle,
                    new_phase: PhaseKind::Idle,
                })
                .await;
        }
        super::super::helpers::emit_history_appended(self.bus(), &event.session_id).await;

        // Persist session after stream finishes.
        if should_save {
            self.save_active_session(&event.session_id).await;
        }

        // Drain a buffered `ToolBatchCompleted` that raced ahead of this
        // `StreamCompleted(ToolUse)`. The buffer is populated by
        // `on_tool_batch_completed` when the batch arrives while the session
        // is still `Streaming`. Now that the phase has advanced to `Sending`,
        // the continuation can be dispatched.
        let drained_batch = event.reason == StreamCompletedReason::ToolUse && {
            self.state.with_session(&self.cap, |view| {
                view.session
                    .map()
                    .get_or_create(&event.session_id)
                    .core
                    .ephemeral
                    .pending_tool_batch
                    .take()
                    .is_some()
            })
        };

        if drained_batch {
            tracing::info!(
                session_id = %event.session_id,
                "draining buffered ToolBatchCompleted after StreamCompleted(ToolUse)"
            );
            self.continue_tool_loop(&event.session_id).await;
        }
    }

    /// Handles `CitationsReceived`: appends a single display-only `Annotation`
    /// entry recording the turn's `url_citation` sources, then persists.
    pub(in crate::feat::session::session_actor) async fn on_citations_received(
        &self,
        event: &CitationsReceived,
    ) {
        if event.citations.is_empty() {
            return;
        }

        {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(&event.session_id);
                session.push_entry(ChatEntry::annotation(event.citations.clone()));
            });
        }

        super::super::helpers::emit_history_appended(self.bus(), &event.session_id).await;
        self.save_active_session(&event.session_id).await;
    }

    /// Applies all stream-completion state mutations under the write lock.
    ///
    /// Pushes reason-specific entries, finalizes token accounting, finishes
    /// streaming, sweeps dangling tool calls on hard cancel, applies pending
    /// history mutations, transitions the phase, and - on error/cancel - drains
    /// queued messages back into the input buffer for the user to retry.
    ///
    /// Returns the before/after phase and the entry IDs whose context overrides
    /// changed, so the caller can emit events outside the lock.
    fn apply_stream_completion(
        &self,
        event: &StreamCompleted,
        output_tokens: Option<u32>,
    ) -> Option<StreamCompletionStateChange> {
        let mut changed_overrides: Vec<ChatEntryId> = Vec::new();
        self.state
            .with_session(&self.cap, |view| -> Option<StreamCompletionStateChange> {
                let session = view.session.map().get_or_create(&event.session_id);

                // Stale-generation guard: reject terminal events from an aborted prior
                // stream (e.g. a retry re-dispatched while the old task was still
                // alive). A completion whose `dispatched_at` predates the current
                // generation is dropped silently.
                if let Some(active) = session.core.ephemeral.stream_dispatched_at
                    && event.dispatched_at < active
                {
                    tracing::warn!(
                        session_id = %event.session_id,
                        event_dispatched_at = %event.dispatched_at,
                        active_dispatched_at = %active,
                        reason = ?event.reason,
                        "dropping stale StreamCompleted from superseded stream generation"
                    );
                    return None;
                }
                // This generation is now consumed.
                session.core.ephemeral.stream_dispatched_at = None;

                let old_phase = session.phase();

                apply_completion_entries(session, event, output_tokens);

                let preserve_assistant = matches!(
                    event.reason,
                    StreamCompletedReason::Finished | StreamCompletedReason::ToolUse,
                );
                session.finish_streaming(preserve_assistant, event.dispatched_at);

                // Hard cancel: force-exclude dangling tool calls left by the interrupted stream.
                if event.reason == StreamCompletedReason::Canceled {
                    changed_overrides.extend(session.force_exclude_dangling_tool_calls());
                }

                // Apply pending history mutations for non-ToolUse completions.
                // ToolUse defers to on_tool_batch_completed.
                if event.reason != StreamCompletedReason::ToolUse {
                    let (count, changed) = session.drain_and_apply_pending_mutations();
                    changed_overrides.extend(changed);
                    if count > 0 {
                        tracing::debug!(
                            session_id = %event.session_id,
                            count,
                            reason = ?event.reason,
                            "applied pending history mutations at stream completion"
                        );
                    }
                }

                // Tool use means the conversation continues - always transition to sending
                // so the tool loop runs.
                if event.reason == StreamCompletedReason::ToolUse {
                    session.begin_sending();
                }

                // When returning to Idle on error/cancel, drain queued messages back to
                // the input buffer so the user can review and retry.
                if matches!(
                    event.reason,
                    StreamCompletedReason::Error | StreamCompletedReason::Canceled
                ) {
                    let drained = session.drain_queue();
                    if let Some(text) = drained_queue_to_text(&drained) {
                        session.update_input(|input| input.replace_all(text));
                    }
                }

                Some(StreamCompletionStateChange {
                    old_phase,
                    new_phase: session.phase(),
                    reason: event.reason,
                    changed_overrides,
                })
            })
    }

    /// Broadcasts [`ContextOverrideChanged`] for each entry whose override changed
    /// during stream completion. Called outside the write lock.
    async fn emit_override_changes(&self, session_id: &SessionId, entry_ids: Vec<ChatEntryId>) {
        for entry_id in entry_ids {
            self.publish(ContextOverrideChanged {
                session_id: session_id.clone(),
                entry_id,
            })
            .await;
        }
    }
}

/// Before/after phase and changed-entry IDs captured while mutating session state
/// under the write lock during stream completion. Consumed by the caller to emit
/// events outside the lock.
///
/// Carries the completion `reason` so the caller can detect the cancel-consumed-
/// by-frontend case: the synchronous ESC-cancel path (`cancel_stream_and_drain`)
/// transitions the phase `Streaming → Idle` directly in the shared `State` without
/// arrives, the phase is already `Idle`, so `emit_phase_changed` would skip the
/// `Idle → Idle` no-op, so subscribers (discord bridge, queue actor, history workers)
/// would never learn the turn ended. The caller detects this case via `reason`
/// and force-publishes so the turn-end signal reaches the bus.
struct StreamCompletionStateChange {
    old_phase: PhaseKind,
    new_phase: PhaseKind,
    reason: StreamCompletedReason,
    changed_overrides: Vec<ChatEntryId>,
}

/// Counts output tokens locally by summing assistant content, thinking content,
/// and tool-call arguments/names.
///
/// Pure and side-effect-free so it can be unit-tested in isolation. Used as the
/// baseline when the provider undercounts (e.g., excludes tool-call arguments).
fn count_tokens_locally(
    counter: &dyn TokenCounter,
    content: &str,
    thinking: &str,
    tool_calls: Option<&[ToolCall]>,
) -> u32 {
    let base = counter.count(content) + counter.count(thinking);
    let tool_tokens = tool_calls.map_or(0, |calls| {
        calls
            .iter()
            .map(|tc| counter.count(&tc.arguments) + counter.count(&tc.name))
            .sum::<usize>()
    });
    (base + tool_tokens) as u32
}

/// Resolves the final output token count for a completed stream.
///
/// Counts locally via `spawn_blocking` (the tokenizer is CPU-bound) unless the
/// stream was canceled/errored, then takes the max of the local and
/// provider-reported counts. Providers that undercount are corrected by the
/// local count.
async fn resolve_output_tokens(counter: TiktokenCounter, event: &StreamCompleted) -> Option<u32> {
    let provider_tokens = event.provider_completion_tokens.map(|t| t as u32);

    let local_handle = if event.reason != StreamCompletedReason::Canceled
        && event.reason != StreamCompletedReason::Error
    {
        event.assistant_content.as_ref().map(|content| {
            let content = content.clone();
            let tool_calls = event.tool_calls.clone();
            let thinking = event.thinking_content.clone().unwrap_or_default();
            tokio::task::spawn_blocking(move || {
                count_tokens_locally(&counter, &content, &thinking, tool_calls.as_deref())
            })
        })
    } else {
        None
    };

    match local_handle {
        Some(handle) => {
            let local = handle.await.unwrap_or_else(|e| {
                tracing::warn!(
                    err = ?e,
                    "spawn_blocking panicked during output token counting"
                );
                0
            });
            Some(local.max(provider_tokens.unwrap_or(0)))
        }
        None => provider_tokens,
    }
}

/// Joins the display text of queued user messages into a single
/// newline-separated string.
///
/// Returns `None` when there are no user messages to drain. Tool continuations
/// contribute no text and are dropped.
fn drained_queue_to_text(items: &VecDeque<QueueItem>) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            QueueItem::UserMessage(entry) => match &entry.kind {
                ChatEntryKind::User { display, .. } => Some(display.as_str()),
                _ => None,
            },
            QueueItem::ToolContinuation => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// Pushes reason-specific entries and finalizes token accounting under the lock.
///
/// - `Canceled`: pushes a "Cancelled" error entry.
/// - `Error`: nothing - the error entry is pushed earlier by the LLM actor via
///   `PushChatEntry` before `StreamCompleted(Error)` is emitted.
/// - `Finished`/`ToolUse`: finalizes the last token record with output tokens,
///   cost, and model, if a record exists (i.e., a prompt was assembled first).
#[expect(clippy::else_if_without_else, reason = "no-op arms are intentional")]
fn apply_completion_entries(
    session: &mut ChatSessionState,
    event: &StreamCompleted,
    output_tokens: Option<u32>,
) {
    if event.reason == StreamCompletedReason::Canceled {
        session.push_entry(ChatEntry::error("Cancelled"));
    } else if event.reason == StreamCompletedReason::Error {
        // Error entry is pushed by the LLM actor via PushChatEntry before
        // emitting StreamCompleted(Error). Nothing to push here.
    } else if let Some(output_tokens) = output_tokens {
        // Finalize the last record if one exists (i.e., prompt assembled first).
        // If no record exists (e.g., session restored mid-stream), skip silently.
        if !session.token_ledger().is_empty()
            && let Err(e) = session.finalize_last_token_record(
                output_tokens,
                event.cost,
                event.model_used.clone(),
                event
                    .provider_prompt_tokens
                    .and_then(|t| u32::try_from(t).ok()),
                event.cached_tokens.and_then(|t| u32::try_from(t).ok()),
            )
        {
            tracing::error!(err = ?e, "failed to finalize token record");
        }
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
    use super::super::super::helpers::{
        test_actor, test_actor_recording, test_actor_with_store_recording,
    };
    use crate::feat::provider::protocol::event::{
        StreamCompleted, StreamCompletedReason, StreamToken,
    };
    use crate::feat::session::phase_machine::PhaseKind;
    use crate::feat::session::protocol::citations_received::CitationsReceived;
    use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
    use crate::feat::session::token_stats::TokenRecord;
    use crate::protocol::{ChangeSource, ChatEntry, ChatEntryKind};

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_stops_streaming() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(!matches!(session.phase(), PhaseKind::Streaming));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_reason_drains_queue_to_input_buffer() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(crate::feat::session::queue_item::QueueItem::UserMessage(
                Box::new(ChatEntry::user("queued message")),
            ));
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 0);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "queued message"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_with_multiple_queued_messages_joins_with_newline() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(crate::feat::session::queue_item::QueueItem::UserMessage(
                Box::new(ChatEntry::user("first message")),
            ));
            session.enqueue(crate::feat::session::queue_item::QueueItem::UserMessage(
                Box::new(ChatEntry::user("second message")),
            ));
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 0);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "first message\nsecond message"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_reason_drains_queue_to_input_buffer() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(crate::feat::session::queue_item::QueueItem::UserMessage(
                Box::new(ChatEntry::user("queued message")),
            ));
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 0);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "queued message"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_emits_history_appended() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream completed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_emits_history_appended() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_emits_history_appended() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream canceled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_after_sync_cancel_publishes_phase_change() {
        // The ESC-confirm path transitions the phase `Streaming → Idle` directly
        // in the shared `State` (`cancel_stream_and_drain`) without emitting a
        // bus event. When the provider's `StreamCompleted(Canceled)` then
        // arrives, the phase is already `Idle`. Subscribers still need a turn-
        // end signal, so `on_stream_completed` must force-publish a
        // `SessionPhaseChanged` — and history is complete with the
        // `Error("Cancelled")` entry by then.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            // Synchronous cancel: phase → Idle, no bus event.
            session.cancel_stream_and_drain();
            assert_eq!(session.phase(), PhaseKind::Idle);
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then a SessionPhaseChanged was published despite the Idle→Idle no-op.
        let phase_events = audit.of_type::<SessionPhaseChanged>();
        assert!(
            phase_events.iter().any(|e| e.new_phase == PhaseKind::Idle),
            "expected SessionPhaseChanged(Idle) after cancel consumed by frontend; got: {:?}",
            audit.names()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_force_excludes_dangling_tool_calls() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::assistant(""));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let history = session.history();
        assert_eq!(
            history[0].context_override(),
            crate::protocol::ContextOverride::Default
        );
        assert_eq!(
            history[1].context_override(),
            crate::protocol::ContextOverride::ForcedExclude
        );
        assert_eq!(
            history[2].context_override(),
            crate::protocol::ContextOverride::ForcedExclude
        );
        assert_eq!(
            history[3].context_override(),
            crate::protocol::ContextOverride::Default
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_token_appends_text_to_assistant_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Hello".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 1,
            token: " world".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant_text = session
            .history()
            .iter()
            .find_map(|e| match &e.kind {
                ChatEntryKind::Assistant(t) => Some(t.clone()),
                _ => None,
            })
            .expect("should have an assistant entry");
        assert_eq!(assistant_text, "Hello world");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_token_keeps_phase_as_streaming() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        {
            let mut state = actor.state.write_test_no_cap();
            state.active_session_mut().begin_streaming();
        }
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "hi".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseKind::Streaming),
            "expected Streaming phase, got {:?}",
            session.phase()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_token_corrects_sending_phase_to_streaming() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("go"));
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "response".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseKind::Streaming),
            "expected Streaming phase after correction from Sending, got {:?}",
            session.phase()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_persists_session() {
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then the session was persisted (should_save = true for Finished).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Finished"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_persists_session() {
        // Given an interacted session in streaming state.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted with Error reason.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then the session was persisted (should_save = true for Error).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_persists_session() {
        // Given an interacted session in streaming state.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted with Canceled reason.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then the session was persisted.
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Canceled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_does_not_count_tokens_on_error() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: Some("some error content".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger[0].tokens_received, 0,
            "expected 0 tokens_received on Error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_preserves_assistant_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("do something"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "I will help".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("response".to_owned()),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let has_assistant = session
            .history()
            .iter()
            .any(|e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "I will help"));
        assert!(
            has_assistant,
            "expected assistant entry 'I will help' to be preserved after ToolUse"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_counts_tool_call_arguments() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("checking".to_owned()),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls -la /very/long/path"}"#.to_owned(),
            }]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert!(
            ledger[0].tokens_received > 2,
            "expected tokens_received > 2 (text only), got {}",
            ledger[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_preserves_assistant_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "world".to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("world".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let has_world = session
            .history()
            .iter()
            .any(|e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t.contains("world")));
        assert!(
            has_world,
            "expected assistant entry with 'world' to be preserved after Finished"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_with_complete_tool_loop_does_not_exclude() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::assistant(""));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::tool_result(
                "tc-1",
                "bash",
                "file.txt",
                crate::feat::session::tool_result_status::ToolResultStatus::Success,
            ));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        for entry in session.history() {
            assert_eq!(
                entry.context_override(),
                crate::protocol::ContextOverride::Default,
                "expected Default for entry {:?}",
                entry.kind
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_without_auto_compaction_goes_to_idle() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseKind::Idle),
            "expected Idle after Finished without auto-compaction, got {:?}",
            session.phase()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_applies_pending_mutations() {
        let (actor, audit) = test_actor_recording().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let entry = ChatEntry::assistant("response");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.begin_streaming();
            session.queue_mutations(vec![
                crate::feat::session::history_mutation::HistoryMutation::SetContextOverride {
                    entry_id: entry_id.clone(),
                    value: crate::protocol::ContextOverride::ForcedExclude,
                    source: ChangeSource::Internal {
                        label: "test".into(),
                    },
                },
            ]);
            (entry_id, state.session.active_session_id().clone())
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            crate::protocol::ContextOverride::ForcedExclude
        );
        assert!(audit.contains_name("HistoryAppended"));
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_applies_pending_mutations() {
        let (actor, _audit) = test_actor_recording().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let entry = ChatEntry::assistant("partial");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.begin_streaming();
            session.queue_mutations(vec![
                crate::feat::session::history_mutation::HistoryMutation::SetContextOverride {
                    entry_id: entry_id.clone(),
                    value: crate::protocol::ContextOverride::ForcedExclude,
                    source: ChangeSource::Internal {
                        label: "test".into(),
                    },
                },
            ]);
            (entry_id, state.session.active_session_id().clone())
        };

        // When handling StreamCompleted with Error reason.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then the mutation was applied.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("assistant entry exists");
        assert_eq!(
            assistant.context_override(),
            crate::protocol::ContextOverride::ForcedExclude,
            "expected mutation to be applied at stream error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_applies_pending_mutations() {
        // Given a session in streaming state with pending mutations.
        let actor = test_actor().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let entry = ChatEntry::assistant("partial");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.begin_streaming();
            session.queue_mutations(vec![
                crate::feat::session::history_mutation::HistoryMutation::SetContextOverride {
                    entry_id: entry_id.clone(),
                    value: crate::protocol::ContextOverride::ForcedExclude,
                    source: ChangeSource::Internal {
                        label: "test".into(),
                    },
                },
            ]);
            (entry_id, state.session.active_session_id().clone())
        };

        // When handling StreamCompleted with Canceled reason.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            crate::protocol::ContextOverride::ForcedExclude
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_does_not_apply_mutations() {
        let (actor, _audit) = test_actor_recording().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let entry = ChatEntry::assistant("checking");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.begin_streaming();
            session.queue_mutations(vec![
                crate::feat::session::history_mutation::HistoryMutation::SetContextOverride {
                    entry_id: entry_id.clone(),
                    value: crate::protocol::ContextOverride::ForcedExclude,
                    source: ChangeSource::Internal {
                        label: "test".into(),
                    },
                },
            ]);
            (entry_id, state.session.active_session_id().clone())
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("response".to_owned()),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            crate::protocol::ContextOverride::Default,
            "expected mutation to NOT be applied for ToolUse reason"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_provider_tokens_used_directly() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
                model_used: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("short".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(5000),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: Some("very long thinking content here".to_owned()),
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 5000);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_local_fallback_includes_thinking() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
                model_used: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("short".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: Some("a substantial amount of reasoning text".to_owned()),
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.token_ledger()[0].tokens_received > 2,
            "expected tokens_received > 2, got {}",
            session.token_ledger()[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_local_fallback_without_thinking_backward_compat() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response text".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.token_ledger()[0].tokens_received > 0,
            "expected nonzero tokens_received for 'response text'"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_provider_tokens_preferred_over_local() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("short".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(9999),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: Some(
                "extremely long thinking content that would produce many tokens".to_owned(),
            ),
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 9999);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_takes_max_when_provider_undercounts() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling StreamCompleted with no provider tokens and no thinking.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("response text".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then tokens_received counts only the text (backward compat).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert!(
            ledger[0].tokens_received > 0,
            "expected nonzero tokens_received for 'response text', got {}",
            ledger[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_takes_max_when_provider_overcounts() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("ok".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(50000),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 50000);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_uses_local_count_when_no_provider_report() {
        use crate::feat::context::strategy::token_estimator::TokenCounter;
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let content = "hello world this is a test";
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some(content.to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let counter =
            crate::feat::context::strategy::token_estimator::TiktokenCounter::o200k_base();
        let expected = counter.count(content) as u32;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, expected);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatched_at_flows_from_stream_token_to_entry_timing() {
        // Given a session actor with a session in streaming state.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling a StreamToken with a specific dispatched_at.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Hello".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // Then the assistant entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            crate::protocol::EntryTiming::Streamed {
                dispatched_at,
                first_token_at,
                finished_at,
            } => {
                assert_eq!(dispatched_at, &dispatched);
                assert!(first_token_at.is_some());
                assert!(finished_at.is_none());
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn thinking_entry_gets_dispatched_at_from_stream_token() {
        // Given a session actor with a session in streaming state.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling a thinking StreamToken with a specific dispatched_at.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // Then the thinking entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            crate::protocol::EntryTiming::Streamed {
                dispatched_at,
                first_token_at,
                finished_at,
            } => {
                assert_eq!(dispatched_at, &dispatched);
                assert!(first_token_at.is_some());
                assert!(finished_at.is_none());
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_sets_finished_at_on_assistant_entry() {
        // Given a session actor with a session in streaming state and a token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Hello".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // When handling StreamCompleted with Finished reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("Hello".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(10),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
        };
        actor.on_stream_completed(&event).await;

        // Then the assistant entry has finished_at set.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            crate::protocol::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "finished_at should be set after completion"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn content_token_after_thinking_finalizes_thinking_entry_finished_at() {
        // Given a session actor streaming with a thinking entry already begun.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the first non-thinking (content) token arrives.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 1,
            token: "answer".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // Then the thinking entry's finished_at is set (duration resolved).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            crate::protocol::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set after content token arrives"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn second_content_token_does_not_move_thinking_finished_at() {
        // Given a session actor streaming with thinking finalized by a content token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 1,
            token: "answer".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });
        let finished_at_first = {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            let thinking = session
                .history()
                .iter()
                .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
                .expect("thinking entry");
            match &thinking.timing {
                crate::protocol::EntryTiming::Streamed { finished_at, .. } => *finished_at,
                other => panic!("expected Streamed, got {other:?}"),
            }
        };

        // When a second content token arrives.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 2,
            token: " more".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // Then the thinking entry's finished_at is unchanged (idempotent).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            crate::protocol::EntryTiming::Streamed { finished_at, .. } => {
                assert_eq!(
                    *finished_at, finished_at_first,
                    "thinking finished_at must not change on subsequent content tokens"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn pure_reasoning_stream_finalizes_thinking_on_stream_completion() {
        // Given a session actor streaming with ONLY thinking tokens (no content).
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "only reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the stream completes without producing a content token.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(10),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: Some("only reasoning".to_owned()),
            dispatched_at: dispatched,
            model_used: None,
        };
        actor.on_stream_completed(&event).await;

        // Then the thinking entry's finished_at is set via the safety net.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            crate::protocol::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set by safety net on pure-reasoning completion"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_during_reasoning_finalizes_thinking_and_preserves_text() {
        // Given a session actor streaming with a thinking entry.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "partial reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the stream is canceled before any content token.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
        };
        actor.on_stream_completed(&event).await;

        // Then the thinking entry's finished_at is set AND its text is preserved.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match (&thinking.kind, &thinking.timing) {
            (
                crate::protocol::ChatEntryKind::Thinking(text),
                crate::protocol::EntryTiming::Streamed { finished_at, .. },
            ) => {
                assert_eq!(
                    text, "partial reasoning",
                    "partial reasoning text preserved"
                );
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set on cancel during reasoning"
                );
            }
            other => panic!("expected Thinking + Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancelled_stream_records_finished_at() {
        // Given a session actor with a session in streaming state and a token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Partial".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // When handling StreamCompleted with Canceled reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
        };
        actor.on_stream_completed(&event).await;

        // Then the assistant entry has finished_at set (cancellation is a finish event).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            crate::protocol::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "finished_at should be set even on cancellation"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    /// Deterministic counter for unit testing - counts characters.
    struct CharCounter;
    impl crate::feat::context::strategy::token_estimator::TokenCounter for CharCounter {
        fn count(&self, text: &str) -> usize {
            text.chars().count()
        }
        fn name(&self) -> &'static str {
            "char"
        }
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_counts_assistant_content() {
        // Given a char-counting counter and assistant content only.
        let counter = CharCounter;

        // When counting locally.
        let total = super::count_tokens_locally(&counter, "hello", "", None);

        // Then the total equals the assistant content length.
        assert_eq!(total, 5);
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_adds_thinking_content() {
        // Given a char-counting counter with content and thinking text.
        let counter = CharCounter;

        // When counting locally.
        let total = super::count_tokens_locally(&counter, "abc", "de", None);

        // Then the total is the sum of content and thinking.
        assert_eq!(total, 5);
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_includes_tool_call_arguments_and_names() {
        // Given a char-counting counter, content, and one tool call.
        let counter = CharCounter;
        let tool_calls = vec![jinn_core_types::tool_types::ToolCall {
            id: "tc-1".to_owned(),
            name: "bash".to_owned(),
            arguments: "ls".to_owned(),
        }];

        // When counting locally (content "ab"=2 + name "bash"=4 + args "ls"=2).
        let total = super::count_tokens_locally(&counter, "ab", "", Some(&tool_calls));

        // Then tool call arguments and names are included.
        assert_eq!(total, 8);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_none_for_empty() {
        // Given an empty drained queue.
        let queue = std::collections::VecDeque::new();

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then no text is produced.
        assert_eq!(text, None);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_none_when_only_tool_continuation() {
        // Given a queue with only a tool continuation.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(crate::feat::session::queue_item::QueueItem::ToolContinuation);

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then no text is produced.
        assert_eq!(text, None);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_text_for_single_user_message() {
        // Given a queue with one user message.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(crate::feat::session::queue_item::QueueItem::UserMessage(
            Box::new(ChatEntry::user("hello world")),
        ));

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then the user message text is produced.
        assert_eq!(text.as_deref(), Some("hello world"));
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_joins_multiple_user_messages_with_newline() {
        // Given a queue with two user messages.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(crate::feat::session::queue_item::QueueItem::UserMessage(
            Box::new(ChatEntry::user("first")),
        ));
        queue.push_back(crate::feat::session::queue_item::QueueItem::UserMessage(
            Box::new(ChatEntry::user("second")),
        ));

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then the messages are joined with a newline.
        assert_eq!(text.as_deref(), Some("first\nsecond"));
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_skips_tool_continuation_when_mixed() {
        // Given a queue with a user message and a tool continuation.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(crate::feat::session::queue_item::QueueItem::UserMessage(
            Box::new(ChatEntry::user("only user")),
        ));
        queue.push_back(crate::feat::session::queue_item::QueueItem::ToolContinuation);

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then only the user message text is produced.
        assert_eq!(text.as_deref(), Some("only user"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_appends_annotation_entry() {
        // Given a recording session actor.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id: session_id.clone(),
            citations: vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }],
        };

        // When handling CitationsReceived.
        actor.on_citations_received(&event).await;

        // Then the session has one Annotation entry carrying the citation.
        let state = actor.state.read();
        let annotations: Vec<_> = state
            .session
            .active_session()
            .history()
            .iter()
            .filter(|e| matches!(e.kind, ChatEntryKind::Annotation { .. }))
            .collect();
        assert_eq!(
            annotations.len(),
            1,
            "expected exactly one annotation entry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_emits_history_appended() {
        // Given a recording session actor.
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id,
            citations: vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }],
        };

        // When handling CitationsReceived.
        actor.on_citations_received(&event).await;

        // Then HistoryAppended was broadcast.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended after citations received"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_empty_citations_creates_nothing() {
        // Given a recording session actor.
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id,
            citations: Vec::new(),
        };

        // When handling CitationsReceived with empty citations.
        actor.on_citations_received(&event).await;

        // Then no annotation entry was added.
        let count = actor
            .state
            .read()
            .session
            .active_session()
            .history()
            .iter()
            .filter(|e| matches!(e.kind, ChatEntryKind::Annotation { .. }))
            .count();
        assert_eq!(count, 0, "empty citations must create no entry");
        // And no HistoryAppended was broadcast.
        assert!(
            !audit.contains_name("HistoryAppended"),
            "empty citations must not emit HistoryAppended"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stale_generation_stream_completed_is_dropped() {
        // Given a streaming session with a current-generation dispatch timestamp.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            let now = jiff::Timestamp::now();
            session.core.ephemeral.stream_dispatched_at = Some(now);
            state.session.active_session_id().clone()
        };

        // When a StreamCompleted arrives carrying an OLDER dispatched_at
        // (simulating an aborted prior stream's late terminal event).
        let backdated = jiff::Timestamp::now()
            .checked_sub(jiff::Span::new().seconds(30))
            .unwrap();
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: backdated,
        };
        actor.on_stream_completed(&event).await;

        // Then the session is STILL Streaming (the stale event was dropped)
        // and the generation guard was not consumed.
        let guard = actor.state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseKind::Streaming),
            "stale-generation StreamCompleted must not transition the session"
        );
        assert!(
            session.core.ephemeral.stream_dispatched_at.is_some(),
            "generation guard must remain set for stale events"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tooluse_without_buffered_batch_does_not_dispatch() {
        // Given a session mid-stream with no buffered tool batch.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When a ToolUse stream completes with no pending batch buffered.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("let me check".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        // Then no SendToLlmProvider is emitted — the continuation must wait
        // for the ExecuteToolBatch round-trip.
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "ToolUse completion without a buffered batch must not dispatch a continuation"
        );
    }
}
