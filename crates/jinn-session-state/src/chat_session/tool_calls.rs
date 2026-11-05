//! Tool call and result streaming: begin, append, finalize, and interrupted-call accounting.

use super::streaming::{interrupted_call_result, stamp_entry_with_generation_model};
use super::*;

impl ChatSessionState {
    /// The tool calls whose arguments are still streaming, as `(tool call id,
    /// tool name, arguments accumulated so far)`.
    ///
    /// The argument text is whatever the model had produced when this was
    /// read, which mid-stream is a prefix of the real JSON and usually not
    /// parseable. It is reported verbatim for exactly that reason: a fragment
    /// is worth showing to the model unaltered, where repairing it would
    /// invent content the model never wrote.
    ///
    /// Read from the streaming phase's index map, so this is only meaningful
    /// while a response is in flight — and only for calls whose arguments had
    /// started arriving. A model that opens with a tool call and is
    /// interrupted before its first delta appears here.
    #[must_use]
    pub fn streaming_tool_call_fragments(&self) -> Vec<(ToolCallId, String, String)> {
        let history = &self.core.history_work.history;
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .values()
            .filter_map(|&index| history.get(index))
            .filter_map(|entry| match &entry.kind {
                ChatEntryKind::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect()
    }

    /// Complete the tool calls a stream rule interrupted, each paired with a
    /// synthetic result explaining that it never ran.
    ///
    /// An intercepted tool call is a call the model made and never got an
    /// answer for, because interrupting its argument stream means it is never
    /// dispatched. Leaving it unanswered is not a neutral outcome: providers
    /// reject a request whose `tool_calls` have no matching results, and
    /// force-excluding it leaves the model resuming with no memory of an
    /// attempt it will simply repeat. The synthetic result is what makes the
    /// call both valid and legible.
    ///
    /// The fragment is placed immediately after its call and *before* the
    /// interrupt's guidance entry, so the model reads the failure first and
    /// the advice second. Guidance is treated as skippable rather than
    /// terminating for the same reason it is not a provider message: it sits
    /// between a call and its result without breaking the pairing.
    ///
    /// Returns the number of calls completed. Zero means the intercept caught
    /// prose or reasoning rather than a tool call, which is the common case
    /// and needs nothing here.
    pub fn explain_interrupted_tool_calls(&mut self) -> usize {
        let fragments = self.streaming_tool_call_fragments();
        if fragments.is_empty() {
            return 0;
        }

        let history = self.core.history_work.history.clone();
        // Collected first and inserted back-to-front, so each insertion's
        // index stays valid: inserting in place would renumber the calls that
        // have not been paired yet.
        let mut insertions: Vec<(usize, ChatEntry)> = Vec::with_capacity(fragments.len());
        for (id, name, arguments) in fragments {
            let Some(call_index) = history.iter().position(
                |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id: t, .. } if t == &id),
            ) else {
                continue;
            };
            insertions.push((
                call_index + 1,
                ChatEntry::tool_result(
                    id.clone(),
                    name.clone(),
                    interrupted_call_result(&name, &arguments),
                    ToolResultStatus::Failure,
                ),
            ));
        }

        let completed = insertions.len();
        for (index, entry) in insertions.into_iter().rev() {
            self.insert_entry_at(index, entry);
        }
        completed
    }

    /// Prepare a stalled stream for retry: take the partial entries out of
    /// context and clear the streaming bookkeeping so the retried generation
    /// starts from scratch.
    ///
    /// A call a stream rule interrupted is deliberately left *in* context when
    /// it has been paired with a synthetic result: that pairing is what makes
    /// the resumed request valid, and taking the call out of context would
    /// leave an answered-but-absent call the model cannot read. Excluding it
    /// was the old behaviour and it is what made a model resume blind and
    /// re-emit the identical call.
    ///
    /// Nothing is removed. A stalled attempt's partial assistant text, thinking
    /// and tool calls stay in history exactly where the user watched them
    /// appear, marked `ForcedExclude` so the retried prompt stays valid —
    /// providers reject a request whose `tool_calls` have no matching results.
    /// This mirrors what the hard-cancel path already does with
    /// [`Self::force_exclude_dangling_tool_calls`].
    ///
    /// Must run while still in `Streaming`: it reads the streaming indices,
    /// which no longer exist once the phase has moved on.
    ///
    /// The excluded entries are also registered as an expanded ignored block.
    /// Exclusion alone is not enough to keep them readable: the chat log
    /// collapses any contiguous run of `!is_in_context()` entries that is at
    /// least `min_collapse_count` long and further than `proximity_count` from
    /// the tail, and it cannot tell a discarded stall from a block the user
    /// chose to ignore. Without this the whole attempt — three entries in the
    /// common case, exactly the collapse threshold — reduces to a single "N
    /// hidden entries" line, which is the invisibility this method exists to
    /// prevent. Registering the ids is a default-expanded state, not a lock:
    /// toggling the block collapses it like any other.
    ///
    /// Returns the ids whose context override changed.
    pub fn reset_streaming_entries_for_retry(&mut self) -> Vec<ChatEntryId> {
        // Collect first: clearing the indices below is what erases the only
        // record of which entries this generation owned.
        let mut indices = self.collect_streaming_history_indices();
        indices.sort_unstable();
        indices.dedup();
        // A tool call answered by a synthetic result is a finished exchange,
        // not an abandoned attempt: it and its result stay in context so the
        // model can read them. Dropping the call would strand an answered
        // call the model never sees, which is the blind resume this whole
        // method exists to avoid.
        //
        // The assistant entry the model was streaming when it made that call
        // is part of the same exchange and is kept with it. Retaining the call
        // but not its host reads to the model as a tool call that appeared out
        // of nowhere, with no preamble to say what it was doing — and the
        // request assembler manufactures an *empty* assistant to carry the call
        // in that case, so the model's own words are dropped while the shape
        // looks fine. An interrupted response is still an unfinished sentence,
        // but the alternative is sending the model a call with no context for
        // it, which is the failure this exclusion exists to prevent.
        //
        // Everything else streaming is still a partial attempt and is taken
        // out: reasoning, and prose from an attempt that made no call at all.
        let answered = self.tool_calls_answered_in_context();
        let carriers = self.entries_hosting_retained_calls(&answered);
        indices.retain(|index| {
            let entry = self.core.history_work.history.get(*index);
            !entry.is_some_and(|entry| answered.contains(&entry.id) || carriers.contains(&entry.id))
        });
        let excluded = self.edit_history().force_exclude_at_indices(&indices);
        if !excluded.is_empty() {
            self.update_view(|v| v.shown_ignored_blocks.extend(excluded.iter().cloned()));
        }
        self.core.ephemeral.clear_streaming_indices();
        excluded
    }

    /// The assistant entries whose adjacent tool calls are in `retained`.
    ///
    /// Walks back from each retained call to the nearest preceding assistant
    /// entry, which is the one it renders into: the request assembler attaches
    /// a call to the most recent assistant message, and synthesizes an empty
    /// one when there is none. Scanning backwards and stopping at the first
    /// assistant mirrors that exactly, so what is kept here is the same entry
    /// that would have carried the call.
    fn entries_hosting_retained_calls(
        &self,
        retained: &HashSet<ChatEntryId>,
    ) -> HashSet<ChatEntryId> {
        let history = &self.core.history_work.history;
        let mut hosts = HashSet::new();
        for (index, entry) in history.iter().enumerate() {
            let ChatEntryKind::ToolCall { .. } = &entry.kind else {
                continue;
            };
            if !retained.contains(&entry.id) {
                continue;
            }
            let host = history
                .get(..index)
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|candidate| matches!(candidate.kind, ChatEntryKind::Assistant(_)));
            if let Some(host) = host {
                hosts.insert(host.id.clone());
            }
        }
        hosts
    }

    /// The ids of tool calls already answered by a result that is itself in
    /// context.
    ///
    /// The pairing is what makes a call a finished exchange, so it is the
    /// test: a call with an answered result belongs in the model's context
    /// even when the attempt that produced it was abandoned mid-stream.
    fn tool_calls_answered_in_context(&self) -> HashSet<ChatEntryId> {
        let history = &self.core.history_work.history;
        let answered: HashSet<&str> = history
            .iter()
            .filter(|entry| entry.is_in_context())
            .filter_map(|entry| match &entry.kind {
                ChatEntryKind::ToolResult { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        history
            .iter()
            .filter(|entry| match &entry.kind {
                ChatEntryKind::ToolCall { id, .. } => answered.contains(id.as_str()),
                _ => false,
            })
            .map(|entry| entry.id.clone())
            .collect()
    }

    /// Every history entry index currently tracked by the streaming phase.
    fn collect_streaming_history_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        if let Some(i) = self.core.ephemeral.streaming_entry_index {
            indices.push(i);
        }
        if let Some(i) = self.core.ephemeral.streaming_thinking_entry_index {
            indices.push(i);
        }
        indices.extend(
            self.core
                .ephemeral
                .streaming_tool_call_indices
                .values()
                .copied(),
        );
        indices.extend(
            self.core
                .ephemeral
                .streaming_tool_result_indices
                .values()
                .copied(),
        );
        indices
    }

    /// Collects the text chunks drained out of the steering buffer and turn
    /// queue, in cancel-recovery order.
    ///
    /// Steering fragments come first, followed by the display text of each
    /// queued `UserMessage`. `ToolContinuation` items are discarded. The
    /// caller applies whatever separator it wants.
    pub(super) fn drain_cancel_chunks(&mut self) -> Vec<String> {
        let steering = self.steering_buffer_mut().drain_fragments();
        let queue = self.message_queue_mut().drain();
        steering
            .into_iter()
            .chain(queue.into_iter().filter_map(|item| match item {
                jinn_turn_dispatch_msg::QueueItem::UserMessage(entry) => match &entry.kind {
                    ChatEntryKind::User { display, .. } => Some(display.clone()),
                    _ => None,
                },
                jinn_turn_dispatch_msg::QueueItem::ToolContinuation => None,
            }))
            .collect()
    }

    /// Create a placeholder `ToolCall` entry and record its history index.
    ///
    /// Called when `ToolUseStarted` arrives - the tool name is known but arguments
    /// are still streaming in.
    ///
    /// Registration happens in whichever busy phase is current, not only in
    /// `Streaming`: a model that opens by calling a tool emits no text token, so
    /// it never reaches `Streaming` and its entire argument stream would
    /// otherwise be discarded.
    pub fn begin_tool_call(
        &mut self,
        index: usize,
        id: &ToolCallId,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        self.ensure_assistant_entry(dispatched_at);
        self.core.identity.last_provider_activity_at = Timestamp::now();
        // Bail out *before* pushing the entry. A registration that cannot
        // succeed must not leave an orphaned entry behind: it would render as a
        // bare tool name for the rest of the stream, and every later delta
        // addressed to it would be refused.
        if self.core.ephemeral.phase == PhaseView::Idle {
            tracing::warn!(
                index,
                "begin_tool_call called with no turn in flight - ignoring"
            );
            return;
        }
        let mut entry = ChatEntry::tool_call(id.clone(), name, "");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        let history_index = self.push_entry(entry);
        // The tracking map is turn-scoped and always present, so a
        // registration made here survives every later phase swap — a prose
        // token arriving mid-arguments no longer discards it.
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .insert(index, history_index);
    }

    /// Append an incremental delta to a streaming tool call's arguments.
    ///
    /// `partial_json` is appended to the existing arguments string - it is *not*
    /// the accumulated total.
    ///
    /// # Panics
    ///
    /// Returns an error if no tool call entry is tracked for the given stream index.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the streaming state is invalid or the index is out of bounds.
    pub fn append_tool_call_delta(
        &mut self,
        index: usize,
        partial_json: &str,
    ) -> Result<(), StreamingError> {
        let history_index = self
            .core
            .ephemeral
            .streaming_tool_call_indices
            .get(&index)
            .copied()
            .ok_or(StreamingError::NoToolCallIndex { index })?;
        if let Some(()) = self
            .edit_history()
            .with_entry_at_mut(history_index, |entry| {
                if let ChatEntryKind::ToolCall {
                    ref mut arguments, ..
                } = entry.kind
                {
                    arguments.push_str(partial_json);
                }
            })
        {
            self.core.identity.last_provider_activity_at = Timestamp::now();
        }
        Ok(())
    }

    /// Overwrite a tool call entry with the final complete arguments.
    ///
    /// Searches recent history for a `ToolCall` entry matching the given ID.
    /// If not found (shouldn't happen in normal flow), pushes a new entry.
    /// Preserves any existing `child_session` link on the entry — only the
    /// streamed-partial fields are finalized here.
    pub fn finalize_tool_call(&mut self, id: &ToolCallId, name: &str, arguments: &str) {
        let finalized = self
            .edit_history()
            .with_last_matching_mut(
                |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { .. }),
                |entry| {
                    let child_session = match &entry.kind {
                        ChatEntryKind::ToolCall { child_session, .. } => child_session.clone(),
                        _ => None,
                    };
                    entry.kind = ChatEntryKind::ToolCall {
                        id: id.clone(),
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                        child_session,
                    };
                    entry.timing.finish();
                },
            )
            .is_some();
        if finalized {
            self.core.identity.last_provider_activity_at = Timestamp::now();
        } else {
            // If not found (shouldn't happen), push a new entry.
            self.push_entry(ChatEntry::tool_call(id.clone(), name, arguments));
        }
    }

    /// Create a pending ToolResult entry when a streaming tool starts executing.
    ///
    /// Creates the entry with `ToolResultStatus::Pending` and empty content,
    /// then tracks its history index for later content appends.
    ///
    /// Accepted in either busy phase. A tool result reaches the session in
    /// `Sending` for the ordinary case — the stream ended in `ToolUse`, the
    /// batch ran, and the tools report back while the next dispatch is being
    /// prepared — and in `Streaming` when a batch overlaps a live stream.
    /// Gating on `Streaming` alone dropped every result, and the finalized
    /// result was then pushed detached at the end of history. `Idle` is still
    /// refused so a canceled session does not accumulate orphan entries.
    pub fn begin_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        // No phase guard here: admission is the turn ledger's decision, and
        // this method is only reached through an admitted path. A phase
        // snapshot here would re-create the race the ledger exists to
        // remove (a result created in `Streaming` was once dropped by a
        // `Sending`-only finalizer — bug #21).
        let mut entry =
            ChatEntry::tool_result(tool_call_id.clone(), name, "", ToolResultStatus::Pending);
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        let history_index = self.push_entry(entry);

        // Re-acquire the tracking map after push_entry releases &mut self.
        self.core
            .ephemeral
            .streaming_tool_result_indices
            .insert(tool_call_id.clone(), history_index);
    }

    /// Append incremental output to a pending ToolResult entry.
    ///
    /// Reads the pending entry from whichever busy phase is holding it.
    ///
    /// # Panics
    ///
    /// Panics if no pending entry exists for the given `tool_call_id`.
    pub fn append_tool_result_output(
        &mut self,
        tool_call_id: &ToolCallId,
        output: &str,
        kind: jinn_tools_msg::ToolOutputKind,
    ) {
        let Some(&history_index) = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .get(tool_call_id)
        else {
            return;
        };
        self.core.identity.last_history_activity_at = jiff::Timestamp::now();
        self.edit_history()
            .with_entry_at_mut(history_index, |entry| {
                if let ChatEntryKind::ToolResult {
                    ref mut content,
                    ref mut is_alert,
                    ..
                } = entry.kind
                {
                    content.push_str(output);
                    if kind == jinn_tools_msg::ToolOutputKind::Alert {
                        *is_alert = true;
                    }
                }
            });
    }

    /// Finalize a pending ToolResult entry with the final content and status.
    ///
    /// If a pending entry exists, updates it with the final content and
    /// success/failure status. If no pending entry exists (non-streaming tool),
    /// pushes a new completed entry.
    ///
    /// Accepts optional truncation metadata and full content from the tool
    /// execution result. When truncation is present, stores both the truncated
    /// content and the original untruncated output.
    ///
    /// Finalizes an existing ToolResult entry only while it is still
    /// `Pending`, returning whether one was found and resolved.
    ///
    /// The synthesized-cancellation path: a cancel's completion must resolve
    /// a call that never finished but must never overwrite a real result
    /// that landed first — real output wins over the drain's bookkeeping.
    fn finalize_pending_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        content: &str,
        status: ToolResultStatus,
    ) -> bool {
        // Untracked pending entry: scan history for the last pending result
        // for this call.
        let index = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .remove(tool_call_id)
            .or_else(|| {
                self.history().iter().rposition(|entry| {
                    matches!(
                        &entry.kind,
                        ChatEntryKind::ToolResult { id, status, .. }
                            if id == tool_call_id && *status == ToolResultStatus::Pending
                    )
                })
            });
        let Some(index) = index else {
            return false;
        };
        self.edit_history()
            .with_entry_at_mut(index, |entry| {
                if let ChatEntryKind::ToolResult {
                    content: entry_content,
                    status: entry_status,
                    is_alert,
                    ..
                } = &mut entry.kind
                {
                    // Pending only: a real completion that landed between the
                    // snapshot and this write already resolved the entry, and
                    // the synthesis must not clobber it.
                    if *entry_status == ToolResultStatus::Pending {
                        content.clone_into(entry_content);
                        *entry_status = status;
                        *is_alert = false;
                        entry.timing.finish();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    /// Finalizes an existing pending ToolResult entry (whichever busy phase
    /// tracks it, then a history scan), returning whether one was found.
    ///
    /// In-place finalization: never reorders entries or splits a tool loop.
    fn finalize_existing_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        content: &str,
        status: ToolResultStatus,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
        attachments: Vec<jinn_core_types::attachment::Attachment>,
    ) -> bool {
        let tracked_index = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .remove(tool_call_id);

        let apply = |entry: &mut ChatEntry| {
            if let ChatEntryKind::ToolResult {
                content: entry_content,
                status: entry_status,
                full_content: entry_full_content,
                truncation: entry_truncation,
                pin_position: entry_kind_pin,
                is_alert,
                attachments: entry_attachments,
                ..
            } = &mut entry.kind
            {
                content.clone_into(entry_content);
                *entry_status = status;
                *entry_full_content = full_content;
                *entry_truncation = truncation;
                *entry_kind_pin = pin_position;
                // The alert styling is pending-state only — a finished result
                // renders with its normal success/failure look.
                *is_alert = false;
                *entry_attachments = attachments;
                // Entry-level pin mirrors the kind-level pin so assembly,
                // compaction, and UI consumers read a single field.
                entry.pin_position = pin_position;
                entry.timing.finish();
            }
        };

        match tracked_index {
            Some(index) => self.edit_history().with_entry_at_mut(index, apply).is_some(),
            None => self
                .edit_history()
                .with_last_matching_mut(
                    |entry| {
                        matches!(&entry.kind, ChatEntryKind::ToolResult { id, .. } if id == tool_call_id)
                    },
                    apply,
                )
                .is_some(),
        }
    }

    pub fn finalize_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        content: &str,
        success: bool,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
    ) {
        self.finalize_tool_result_with_attachments(
            tool_call_id,
            name,
            content,
            success,
            full_content,
            truncation,
            pin_position,
            Vec::new(),
            None,
        );
    }

    /// [`Self::finalize_tool_result`] for results that produced non-text
    /// attachments (e.g. an image returned by `read_image`).
    ///
    /// `synthesized` is the cancel drain's mark: a synthesized completion
    /// finalizes only an entry that is still `Pending` — a real result that
    /// also landed already finalized it, and the synthesis must not overwrite
    /// real output — and never pushes a new entry. `None` means an ordinary
    /// completion, which keeps the historical push-on-miss behavior.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors finalize_tool_result; struct-builder would ripple to all tool actors"
    )]
    pub fn finalize_tool_result_with_attachments(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        content: &str,
        success: bool,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
        attachments: Vec<jinn_core_types::attachment::Attachment>,
        synthesized: Option<bool>,
    ) {
        let status = if success {
            ToolResultStatus::Success
        } else {
            ToolResultStatus::Failure
        };
        let pin_position_result = pin_position;

        let finalized = if synthesized == Some(true) {
            // A synthesized cancellation resolves a call whose entry is still
            // pending; a completed one already carries the real result and is
            // left alone.
            self.finalize_pending_tool_result(tool_call_id, content, ToolResultStatus::Failure)
        } else {
            self.finalize_existing_tool_result(
                tool_call_id,
                content,
                status,
                full_content.clone(),
                truncation.clone(),
                pin_position,
                attachments.clone(),
            )
        };

        if !finalized && synthesized != Some(true) {
            // No existing entry found - push a new one.
            let mut entry = if let Some(meta) = truncation {
                let full = full_content.unwrap_or_default();
                ChatEntry::tool_result_truncated(
                    tool_call_id.clone(),
                    name,
                    content.to_owned(),
                    full,
                    status,
                    meta,
                )
            } else {
                ChatEntry::tool_result_with_attachments(
                    tool_call_id.clone(),
                    name,
                    content,
                    status,
                    attachments,
                )
            };
            // Propagate tool-requested pin onto both the kind variant and
            // the entry-level field so assembly/compaction read a single source.
            if let ChatEntryKind::ToolResult {
                pin_position: ref mut kp,
                ..
            } = entry.kind
            {
                *kp = pin_position;
            }
            entry.pin_position = pin_position;
            self.push_entry(entry);
        }

        // A tool-requested pin (skill/save_plan bodies) expands to the whole
        // tool loop via the editor, so the pinned result can never be
        // separated from its call by pruning or compaction.
        if let Some(position) = pin_position_result {
            let result_id = self
    .core.history_work
    .history
                .iter()
                .rev()
                .find(|entry| {
                    matches!(&entry.kind, ChatEntryKind::ToolResult { id, .. } if id == tool_call_id)
                })
                .map(|entry| entry.id.clone());
            if let Some(id) = result_id {
                self.edit_history().pin(&id, position);
            }
        }
    }

    /// Force-exclude any `ToolCall` entries that lack matching `ToolResult` entries,
    /// and their empty parent `Assistant` entry.
    ///
    /// Called after hard cancel (ESC) to ensure the assembled prompt doesn't
    /// contain dangling `tool_calls` without corresponding `tool` results,
    /// which causes LLM providers to reject the request (e.g., ZAI error 1214).
    ///
    /// Uses `ContextOverride::ForcedExclude` rather than removing entries,
    /// preserving them for display in the UI.
    ///
    /// The excluded entries are also registered as an expanded ignored block,
    /// for the same reason [`Self::reset_streaming_entries_for_retry`] does it:
    /// an interrupted attempt is typically three entries — exactly the collapse
    /// threshold — so exclusion alone would reduce the whole attempt to a single
    /// "N hidden entries" line.
    ///
    /// Returns the ids whose context override changed.
    pub fn force_exclude_dangling_tool_calls(&mut self) -> Vec<ChatEntryId> {
        let excluded = self.edit_history().exclude_incomplete_trailing_loops();
        if !excluded.is_empty() {
            self.update_view(|v| v.shown_ignored_blocks.extend(excluded.iter().cloned()));
        }
        excluded
    }
}
