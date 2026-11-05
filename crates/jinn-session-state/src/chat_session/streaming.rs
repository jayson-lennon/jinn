//! Streaming lifecycle: arm/dispatch generation, token appends, settle, retry, cancel, finalize.

use super::*;

impl ChatSessionState {
    /// Lazily create the Assistant entry for the current stream.
    ///
    /// Called on first `append_stream_token`, `finish_streaming`,
    /// `begin_tool_call`, or `cancel_streaming`. No-op if the entry
    /// already exists or the session is not streaming.
    pub(super) fn ensure_assistant_entry(&mut self, dispatched_at: jiff::Timestamp) {
        if self.core.ephemeral.streaming_entry_index.is_some()
            || self.core.ephemeral.phase != PhaseView::Streaming
        {
            return;
        }
        let mut entry = ChatEntry::assistant("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        let index = self.push_entry(entry);
        self.core.ephemeral.streaming_entry_index = Some(index);
    }

    pub(super) fn finish_streaming_entry(&mut self, idx: usize) {
        self.edit_history()
            .with_entry_at_mut(idx, |entry| entry.timing.finish());
    }

    /// Finalize the streaming thinking entry's timing, if it hasn't already been.
    ///
    /// Guarded to be a no-op if `finished_at` is already set, so safety-net
    /// calls from `finish_streaming`/`cancel_streaming` don't move an
    /// already-recorded timestamp.
    pub fn finish_thinking_entry(&mut self, idx: usize) {
        self.edit_history().with_entry_at_mut(idx, |entry| {
            if entry.timing.finished_at().is_none() {
                entry.timing.finish();
            }
        });
    }

    /// Mark a provider stream as begun (`Streaming`).
    ///
    /// A display projection: the turn ledger's mint paths call this after
    /// admission, and nothing reads it for admission. Setting the phase
    /// cannot fail — there is no validated machine left to refuse it — so
    /// tests that seed a busy session by hand get exactly what production's
    /// admitted mint would have written.
    pub fn begin_streaming(&mut self) {
        self.core.ephemeral.phase = PhaseView::Streaming;
        self.core.identity.last_history_activity_at = Timestamp::now();
    }

    /// Append a token to the streaming assistant entry.
    ///
    /// Lazily creates the Assistant entry if this is the first token.
    ///
    /// Soft guard: if the session is not streaming, logs a warning and returns an error.
    ///
    /// # Errors
    ///
    /// Returns `Err(StreamingError::NoStreamingEntry)` if the session is not in Streaming phase.
    pub fn append_stream_token<S>(
        &mut self,
        token: S,
        dispatched_at: jiff::Timestamp,
    ) -> Result<(), StreamingError>
    where
        S: AsRef<str>,
    {
        if self.core.ephemeral.phase != PhaseView::Streaming {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.phase,
                "append_stream_token called while not streaming - ignoring"
            );
            return Err(StreamingError::NoStreamingEntry);
        }
        self.ensure_assistant_entry(dispatched_at);
        self.core.identity.last_history_activity_at = Timestamp::now();
        self.core.identity.last_provider_activity_at = Timestamp::now();
        let index = self
            .core
            .ephemeral
            .streaming_entry_index
            .ok_or(StreamingError::NoStreamingEntry)?;
        if let Some(entry) = self.edit_history().with_entry_at_mut(index, |entry| {
            if let ChatEntryKind::Assistant(text) = &mut entry.kind {
                text.push_str(token.as_ref());
                true
            } else {
                false
            }
        }) && entry
        {
            Ok(())
        } else {
            Err(StreamingError::NotAssistantEntry)
        }
    }

    /// Begin accumulating thinking tokens.
    ///
    /// Appends an empty `Thinking` entry to the history. The Assistant entry
    /// is created lazily later (on first `append_stream_token` or `finish_streaming`),
    /// so entries naturally appear in order: thinking before assistant.
    ///
    /// Soft guard: if the session is not streaming or thinking has already begun,
    /// logs a warning and returns without changing state.
    pub fn begin_thinking(&mut self, dispatched_at: jiff::Timestamp) {
        if self.core.ephemeral.phase != PhaseView::Streaming {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.phase,
                "begin_thinking called while not streaming - ignoring"
            );
            return;
        }
        if self.core.ephemeral.streaming_thinking_entry_index.is_some() {
            tracing::warn!("begin_thinking called while already thinking - ignoring");
            return;
        }
        let mut entry = ChatEntry::thinking("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        // Insert thinking BEFORE the assistant entry when the assistant entry
        // already exists. Some providers (OpenRouter) send reasoning tokens
        // AFTER content tokens, so the assistant entry is already in history.
        // The thinking entry should appear before it in the chat log.
        let index = if let Some(assistant_idx) = self.core.ephemeral.streaming_entry_index {
            self.insert_entry_at(assistant_idx, entry)
        } else {
            self.push_entry(entry)
        };
        self.core.ephemeral.streaming_thinking_entry_index = Some(index);
    }

    /// Append a thinking token to the streaming Thinking entry.
    ///
    /// # Panics
    ///
    /// Panics if `begin_thinking()` has not been called.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the session is not in a valid streaming state.
    pub fn append_thinking_token<S>(&mut self, token: S) -> Result<(), StreamingError>
    where
        S: AsRef<str>,
    {
        let index = self
            .core
            .ephemeral
            .streaming_thinking_entry_index
            .ok_or(StreamingError::NoThinkingEntry)?;
        self.core.identity.last_history_activity_at = Timestamp::now();
        self.core.identity.last_provider_activity_at = Timestamp::now();
        if let Some(true) = self.edit_history().with_entry_at_mut(index, |entry| {
            if let ChatEntryKind::Thinking(text) = &mut entry.kind {
                text.push_str(token.as_ref());
                true
            } else {
                false
            }
        }) {
            return Ok(());
        }
        // Original behavior: a non-thinking entry at the index is ignored.
        Ok(())
    }

    /// The index of the streaming thinking entry, if thinking is being accumulated.
    pub fn streaming_thinking_entry_index(&self) -> Option<usize> {
        self.core.ephemeral.streaming_thinking_entry_index
    }

    /// The index of the entry accumulating assistant content, if any.
    ///
    /// The renderer needs this to recognise the one entry whose content
    /// changes on every streamed token, as distinct from settled history.
    #[must_use]
    pub fn streaming_entry_index(&self) -> Option<usize> {
        self.core.ephemeral.streaming_entry_index
    }

    /// Settle the session to `Idle` from any phase.
    ///
    /// The display projection of a turn settlement: the turn ledger's
    /// settle paths call this after adjudicating the turn's end. Every
    /// registration the turn owned is dropped here with the phase — a
    /// settled session has no streaming indices and no tool tracking.
    pub fn settle_to_idle(&mut self) {
        self.core.ephemeral.phase = PhaseView::Idle;
        self.core.ephemeral.clear_streaming_indices();
    }

    /// Rewind for a re-dispatch of the live turn: `Streaming` → `Working`.
    ///
    /// Called by the stall-retry and rule-intercept paths, whose resumed
    /// dispatch re-enters `Streaming` through the ledger's mint. The
    /// streaming indices are dropped here, so the retried stream starts
    /// from a clean slate; the caller must have consumed them first.
    pub fn rewind_for_retry(&mut self) {
        self.core.ephemeral.phase = PhaseView::Working;
        self.core.ephemeral.streaming_entry_index = None;
        self.core.ephemeral.streaming_thinking_entry_index = None;
        self.core.ephemeral.clear_streaming_indices();
    }

    /// Sweeps every still-`Pending` tool result to `Failure`.
    ///
    /// The cancel drain's last step: a result whose call never completed
    /// (a straggler the synthesized completions missed, or one refused by
    /// the ledger) must not survive as `Pending` — an unresolved batch
    /// makes the context converter strip the whole assistant tool-call
    /// block (bug #21). Clearing the tracking indices keeps the machine
    /// consistent with the history it just settled.
    pub fn sweep_pending_tool_results_to_failure(&mut self) {
        let swept = self.edit_history().sweep_pending_tool_results_to_failure();
        self.core.ephemeral.streaming_tool_result_indices.clear();
        if swept > 0 {
            tracing::info!(
                swept,
                "swept pending tool results to Failure at cancel settle"
            );
        }
    }

    /// Finalize the streaming entries ahead of a normal (non-cancel)
    /// completion: ensure the assistant entry exists, stamp its
    /// `finished_at`, and resolve any still-pending thinking entry.
    ///
    /// The entry half of [`Self::settle_to_idle`], run by
    /// the session actor's completion fold before the phase actor applies
    /// the machine edge.
    ///
    /// Exclusively called by the session actor's completion fold.
    pub fn finalize_entries_for_finish(
        &mut self,
        preserve_assistant: bool,
        dispatched_at: jiff::Timestamp,
    ) {
        if preserve_assistant {
            self.ensure_assistant_entry(dispatched_at);
        }
        if let Some(idx) = self.core.ephemeral.streaming_entry_index {
            self.finish_streaming_entry(idx);
        }
        if let Some(idx) = self.core.ephemeral.streaming_thinking_entry_index {
            self.finish_thinking_entry(idx);
        }
    }

    /// Ends one streaming round within a live turn: the assistant and
    /// thinking entries are finalized, so their indices must not survive
    /// into the next round.
    ///
    /// Called by the session actor's completion fold after the entries are
    /// finalized and their models overridden. The old phase machine got
    /// this for free — the `Streaming` phase struct carrying the indices
    /// was dropped at the tool-use edge, so the next round lazily created
    /// fresh entries below the tool calls. The projection write that
    /// replaced the machine has no struct to drop, so the round boundary
    /// clears them explicitly. Keeping them would append the next round's
    /// tokens into the previous round's assistant entry, stacking every
    /// round's tool calls and results below a single stale text block.
    ///
    /// The tool-call and tool-result tracking maps are untouched: they are
    /// turn-scoped, and the batch the stream handed off to is still live.
    pub fn finish_streaming_round(&mut self) {
        self.core.ephemeral.streaming_entry_index = None;
        self.core.ephemeral.streaming_thinking_entry_index = None;
    }

    /// Cancel streaming but keep partial text in history.
    ///
    /// The entry half of a cancel: finalize the partial assistant entry
    /// and any still-pending thinking entry so their durations resolve.
    /// The machine edge is the phase actor's
    /// [`Self::settle_to_idle`], applied after this.
    ///
    /// Exclusively called by the session actor's completion fold.
    pub fn finalize_entries_for_cancel(&mut self, dispatched_at: jiff::Timestamp) {
        self.ensure_assistant_entry(dispatched_at);

        // Set finished_at on the assistant entry.
        if let Some(idx) = self.core.ephemeral.streaming_entry_index {
            self.finish_streaming_entry(idx);
        }

        // Safety net: finalize any still-pending thinking entry so its duration
        // resolves even if reasoning was interrupted. Must run before cancel()
        // drops the StreamingPhase and clears streaming_thinking_entry_index().
        if let Some(idx) = self.core.ephemeral.streaming_thinking_entry_index {
            self.finish_thinking_entry(idx);
        }
    }

    /// Sets the response-reported model on every live entry of the current
    /// generation, replacing only `model_used`.
    ///
    /// `model_requested` is never touched: it is the request-time fact and
    /// the whole point of the two fields is that they can differ. Live
    /// entries are the ones the machine's indices still name — the
    /// streaming assistant entry, the thinking entry, and every in-flight
    /// tool call. Entries created before the stream started (the user's
    /// message) are not provider-produced and are not stamped.
    ///
    /// Callers must invoke this while the indices are intact: the fold
    /// runs before the phase actor's settle drops the `StreamingPhase`.
    pub fn override_live_entry_models(&mut self, model: Option<&str>) {
        let indices = {
            let eph = &self.core.ephemeral;
            let mut all = Vec::new();
            if let Some(idx) = eph.streaming_entry_index {
                all.push(idx);
            }
            if let Some(idx) = eph.streaming_thinking_entry_index {
                all.push(idx);
            }
            all.extend(eph.streaming_tool_call_indices.values().copied());
            all
        };
        for idx in indices {
            self.edit_history().with_entry_at_mut(idx, |entry| {
                entry.model_used = model.map(str::to_owned);
            });
        }
    }

    /// Cancel streaming and drain steering fragments plus queued messages back
    /// into the input buffer.
    ///
    /// Used when the user interrupts via ESC-confirm during an active stream.
    /// Steering fragments (drained first) and the display text of each queued
    /// `UserMessage` are joined with `"\n\n---\n\n"` and replace whatever was
    /// in the input box. `ToolContinuation` items are silently discarded.
    /// If nothing was drained, the input box is left untouched.
    ///
    /// The drain half only: the phase edge is the phase actor's, applied
    /// when the cancel's `TurnCanceled` command lands.
    pub fn drain_cancelled_work_to_input(&mut self) {
        let drained_text = self.drain_cancel_chunks().join("\n\n---\n\n");
        if !drained_text.is_empty() {
            self.update_input(|input| input.replace_all(drained_text));
        }
    }

    /// Mark the session as preparing its next request (`Working`).
    ///
    /// The display projection of the tool-use edge: the tool loop has the
    /// stream retired and the calls executing, and the continuation dispatch
    /// re-enters `Streaming` through the ledger's mint.
    pub fn begin_sending(&mut self) {
        self.core.ephemeral.phase = PhaseView::Working;
        self.core.identity.last_history_activity_at = Timestamp::now();
    }
}

////// The tool-result content for a call a stream rule interrupted mid-arguments.
///
/// The content is what the model reads instead of running the call, so it has
/// to say two things: that the call did not happen, and what it was trying to
/// do. The arguments ride verbatim and are labelled truncated — completing them
/// would invent a call the model never made, and the whole point is to show
/// the model the mistake it is about to repeat.
///
/// The rule is named only as a user-defined rule, never by name: the name
/// crosses no boundary, and a generic framing keeps the result about the call
/// rather than about jinn's configuration.
///
/// The fragment lives in the result's content rather than the call's arguments
/// deliberately. Arguments are serialized arguments that every provider parses
/// as JSON, and a truncated fragment is not JSON — Google's encoder degrades
/// one to `{}` silently, which would leave the model a call that does nothing
/// and no explanation of why. Result content is free text on all three
/// providers, so this text reaches the model intact everywhere.
pub(super) fn interrupted_call_result(tool_name: &str, arguments: &str) -> String {
    format!(
        "This `{tool_name}` call did not run. A user-defined rule triggered \
     and stopped the response before the call was sent.\n\n\
     The tool call fragment provided (do not re-run as is), truncated where it was interrupted:\n\n\
     {arguments}"
    )
}

/// Stamps an entry with the dispatching generation's requested model.
///
/// Both fields get the same value: `model_requested` is the generation's
/// request-time fact and is never overridden, while `model_used` starts
/// here and is later replaced by the response-reported model when the
/// stream completes normally. Tool results are deliberately unstamped —
/// they are produced locally by the tool executor after the stream ended;
/// the audit popup derives their model from the paired tool call instead.
pub(super) fn stamp_entry_with_generation_model(entry: &mut ChatEntry, model: Option<&str>) {
    entry.model_used = model.map(str::to_owned);
    entry.model_requested = model.map(str::to_owned);
}

/// Error returned when a streaming operation fails.
#[derive(Debug, wherror::Error)]
pub enum StreamingError {
    /// No streaming entry index is set.
    #[error("no streaming entry index")]
    NoStreamingEntry,
    /// The streaming entry has an unexpected kind.
    #[error("streaming entry is not an Assistant entry")]
    NotAssistantEntry,
    /// No thinking entry index is set.
    #[error("no thinking entry index")]
    NoThinkingEntry,
    /// No tool call tracked for the given stream index.
    #[error("no entry tracked for tool call stream index {index}")]
    NoToolCallIndex { index: usize },
    /// The token ledger is empty.
    #[error("token ledger is empty")]
    EmptyLedger,
}

impl ChatSessionState {
    /// Arm the current inference-stream generation.
    pub fn arm_stream(&mut self, dispatched_at: Timestamp) {
        self.core.ephemeral.stream_dispatched_at = Some(dispatched_at);
    }

    /// Record the model requested for the current inference-stream generation.
    ///
    /// An empty string (the no-provider sentinel `accum.model_id`) is
    /// normalized to `None` so no entry carries an empty model id. A
    /// dispatch without a receipt always leaves the previous generation's
    /// value in place — overwrite-on-dispatch is the mechanism that keeps
    /// the stamp with its generation.
    pub fn set_stream_request_model(&mut self, model: Option<String>) {
        let normalized = model.filter(|m| !m.is_empty());
        self.core.ephemeral.stream_request_model = normalized;
    }

    /// Return the model requested for the active inference-stream generation.
    #[must_use]
    pub fn stream_request_model(&self) -> Option<String> {
        self.core.ephemeral.stream_request_model.clone()
    }

    /// Drop the requested-model stamp along with the generation it belongs to.
    pub fn clear_stream_request_model(&mut self) {
        self.core.ephemeral.stream_request_model = None;
    }

    /// Return the active inference-stream generation marker.
    #[must_use]
    pub fn stream_dispatched_at(&self) -> Option<Timestamp> {
        self.core.ephemeral.stream_dispatched_at
    }

    /// Consume the active inference-stream generation marker.
    pub fn clear_stream_generation(&mut self) {
        self.core.ephemeral.stream_dispatched_at = None;
    }

    /// Return whether an inference request is currently in flight.
    #[must_use]
    pub fn has_in_flight_stream(&self) -> bool {
        self.core.ephemeral.stream_dispatched_at.is_some()
    }
}
