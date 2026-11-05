//! Session identity and lifecycle: id, timestamps, cwd/project, persistence, state machine.

use super::*;

impl ChatSessionState {
    /// Captures all durable state from this authoritative session revision.
    ///
    /// Call under the application state read lock. The revision and payload are
    /// derived from the same core reference, so metadata, persisted history,
    /// and token accounting cannot come from separate application versions.
    #[must_use]
    pub fn capture_snapshot(&self) -> crate::snapshot::SessionSnapshot {
        let revision = self.core.next_capture_revision();
        crate::snapshot::SessionSnapshot::from((revision, &self.core))
    }

    /// The store crate (`jinn-session-store`) serializes this into the
    /// session row's metadata blob. Returns a clone so the live session is
    /// never mutably borrowed by persistence.
    #[must_use]
    pub fn persistable_core(&self) -> SessionCore {
        self.core.clone()
    }

    /// Replace the core wholesale (load path).
    ///
    /// The store crate rebuilds a [`SessionCore`] from a persisted snapshot
    /// and swaps it into a default-constructed shell. Do not call on a live
    /// session — in-flight turn state lives in the core's `ephemeral`.
    pub fn set_core(&mut self, core: SessionCore) {
        self.core = core;
    }

    /// Whether this session has no history entries.
    ///
    /// A session is "empty" when it has never had any entries pushed -
    /// no user messages, no system messages, nothing.
    /// Not to be confused with [`Self::is_idle`] which checks
    /// streaming/sending/assembling state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core.history_work.history.is_empty()
    }

    /// Whether this session should be persisted to disk.
    pub fn persist(&self) -> bool {
        self.core.storage.persist
    }

    /// Mark this session as having been meaningfully interacted with by the user.
    /// Once set, the session becomes eligible for persistence.
    pub fn mark_interacted(&mut self) {
        self.core.identity.has_interacted = true;
    }

    /// Whether this session has been interacted with.
    #[must_use]
    pub fn has_interacted(&self) -> bool {
        self.core.identity.has_interacted
    }

    /// Whether this session should be persisted to disk.
    ///
    /// Returns `false` immediately when `persist == false` (explicitly marked
    /// transient — e.g. a one-shot). Otherwise returns `true`
    /// if any of:
    /// - The user has interacted with this session (`has_interacted`)
    /// - The session has a lifecycle (setup/teardown scripts)
    /// - The session was forked from another session
    #[must_use]
    pub fn is_persistable(&self) -> bool {
        if !self.core.storage.persist {
            return false;
        }
        if self.core.lifecycle.lifecycle_name.is_some() {
            return true;
        }
        if self.core.identity.parent_session.is_some() {
            return true;
        }
        if self.core.identity.has_interacted {
            return true;
        }
        false
    }

    /// Returns this session's working directory for tool execution.
    pub fn cwd(&self) -> &MarkedPathBuf<Absolute> {
        &self.core.lifecycle.cwd
    }

    /// When this session last saw provider activity (model responses).
    pub fn last_provider_activity_at(&self) -> &Timestamp {
        &self.core.identity.last_provider_activity_at
    }

    /// When this session last saw history activity (new entries appended).
    pub fn last_history_activity_at(&self) -> &Timestamp {
        &self.core.identity.last_history_activity_at
    }

    /// Sets when this session last saw provider activity (streaming/turn).
    pub fn set_last_provider_activity_at(&mut self, ts: Timestamp) {
        self.core.identity.last_provider_activity_at = ts;
    }

    /// Sets when this session last saw history activity (new entries appended).
    pub fn set_last_history_activity_at(&mut self, ts: Timestamp) {
        self.core.identity.last_history_activity_at = ts;
    }

    /// Sets this session's working directory.
    pub fn set_cwd(&mut self, cwd: MarkedPathBuf<Absolute>) {
        self.core.lifecycle.cwd = cwd;
    }

    /// Returns the project directory this session is associated with, if any.
    pub fn project(&self) -> Option<&MarkedPathBuf<Absolute>> {
        self.core.identity.project.as_ref()
    }

    /// Stamps the session's project association. Callers are the projects UI
    /// flow (at session creation) and subagent spawning (inheriting the
    /// parent's stamp); the stamp never follows later cwd changes.
    pub fn set_project(&mut self, project: Option<MarkedPathBuf<Absolute>>) {
        self.core.identity.project = project;
    }

    /// Sets this session's home directory for resolving `@~/path` references.
    pub fn set_home(&mut self, home: MarkedPathBuf<Absolute>) {
        self.core.lifecycle.home = home;
    }

    /// The parent session, if this session was forked from another.
    pub fn parent_session(&self) -> &Option<SessionId> {
        &self.core.identity.parent_session
    }

    /// The highest entry ordinal inherited from parent at fork time.
    /// `None` for root sessions.
    pub fn fork_ordinal(&self) -> Option<usize> {
        self.core.identity.fork_ordinal
    }

    /// How this session came into being. Identity, not structure —
    /// see [`SessionOrigin`].
    pub fn origin(&self) -> SessionOrigin {
        self.core.identity.origin
    }

    /// Set the fork ordinal for testing and construction.
    pub fn set_fork_ordinal(&mut self, ordinal: usize) {
        self.core.identity.fork_ordinal = Some(ordinal);
    }

    /// Set the session origin for construction paths that decide the kind
    /// after building the session (forks built from snapshots, test
    /// fixtures). Construction paths that know the kind up front use a
    /// dedicated constructor (`new_child`, `new_bot`) instead.
    pub fn set_origin(&mut self, origin: SessionOrigin) {
        self.core.identity.origin = origin;
    }

    /// Set the parent session.
    pub fn set_parent_session(&mut self, parent: SessionId) {
        self.core.identity.parent_session = Some(parent);
    }

    /// This session's unique identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.core.identity.session_id
    }

    /// Set the session ID (used when inserting into a HashMap with an external key).
    pub fn set_session_id(&mut self, id: SessionId) {
        self.core.identity.session_id = id;
    }

    /// The session title. `None` until the first user message.
    pub fn title(&self) -> Option<&str> {
        self.core.identity.title.as_deref()
    }

    /// Set the session title.
    pub fn set_title(&mut self, title: String) {
        self.core.identity.title = Some(title);
    }

    /// Mark this session as persistent (`true`) or transient (`false`).
    /// Transient sessions (e.g. one-shots) are never written to the store.
    pub fn set_persist(&mut self, persist: bool) {
        self.core.storage.persist = persist;
    }

    /// When this session was last updated.
    pub fn updated_at(&self) -> &Timestamp {
        &self.core.identity.updated_at
    }

    /// When this session was created. Never changes after construction.
    pub fn created_at(&self) -> &Timestamp {
        &self.core.identity.created_at
    }

    /// Update the timestamp to now.
    pub fn touch(&mut self) {
        self.core.identity.updated_at = Timestamp::now();
    }

    /// Generic blob storage for future subsystems.
    pub fn blobs(&self) -> &HashMap<String, JsonValue> {
        &self.core.integrations.blobs
    }

    /// Mutable access to generic blob storage.
    pub fn blobs_mut(&mut self) -> &mut HashMap<String, JsonValue> {
        &mut self.core.integrations.blobs
    }

    /// The name of the lifecycle that created this session, if any.
    pub fn lifecycle_name(&self) -> Option<&str> {
        self.core.lifecycle.lifecycle_name.as_deref()
    }

    /// Set the lifecycle name.
    pub fn set_lifecycle_name(&mut self, name: Option<String>) {
        self.core.lifecycle.lifecycle_name = name;
    }

    /// The args used during setup (replayed for teardown), as ordered
    /// name-value pairs.
    pub fn lifecycle_args(&self) -> &LifecycleArgs {
        &self.core.lifecycle.lifecycle_args
    }

    /// Set the lifecycle args.
    pub fn set_lifecycle_args(&mut self, args: LifecycleArgs) {
        self.core.lifecycle.lifecycle_args = args;
    }

    /// The workflow-local lifecycle file this session's lifecycle was
    /// resolved from, if any. `None` means jinn.toml.
    pub fn lifecycle_source(&self) -> Option<&str> {
        self.core.lifecycle.lifecycle_source.as_deref()
    }

    /// Set the workflow-local lifecycle file the lifecycle resolved from.
    pub fn set_lifecycle_source(&mut self, source: Option<String>) {
        self.core.lifecycle.lifecycle_source = source;
    }

    /// Returns the session's memory state.
    pub fn session_state(&self) -> SessionState {
        self.core.storage.session_state
    }

    /// Sets the session's memory state.
    pub fn set_session_state(&mut self, state: SessionState) {
        self.core.storage.session_state = state;
    }

    /// Whether this session belongs to the sidebar's Sessions list.
    ///
    /// A listed session is one that is in memory (`Loaded`) and is not a
    /// Library member: the Sessions tree lists live conversations, while a
    /// member has been *moved* to the Library section and is hidden from it.
    /// Every consumer that filters the session map into the Sessions list
    /// must ask this instead of restating the conjunction, so the list, the
    /// memoization key, and the cursor's index space cannot disagree.
    #[must_use]
    pub fn is_listed(&self) -> bool {
        self.core.storage.session_state == SessionState::Loaded && !self.core.storage.in_library
    }

    /// Whether this session belongs to the sidebar's Library section.
    pub fn in_library(&self) -> bool {
        self.core.storage.in_library
    }

    /// Sets this session's Library membership.
    pub fn set_in_library(&mut self, in_library: bool) {
        self.core.storage.in_library = in_library;
    }

    /// Returns the lifecycle script state.
    pub fn lifecycle_script_state(&self) -> LifecycleScriptState {
        self.core.lifecycle.lifecycle_script_state
    }

    /// Advances lifecycle state after successful setup: `NothingRan → SetupRan`.
    pub fn advance_lifecycle_after_setup(&mut self) {
        self.core
            .lifecycle
            .lifecycle_script_state
            .advance_after_setup();
    }

    /// Advances lifecycle state after successful teardown: `SetupRan → TeardownRan`.
    pub fn advance_lifecycle_after_teardown(&mut self) {
        self.core
            .lifecycle
            .lifecycle_script_state
            .advance_after_teardown();
    }
}
