//! Queued turn items and the deferred history-mutation accumulator plumbing.

use super::*;

impl ChatSessionState {
    /// The turn dispatch queue.
    ///
    /// The queue is a [`TurnQueue`] and this hands back that type rather than
    /// unwrapping it into a bare `VecDeque`: the six methods it used to proxy
    /// (`queue`, `queue_len`, `enqueue`, `enqueue_front`, `dequeue`,
    /// `drain_queue`) were one forwarding call each, so every caller was one
    /// step further from the type that owns the behavior. Callers reach the
    /// operations they need directly on the queue instead.
    ///
    #[must_use]
    pub fn message_queue(&self) -> &jinn_turn_dispatch_msg::TurnQueue {
        &self.core.ephemeral.message_queue
    }

    /// Mutable access to the turn dispatch queue.
    ///
    /// For the operations that consume the queue rather than inspect it —
    /// `drain` and `pop` — which cannot be reached through a shared borrow.
    /// The turn-dispatch actor still pops through [`Self::dequeue`]; this
    /// exists for the paths that take the whole queue at once, such as a
    /// cancel draining everything queued behind the turn it is aborting.
    pub fn message_queue_mut(&mut self) -> &mut jinn_turn_dispatch_msg::TurnQueue {
        &mut self.core.ephemeral.message_queue
    }

    /// Number of items waiting in the queue.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.core.ephemeral.message_queue.len()
    }

    /// Push an item onto the back of the queue.
    pub fn enqueue(&mut self, item: jinn_turn_dispatch_msg::QueueItem) {
        self.core.ephemeral.message_queue.enqueue(item);
    }

    /// Push an item onto the front of the queue (for priority items).
    pub fn enqueue_front(&mut self, item: jinn_turn_dispatch_msg::QueueItem) {
        self.core.ephemeral.message_queue.enqueue_front(item);
    }

    /// Pop the front item from the queue, if any.
    ///
    /// The turn-dispatch slice's queue actor is the production caller; the
    /// queue lives on the session, so the pop must be reachable there.
    pub fn dequeue(&mut self) -> Option<jinn_turn_dispatch_msg::QueueItem> {
        self.core.ephemeral.message_queue.pop()
    }

    /// Queue a batch of mutations for deferred application.
    ///
    /// Empty batches are silently ignored.
    pub fn queue_mutations(&mut self, batch: Vec<HistoryMutation>) {
        if !batch.is_empty() {
            self.core.ephemeral.pending_mutations.push(batch);
        }
    }

    /// Drain all pending mutation batches.
    pub fn drain_pending_mutations(&mut self) -> Vec<Vec<HistoryMutation>> {
        std::mem::take(&mut self.core.ephemeral.pending_mutations)
    }

    /// Apply a batch of mutations. Resolves IDs to current positions.
    ///
    /// Silently skips mutations targeting nonexistent entries.
    /// Processing order within a batch is preserved - earlier mutations
    /// are visible to later ones in the same batch.
    pub fn apply_mutations(&mut self, batch: Vec<HistoryMutation>) -> Vec<ChatEntryId> {
        self.edit_history().apply(batch)
    }

    /// Drain all pending mutation batches and apply them.
    ///
    /// Returns the number of batches applied and the entry IDs whose
    /// `context_override` actually changed value during this drain.
    pub fn drain_and_apply_pending_mutations(&mut self) -> (usize, Vec<ChatEntryId>) {
        let batches = self.drain_pending_mutations();
        let count = batches.len();
        let mut changed = Vec::new();
        for batch in batches {
            let mut batch_changed = self.apply_mutations(batch);
            changed.append(&mut batch_changed);
        }
        (count, changed)
    }

    /// Current deduplicated token total held in the accumulation buffer.
    ///
    /// Drives the threshold flush decision in `handle_submit_history_mutations`.
    #[must_use]
    pub fn accumulated_overrides_total(&self) -> u64 {
        self.core.ephemeral.accumulated_overrides.total_tokens()
    }

    /// The number of distinct entries currently held back in the accumulation
    /// buffer awaiting the threshold flush. Each represents one pending prune.
    /// Unlike the token total, this is stable across threshold changes and
    /// counts only `ForcedExclude` (prune) overrides — shields and compaction
    /// never enter the buffer.
    #[must_use]
    pub fn accumulated_prune_count(&self) -> usize {
        self.core.ephemeral.accumulated_overrides.len()
    }

    /// Pushes a `SetContextOverride` mutation into the accumulation buffer with
    /// its pre-resolved token cost. The buffer dedups by entry and respects
    /// shield dominance; the override is held back until the threshold flush.
    pub fn route_override(
        &mut self,
        entry_id: ChatEntryId,
        value: ContextOverride,
        source: ChangeSource,
        token_cost: u32,
    ) {
        self.core
            .ephemeral
            .accumulated_overrides
            .push(entry_id, value, source, token_cost);
    }

    /// Return the number of deferred history-mutation batches.
    #[must_use]
    pub fn pending_mutation_count(&self) -> usize {
        self.core.ephemeral.pending_mutations.len()
    }

    /// Return whether history mutations are currently deferred.
    #[must_use]
    pub fn has_pending_mutations(&self) -> bool {
        !self.core.ephemeral.pending_mutations.is_empty()
    }

    /// Apply a context override to an entry by current history index.
    pub fn set_entry_context_override_at(
        &mut self,
        index: usize,
        value: ContextOverride,
        source: &ChangeSource,
    ) -> bool {
        self.edit_history()
            .with_entry_at_mut(index, |entry| {
                entry.apply_context_override(value, source.clone());
            })
            .is_some()
    }

    /// Drains the accumulation buffer into `pending_mutations` as a single batch
    /// if its token total has crossed the threshold.
    ///
    /// Returns `true` if a flush occurred.
    pub fn flush_accumulated_overrides_if_needed(&mut self, threshold: u32) -> bool {
        if self.accumulated_overrides_total() >= u64::from(threshold) {
            let batch = self.core.ephemeral.accumulated_overrides.drain();
            self.queue_mutations(batch);
            true
        } else {
            false
        }
    }

    /// Disable the tool loop for this session's current turn.
    ///
    pub fn set_tool_loop_disabled(&mut self) {
        self.core.ephemeral.set_tool_loop_disabled();
    }

    /// Take the tool-loop-disabled flag, clearing it.
    ///
    pub fn take_tool_loop_disabled(&mut self) -> bool {
        self.core.ephemeral.take_tool_loop_disabled()
    }

    /// Check whether the tool loop is disabled, without clearing.
    ///
    pub fn is_tool_loop_disabled(&self) -> bool {
        self.core.ephemeral.is_tool_loop_disabled()
    }

    /// Request a buffer drain at this turn's cancel settle.
    ///
    pub fn request_cancel_drain(&mut self) {
        self.core.ephemeral.request_cancel_drain();
    }

    /// Take the pending-cancel-drain request, clearing it.
    ///
    pub fn take_pending_cancel_drain(&mut self) -> bool {
        self.core.ephemeral.take_pending_cancel_drain()
    }
}
