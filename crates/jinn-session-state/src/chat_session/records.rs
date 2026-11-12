//! Activity records: token ledger, task list, context size, and the discovered caches.

use super::*;

impl ChatSessionState {
    /// Read-only access to the token ledger.
    pub fn token_ledger(&self) -> &[TokenRecord] {
        &self.core.history_work.token_ledger
    }

    /// Push a token record onto the ledger.
    ///
    /// Records are immutable once pushed - this is the only way to add them.
    pub fn push_token_record(&mut self, record: TokenRecord) {
        self.core.history_work.token_ledger.push(record);
    }

    /// Read-only access to this session's task list.
    pub fn task_list(&self) -> &jinn_tools_msg::TaskList {
        &self.core.history_work.task_list
    }

    /// Mutable access to this session's task list.
    pub fn task_list_mut(&mut self) -> &mut jinn_tools_msg::TaskList {
        &mut self.core.history_work.task_list
    }

    /// Update the last token record's received count and cost.
    ///
    /// Called when `StreamCompleted` arrives to finalize the pending record.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the ledger is empty.
    pub fn finalize_last_token_record(
        &mut self,
        tokens_received: u32,
        cost: Option<f64>,
        model_used: Option<String>,
        prompt_tokens: Option<u32>,
        cached_tokens: Option<u32>,
    ) -> Result<(), StreamingError> {
        let last = self
            .core
            .history_work
            .token_ledger
            .last_mut()
            .ok_or(StreamingError::EmptyLedger)?;
        last.tokens_received = tokens_received;
        last.cost = cost;
        last.model_used = model_used;
        last.prompt_tokens = prompt_tokens;
        last.cached_tokens = cached_tokens;
        Ok(())
    }

    /// Sets `model_used` on the last token record (the placeholder pushed at enqueue time).
    /// This makes the model visible in the status bar immediately, before streaming completes.
    pub fn set_last_token_model(&mut self, model: String) {
        if let Some(last) = self.core.history_work.token_ledger.last_mut() {
            last.model_used = Some(model);
        }
    }

    /// The cached context size in tokens, if a prompt has been assembled.
    pub fn context_size(&self) -> Option<u32> {
        self.core.ephemeral.cached_context_size
    }

    /// Record an honest assembly's sizes: the prompt total and the
    /// constant floor's share of it (system prompt, skills, shipped tool
    /// schemas).
    ///
    /// The only production writer is the context-size actor, and the pair
    /// must land together — a total without its system half would make
    /// [`Self::work_size`] read `None` (gate passes) rather than a
    /// plausible-looking wrong number.
    pub fn set_context_sizes(&mut self, total: u32, system: u32) {
        self.core.ephemeral.cached_context_size = Some(total);
        self.core.ephemeral.cached_system_size = Some(system);
    }

    /// The work size in tokens: assembled total minus the constant floor
    /// — the conversation content alone.
    ///
    /// `None` unless both halves of the cached pair are present — after a
    /// compaction invalidation this reads `None` until the next honest
    /// recalc lands, which is exactly the behavior that stops a stale
    /// gate from re-firing on fresh history. The floor (system prompt,
    /// skills, shipped tool schemas) is constant across requests, so it
    /// is subtracted out here and can neither grow with the conversation
    /// nor sit at a compaction trigger's fire line.
    #[must_use]
    pub fn work_size(&self) -> Option<u32> {
        self.core
            .ephemeral
            .cached_context_size
            .zip(self.core.ephemeral.cached_system_size)
            .map(|(total, system)| total.saturating_sub(system))
    }

    /// Invalidate the cached size pair and bump the size generation.
    ///
    /// Called from the mutation drain when an applied batch carries a
    /// compaction-sourced mutation: the just-applied fold means the
    /// cached pre-compaction total no longer describes the session, and
    /// a gate reading it would re-fire on the fresh, tiny history.
    /// Gates read [`Self::work_size`] as `None` and pass until the next
    /// honest recalc.
    pub fn invalidate_sizes_bumping_generation(&mut self) {
        self.core.ephemeral.cached_context_size = None;
        self.core.ephemeral.cached_system_size = None;
        self.core.ephemeral.size_generation = self.core.ephemeral.size_generation.wrapping_add(1);
    }

    /// The current size generation: recalcs snapshot it before assembling.
    pub fn size_generation(&self) -> u64 {
        self.core.ephemeral.size_generation
    }

    /// Re-checks a recalc's snapshot: `true` when the generation moved
    /// since `snapshotted`, meaning the recalc's result is stale and its
    /// write must be discarded.
    #[must_use]
    pub fn size_generation_moved_since(&self, snapshotted: u64) -> bool {
        self.core.ephemeral.size_generation != snapshotted
    }

    // ----- Handoff compaction (per-session, process-local) -----

    /// Whether a handoff-compaction turn is in flight.
    pub fn is_handoff_pending(&self) -> bool {
        self.core.ephemeral.handoff_pending
    }

    /// Marks the handoff turn as in flight: the gate fired and the recap
    /// request replaced the pending dispatch.
    pub fn begin_handoff(&mut self) {
        self.core.ephemeral.handoff_pending = true;
    }

    /// Ends the handoff turn. Returns whether the caller owes the fold
    /// (`true` only when the turn was pending and reached its terminal
    /// success; `false` for a cancel, an error, or a turn that was never
    /// pending).
    ///
    /// A non-terminal outcome (stream-rule intercept, loop retry) leaves
    /// the flags untouched: the turn is still live and the fold still
    /// fires at its eventual terminal completion. A terminal outcome —
    /// success or failure — clears both handoff fields: the recap cycle's
    /// bookkeeping is done either way, and a failure leaves nothing owed.
    /// The success-path fold re-clears idempotently; on the mid-loop
    /// shape the fold's call here is the clear.
    pub fn end_handoff(&mut self, turn_is_over: bool) -> bool {
        if !self.core.ephemeral.handoff_pending {
            return false;
        }
        if turn_is_over {
            self.core.ephemeral.handoff_pending = false;
            self.core.ephemeral.handoff_continuation_owed = false;
            return true;
        }
        // Terminal failure: the turn is over, the fold is not owed, and
        // the owed-continuation marker must not orphan — the field pair
        // clears together.
        self.core.ephemeral.handoff_pending = false;
        self.core.ephemeral.handoff_continuation_owed = false;
        false
    }

    /// Clears both handoff fields outright — the cancel sweep's cleanup:
    /// the turn is gone, so nothing owes the fold and no continuation is
    /// owed. Canceling a handoff cancels the turn; there is no
    /// handoff-specific cancel path.
    pub fn abandon_handoff(&mut self) {
        self.core.ephemeral.handoff_pending = false;
        self.core.ephemeral.handoff_continuation_owed = false;
    }

    /// Record that the just-run fold excluded its set: the session has
    /// folded since the last User or Intersession entry, until the next
    /// such entry arrives. The gate refuses to fold again while this
    /// holds — a fire with nothing new to summarize would loop on the
    /// recap itself.
    pub fn set_last_fold_gathered(&mut self) {
        self.core.ephemeral.folded_since_last_user_turn = true;
    }

    /// Whether a fold has run since the last User or Intersession entry.
    #[must_use]
    pub fn folded_since_last_user_turn(&self) -> bool {
        self.core.ephemeral.folded_since_last_user_turn
    }

    /// A User or Intersession entry arrived: new task-bearing work exists
    /// for a fold to summarize, so re-arm the gate's fire line.
    pub fn note_user_turn_for_fold(&mut self) {
        self.core.ephemeral.folded_since_last_user_turn = false;
    }

    /// Records that the gate fired mid-loop: the replaced request was a
    /// live tool-loop continuation, so the recap's success must route
    /// back into the loop instead of ending the turn. Read (never
    /// cleared) by the completion routing; cleared alongside
    /// [`Self::is_handoff_pending`] by [`Self::end_handoff`] and
    /// [`Self::abandon_handoff`].
    pub fn set_handoff_continuation_owed(&mut self) {
        self.core.ephemeral.handoff_continuation_owed = true;
    }

    /// Whether the handoff replaced a mid-loop continuation, without
    /// clearing — the completion routing peeks this (together with
    /// [`Self::is_handoff_pending`]) to ack the recap's success as a loop
    /// continuation; the clears belong to [`Self::end_handoff`] and
    /// [`Self::abandon_handoff`].
    #[must_use]
    pub fn handoff_continuation_owed(&self) -> bool {
        self.core.ephemeral.handoff_continuation_owed
    }

    // ----- Discovered resources (per-session, cwd-scoped) -----
    //
    // These are populated by the scan actors and read by prompt assembly and
    // the skill tool. They are NOT persisted.

    /// Returns the skills discovered for this session's cwd tree.
    pub fn discovered_skills(&self) -> &[Skill] {
        &self.core.ephemeral.discovered_skills
    }

    /// Returns the prompt templates discovered for this session's cwd tree.
    pub fn discovered_prompt_templates(&self) -> &PromptTemplateStore {
        &self.core.ephemeral.discovered_prompt_templates
    }

    /// Returns the context files discovered for this session's cwd tree.
    pub fn discovered_context_files(&self) -> &[jinn_context::ContextFile] {
        &self.core.ephemeral.discovered_context_files
    }

    /// Returns the workflows discovered for this session (launch-global).
    pub fn discovered_workflows(&self) -> &[Workflow] {
        &self.core.ephemeral.discovered_workflows
    }

    /// Replaces the discovered skills set for this session (scan-actor write path).
    pub fn set_discovered_skills(&mut self, skills: Vec<Skill>) {
        self.core.ephemeral.discovered_skills = skills;
    }

    /// Replaces the discovered prompt-template store for this session.
    pub fn set_discovered_prompt_templates(&mut self, store: PromptTemplateStore) {
        self.core.ephemeral.discovered_prompt_templates = store;
    }

    /// Replaces the discovered context files for this session.
    pub fn set_discovered_context_files(&mut self, files: Vec<jinn_context::ContextFile>) {
        self.core.ephemeral.discovered_context_files = files;
    }

    /// Replaces the discovered workflows set for this session (scan-actor write path).
    pub fn set_discovered_workflows(&mut self, workflows: Vec<Workflow>) {
        self.core.ephemeral.discovered_workflows = workflows;
    }

    /// Restore the token ledger from persisted data.
    pub fn restore_token_ledger(&mut self, records: Vec<TokenRecord>) {
        self.core.history_work.token_ledger = records;
    }

    /// Restore the parent session from persisted data.
    pub fn restore_parent_session(&mut self, parent: Option<SessionId>) {
        self.core.identity.parent_session = parent;
    }

    /// Restore the updated_at timestamp from persisted data.
    pub fn restore_updated_at(&mut self, ts: jiff::Timestamp) {
        self.core.identity.updated_at = ts;
    }

    /// Restore the creation timestamp from persisted data.
    pub fn restore_created_at(&mut self, ts: jiff::Timestamp) {
        self.core.identity.created_at = ts;
    }
}
