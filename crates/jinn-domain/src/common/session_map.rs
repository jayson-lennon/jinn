//! A non-empty map of chat sessions.
//!
//! [`SessionMap`] wraps a `HashMap<SessionId, ChatSessionState>` and enforces
//! the invariant that at least one session always exists. The active session
//! ID always points to a valid entry. This makes `active_session()` and
//! `active_session_mut()` infallible - no `Option`, no `expect`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::tree_aggregate::FrozenTreeNode;
use crate::protocol::SessionId;

/// Session lifecycle state - owned by the session-actor.
///
/// Tracks an in-progress session load from disk.
///
/// Only one session can be loaded at a time. The guard is set by the
/// IntentHandler when the user confirms a session load, and cleared by
/// the session-actor on completion (or the TUI tick on timeout).
#[derive(Debug)]
pub struct SessionLoadGuard {
    /// Which session is being loaded.
    pub session_id: SessionId,
    /// When the load started - used for timeout detection.
    pub started_at: std::time::Instant,
}

/// A non-empty map of sessions. The active session is always present.
///
/// # Invariants
///
/// - The map is never empty after construction.
/// - `active_session_id` always points to an existing entry.
/// - `remove()` creates a fresh session if the map would become empty.
#[derive(Debug)]
pub struct SessionMap {
    sessions: HashMap<SessionId, ChatSessionState>,
    /// Lightweight snapshots of archived session stats, used for tree aggregation.
    frozen_nodes: HashMap<SessionId, FrozenTreeNode>,
    active_session: SessionId,
    session_load_guard: Option<SessionLoadGuard>,
    default_cwd: PathBuf,
    /// Late-attached handle to the slice registry, carrying the
    /// slice cells (chat-log-view, chat-input, ...). Attached once at wiring; every
    /// session looked
    /// up through the map gets the handle on its way out (see
    /// `attach_slices` on `ChatSessionState`). Before attach, session
    /// facades run on their in-struct fallbacks — the removability
    /// property. (Composition attaches once on the map; direct pokes
    /// defeat the facade.)
    slices: std::sync::OnceLock<jinn_slices::Slices>,
}

impl Default for SessionMap {
    fn default() -> Self {
        let session = ChatSessionState::new();
        let id = session.session_id().clone();
        let mut sessions = HashMap::new();
        sessions.insert(id.clone(), session);
        Self {
            sessions,
            frozen_nodes: HashMap::new(),
            active_session: id,
            session_load_guard: None,
            default_cwd: PathBuf::from("/"),
            slices: std::sync::OnceLock::new(),
        }
    }
}

impl SessionMap {
    /// Create with an initial session. The map is never empty.
    pub fn new(session: ChatSessionState, default_cwd: PathBuf) -> Self {
        let id = session.session_id().clone();
        let mut sessions = HashMap::new();
        sessions.insert(id.clone(), session);
        Self {
            sessions,
            frozen_nodes: HashMap::new(),
            active_session: id,
            session_load_guard: None,
            default_cwd,
            slices: std::sync::OnceLock::new(),
        }
    }

    /// Attaches the slice registry handle carrying the slice cells.
    /// Called once at wiring; later calls are ignored.
    pub fn attach_slices(&self, slices: jinn_slices::Slices) {
        let _ = self.slices.set(slices);
    }

    /// The attached handle, if any.
    #[must_use]
    pub fn slices(&self) -> Option<&jinn_slices::Slices> {
        self.slices.get()
    }

    /// Hands the attached handle to `session` (a no-op before attach).
    /// Called on every path that lets a session out of the map, so a
    /// session's slice facade resolves the cell without per-creation
    /// wiring.
    fn attach_to(&self, session: &ChatSessionState) {
        if let Some(slices) = self.slices.get() {
            session.attach_slices(slices.clone());
        }
    }

    /// An owned clone of the attached handle, for `&mut` accessors that
    /// cannot re-borrow `self` while a session reference is live.
    fn handle_(&self) -> Option<jinn_slices::Slices> {
        self.slices.get().cloned()
    }

    /// Infallible - the active session is always present.
    ///
    /// # Panics
    ///
    /// Panics if the invariant is violated (active session ID not in map).
    /// This indicates a programming error - the map should never be in this state.
    #[expect(
        clippy::expect_used,
        reason = "SessionMap invariant: active_session always valid"
    )]
    pub fn active_session(&self) -> &ChatSessionState {
        let session = self
            .sessions
            .get(&self.active_session)
            .expect("SessionMap invariant violation: active_session not in map");
        self.attach_to(session);
        session
    }

    /// Infallible mutable access - the active session is always present.
    ///
    /// # Panics
    ///
    /// Panics if the invariant is violated (active session ID not in map).
    /// This indicates a programming error - the map should never be in this state.
    #[expect(
        clippy::expect_used,
        reason = "SessionMap invariant: active_session always valid"
    )]
    pub fn active_session_mut(&mut self) -> &mut ChatSessionState {
        let handle = self.handle_();
        let session = self
            .sessions
            .get_mut(&self.active_session)
            .expect("SessionMap invariant violation: active_session not in map");
        if let Some(slices) = handle {
            session.attach_slices(slices);
        }
        session
    }

    /// The active session ID.
    pub fn active_session_id(&self) -> &SessionId {
        &self.active_session
    }

    /// Set the active session. Returns false if the ID doesn't exist.
    pub fn set_active(&mut self, id: SessionId) -> bool {
        if self.sessions.contains_key(&id) {
            self.active_session = id;
            true
        } else {
            false
        }
    }

    /// Fallible lookup by ID.
    pub fn get(&self, id: &SessionId) -> Option<&ChatSessionState> {
        let session = self.sessions.get(id)?;
        self.attach_to(session);
        Some(session)
    }

    /// Fallible mutable lookup by ID.
    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut ChatSessionState> {
        let handle = self.handle_();
        let session = self.sessions.get_mut(id)?;
        if let Some(slices) = handle {
            session.attach_slices(slices);
        }
        Some(session)
    }

    /// Infallible lookup by ID. Use when the caller knows the session exists.
    ///
    /// # Panics
    ///
    /// Panics if the session does not exist. Prefer `get()` when the ID
    /// may not be present.
    #[expect(clippy::expect_used, reason = "caller guarantees session exists")]
    pub fn get_unchecked(&self, id: &SessionId) -> &ChatSessionState {
        let session = self
            .sessions
            .get(id)
            .expect("session must exist in SessionMap");
        self.attach_to(session);
        session
    }

    /// Infallible mutable lookup by ID. Use when the caller knows the session exists.
    ///
    /// # Panics
    ///
    /// Panics if the session does not exist. Prefer `get_mut()` when the ID
    /// may not be present.
    #[expect(clippy::expect_used, reason = "caller guarantees session exists")]
    pub fn get_unchecked_mut(&mut self, id: &SessionId) -> &mut ChatSessionState {
        let handle = self.handle_();
        let session = self
            .sessions
            .get_mut(id)
            .expect("session must exist in SessionMap");
        if let Some(slices) = handle {
            session.attach_slices(slices);
        }
        session
    }

    /// Returns mutable access to a session by ID, creating it if missing.
    pub fn get_or_create(&mut self, id: &SessionId) -> &mut ChatSessionState {
        let handle = self.handle_();
        let default_cwd = self.default_cwd.clone();
        let session = self.sessions.entry(id.clone()).or_insert_with(|| {
            let mut s = ChatSessionState::new();
            s.set_session_id(id.clone());
            s.set_cwd(default_cwd);
            s
        });
        if let Some(slices) = handle {
            session.attach_slices(slices);
        }
        session
    }

    /// Insert a session.
    pub fn insert(&mut self, session: ChatSessionState) {
        self.attach_to(&session);
        self.sessions.insert(session.session_id().clone(), session);
    }

    /// Remove a session. If the map would become empty, creates a fresh session.
    /// If the removed session was active, switches to the next (or fresh) session.
    /// Returns true if the session was found and removed.
    pub fn remove(&mut self, id: &SessionId) -> bool {
        let removed = self.sessions.remove(id).is_some();
        if !removed {
            return false;
        }

        // If we removed the active session, switch to another.
        if id == &self.active_session {
            self.active_session = self.sessions.keys().next().cloned().unwrap_or_else(|| {
                // Map is empty - create a fresh session.
                let fresh = ChatSessionState::new();
                let fresh_id = fresh.session_id().clone();
                self.attach_to(&fresh);
                self.sessions.insert(fresh_id.clone(), fresh);
                fresh_id
            });
        }

        true
    }

    /// Remove a session and replace with a caller-provided fresh session if
    /// the map would become empty. If the removed session was active, switches
    /// to the next available session. Atomically maintains the invariant.
    ///
    /// Returns `true` if the session was found and removed.
    ///
    /// Use this instead of `remove()` when the caller wants to control the
    /// profile of the fresh session (e.g. preserving model/strategy preferences).
    pub fn remove_and_replace(&mut self, id: &SessionId, fresh_session: ChatSessionState) -> bool {
        // Attach before insert so the fresh session is wired even when it
        // outlives the map as the sole resident.
        self.attach_to(&fresh_session);
        let removed = self.sessions.remove(id).is_some();
        if !removed {
            return false;
        }

        // If we removed the active session, switch to another.
        if id == &self.active_session {
            self.active_session = self.sessions.keys().next().cloned().unwrap_or_else(|| {
                // Map is empty - insert the caller-provided fresh session.
                let fresh_id = fresh_session.session_id().clone();
                self.sessions.insert(fresh_id.clone(), fresh_session);
                fresh_id
            });
        }

        true
    }

    /// Remove a session without creating a replacement or fixing the active
    /// session. The caller **must** insert a replacement and call `set_active`
    /// immediately afterward to restore the invariant.
    ///
    /// This is safe only when the caller has exclusive `&mut` access (no
    /// concurrent readers can observe the temporary violation).
    ///
    /// Returns `true` if the session was found and removed.
    pub fn remove_without_replacement(&mut self, id: &SessionId) -> bool {
        self.sessions.remove(id).is_some()
    }

    /// All sessions.
    pub fn sessions(&self) -> &HashMap<SessionId, ChatSessionState> {
        &self.sessions
    }

    /// Insert a frozen node snapshot for an archived session.
    ///
    /// Replaces any existing frozen node for the same session ID.
    pub(crate) fn insert_frozen_node(&mut self, node: FrozenTreeNode) {
        self.frozen_nodes.insert(node.session_id.clone(), node);
    }

    /// Read-only access to frozen node snapshots.
    ///
    /// Used by [`aggregate_tree_stats`](crate::feat::session::aggregate_tree_stats)
    /// to include archived sessions in tree summaries.
    pub fn frozen_nodes(&self) -> &HashMap<SessionId, FrozenTreeNode> {
        &self.frozen_nodes
    }

    /// Remove a frozen node snapshot.
    ///
    /// Called when a session is loaded back into memory so its stats
    /// come from the live session instead.
    /// Returns `true` if a frozen node was removed.
    pub fn remove_frozen_node(&mut self, id: &SessionId) -> bool {
        self.frozen_nodes.remove(id).is_some()
    }

    /// Mutable access to all sessions. `pub(crate)` to prevent external bypass of invariants.
    #[cfg(test)]
    pub(crate) fn sessions_mut(&mut self) -> &mut HashMap<SessionId, ChatSessionState> {
        &mut self.sessions
    }

    /// Iterate over all sessions.
    pub fn iter(&self) -> impl Iterator<Item = (&SessionId, &ChatSessionState)> {
        self.sessions.iter()
    }

    /// The number of sessions in the map.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the map contains a session with the given ID.
    pub fn contains(&self, id: &SessionId) -> bool {
        self.sessions.contains_key(id)
    }

    /// Whether the map has no sessions.
    ///
    /// Should always return `false` after construction (the invariant guarantees
    /// at least one session exists). Returns `true` only during transient states
    /// before the invariant is restored (e.g. between `remove_without_replacement`
    /// and a subsequent `insert`).
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns the ID of an arbitrary session in the map, or `None` if empty.
    pub fn any_session_id(&self) -> Option<SessionId> {
        self.sessions.keys().next().cloned()
    }

    /// Whether a session is currently being loaded from disk.
    pub fn is_loading(&self) -> bool {
        self.session_load_guard.is_some()
    }

    /// Begin loading a session. Sets the guard with the current timestamp.
    pub fn begin_load(&mut self, session_id: SessionId) {
        self.session_load_guard = Some(SessionLoadGuard {
            session_id,
            started_at: std::time::Instant::now(),
        });
    }

    /// Clear the loading guard (called on completion or timeout).
    pub fn clear_load(&mut self) {
        self.session_load_guard = None;
    }

    /// The loading guard, if active.
    pub fn session_load_guard(&self) -> Option<&SessionLoadGuard> {
        self.session_load_guard.as_ref()
    }

    /// The default CWD for new sessions.
    pub fn default_cwd(&self) -> &PathBuf {
        &self.default_cwd
    }

    /// Set the default CWD.
    pub fn set_default_cwd(&mut self, cwd: PathBuf) {
        self.default_cwd = cwd;
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
    use super::*;
    fn default_map() -> SessionMap {
        let session = ChatSessionState::new();
        SessionMap::new(session, PathBuf::from("/"))
    }

    #[rstest::rstest]
    fn active_session_returns_valid_ref() {
        // Given a default session map.
        let map = default_map();

        // When accessing the active session.
        let session = map.active_session();

        // Then it returns a valid reference.
        assert_eq!(session.session_id(), map.active_session_id());
    }

    #[rstest::rstest]
    fn set_active_returns_false_for_unknown_id() {
        // Given a session map.
        let mut map = default_map();

        // When setting active to a non-existent ID.
        let result = map.set_active(SessionId::new());

        // Then it returns false.
        assert!(!result);
    }

    #[rstest::rstest]
    fn remove_last_session_creates_fresh() {
        // Given a map with one session.
        let mut map = default_map();
        let id = map.active_session_id().clone();

        // When removing the only session.
        let removed = map.remove(&id);

        // Then it was removed.
        assert!(removed);
        // And the map still has a session (fresh one created).
        assert!(!map.sessions.is_empty());
        assert_ne!(map.active_session().session_id(), &id);
    }

    #[rstest::rstest]
    fn remove_non_active_preserves_active() {
        // Given a map with two sessions.
        let mut map = default_map();
        let active_id = map.active_session_id().clone();
        let other = ChatSessionState::new();
        let other_id = other.session_id().clone();
        map.insert(other);

        // When removing the non-active session.
        let removed = map.remove(&other_id);

        // Then the active session is unchanged.
        assert!(removed);
        assert_eq!(map.active_session_id(), &active_id);
    }

    #[rstest::rstest]
    fn remove_active_switches_to_next() {
        // Given a map with two sessions.
        let mut map = default_map();
        let other = ChatSessionState::new();
        let other_id = other.session_id().clone();
        map.insert(other);
        let active_id = map.active_session_id().clone();

        // When removing the active session.
        let removed = map.remove(&active_id);

        // Then active switches to the remaining session.
        assert!(removed);
        assert_eq!(map.active_session_id(), &other_id);
    }

    #[rstest::rstest]
    fn get_returns_none_for_unknown() {
        // Given a session map.
        let map = default_map();

        // When looking up a non-existent ID.
        let result = map.get(&SessionId::new());

        // Then it returns None.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn get_or_create_creates_when_missing() {
        // Given a session map.
        let mut map = default_map();
        let new_id = SessionId::new();

        // When getting or creating a missing session.
        let session = map.get_or_create(&new_id);

        // Then a new session was created with that ID.
        assert_eq!(session.session_id(), &new_id);
    }

    #[rstest::rstest]
    fn remove_and_replace_creates_fresh_when_map_emptied() {
        // Given a map with one session.
        let mut map = default_map();
        let id = map.active_session_id().clone();
        let fresh = ChatSessionState::new();
        let fresh_id = fresh.session_id().clone();

        // When removing the only session with a fresh replacement.
        let removed = map.remove_and_replace(&id, fresh);

        // Then it was removed.
        assert!(removed);
        // And the map has exactly one session (the fresh one).
        assert_eq!(map.session_count(), 1);
        assert_eq!(map.active_session_id(), &fresh_id);
    }

    #[rstest::rstest]
    fn remove_and_replace_switches_active_when_others_remain() {
        // Given a map with two sessions.
        let mut map = default_map();
        let other = ChatSessionState::new();
        let other_id = other.session_id().clone();
        map.insert(other);
        let active_id = map.active_session_id().clone();

        // When removing the active session.
        let fresh = ChatSessionState::new();
        let removed = map.remove_and_replace(&active_id, fresh);

        // Then active switches to the remaining session.
        assert!(removed);
        assert_eq!(map.active_session_id(), &other_id);
        // And the fresh session was NOT inserted (others exist).
        assert_eq!(map.session_count(), 1);
    }

    #[rstest::rstest]
    fn remove_and_replace_returns_false_for_missing_id() {
        // Given a map.
        let mut map = default_map();

        // When removing a non-existent ID.
        let fresh = ChatSessionState::new();
        let removed = map.remove_and_replace(&SessionId::new(), fresh);

        // Then it returns false.
        assert!(!removed);
        // And the original session is untouched.
        assert_eq!(map.session_count(), 1);
    }

    #[rstest::rstest]
    fn remove_without_replacement_removes_session() {
        // Given a map with two sessions.
        let mut map = default_map();
        let other = ChatSessionState::new();
        let other_id = other.session_id().clone();
        map.insert(other);
        let active_id = map.active_session_id().clone();

        // When removing the non-active session without replacement.
        let removed = map.remove_without_replacement(&other_id);

        // Then it was removed.
        assert!(removed);
        assert!(!map.contains(&other_id));
        // And the active session is unchanged.
        assert_eq!(map.active_session_id(), &active_id);
    }

    #[rstest::rstest]
    fn remove_without_replacement_does_not_touch_active() {
        // Given a map with one session.
        let mut map = default_map();
        let id = map.active_session_id().clone();

        // When removing the active session without replacement.
        let removed = map.remove_without_replacement(&id);

        // Then it was removed.
        assert!(removed);
        // And active_session_id still points to the old ID (caller must fix).
        assert_eq!(map.active_session_id(), &id);
        // And the map is empty (caller must insert replacement).
        assert!(map.is_empty());
    }

    #[rstest::rstest]
    fn remove_without_replacement_returns_false_for_missing() {
        // Given a map.
        let mut map = default_map();

        // When removing a non-existent ID.
        let removed = map.remove_without_replacement(&SessionId::new());

        // Then it returns false.
        assert!(!removed);
    }

    #[rstest::rstest]
    fn iter_yields_all_sessions() {
        // Given a map with two sessions.
        let mut map = default_map();
        let other = ChatSessionState::new();
        let other_id = other.session_id().clone();
        map.insert(other);

        // When iterating.
        let ids: Vec<_> = map.iter().map(|(id, _)| id.clone()).collect();

        // Then both session IDs are present.
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(map.active_session_id()));
        assert!(ids.contains(&other_id));
    }

    #[rstest::rstest]
    fn session_count_matches_number_of_sessions() {
        // Given a map with one session.
        let map = default_map();

        // When counting.
        // Then it returns 1.
        assert_eq!(map.session_count(), 1);
    }

    #[rstest::rstest]
    fn contains_returns_true_for_existing_session() {
        // Given a map.
        let map = default_map();

        // When checking if the active session exists.
        // Then it returns true.
        assert!(map.contains(map.active_session_id()));
    }

    #[rstest::rstest]
    fn contains_returns_false_for_missing_session() {
        // Given a map.
        let map = default_map();

        // When checking a random ID.
        // Then it returns false.
        assert!(!map.contains(&SessionId::new()));
    }

    #[rstest::rstest]
    fn is_empty_returns_false_after_construction() {
        // Given a default map.
        let map = default_map();

        // When checking emptiness.
        // Then it returns false.
        assert!(!map.is_empty());
    }

    #[rstest::rstest]
    fn any_session_id_returns_some_after_construction() {
        // Given a default map.
        let map = default_map();

        // When getting any session ID.
        let id = map.any_session_id();

        // Then it returns a valid ID.
        assert!(id.is_some());
        assert!(map.contains(&id.unwrap()));
    }

    #[rstest::rstest]
    fn is_loading_returns_false_initially() {
        // Given a default map.
        let map = default_map();

        // When checking loading state.
        // Then it returns false.
        assert!(!map.is_loading());
    }

    #[rstest::rstest]
    fn begin_load_sets_is_loading_true() {
        // Given a session map.
        let mut map = default_map();
        let session_id = SessionId::new();

        // When beginning a load.
        map.begin_load(session_id);

        // Then is_loading is true.
        assert!(map.is_loading());
    }

    #[rstest::rstest]
    fn clear_load_resets_is_loading() {
        // Given a map with an active load.
        let mut map = default_map();
        map.begin_load(SessionId::new());
        assert!(map.is_loading());

        // When clearing the load.
        map.clear_load();

        // Then is_loading is false again.
        assert!(!map.is_loading());
    }

    #[rstest::rstest]
    fn session_load_guard_returns_none_initially() {
        // Given a default map.
        let map = default_map();

        // When getting the load guard.
        // Then it returns None.
        assert!(map.session_load_guard().is_none());
    }

    #[rstest::rstest]
    fn session_load_guard_returns_some_with_correct_session_id() {
        // Given a session map.
        let mut map = default_map();
        let session_id = SessionId::new();

        // When beginning a load.
        map.begin_load(session_id.clone());

        // Then the guard contains the correct session ID.
        let guard = map.session_load_guard().expect("guard should exist");
        assert_eq!(guard.session_id, session_id);
    }

    #[rstest::rstest]
    fn clear_load_removes_guard() {
        // Given a map with an active load guard.
        let mut map = default_map();
        map.begin_load(SessionId::new());
        assert!(map.session_load_guard().is_some());

        // When clearing the load.
        map.clear_load();

        // Then the guard is removed.
        assert!(map.session_load_guard().is_none());
    }

    #[rstest::rstest]
    fn any_session_id_returns_none_when_truly_empty() {
        // Given a map with one session.
        let mut map = default_map();
        let id = map.active_session_id().clone();

        // When removing without replacement (leaving the map empty).
        map.remove_without_replacement(&id);

        // Then any_session_id returns None.
        assert!(map.any_session_id().is_none());
    }
}
