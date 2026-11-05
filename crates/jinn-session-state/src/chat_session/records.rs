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

    /// Update the cached context size.
    pub fn set_context_size(&mut self, size: u32) {
        self.core.ephemeral.cached_context_size = Some(size);
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
