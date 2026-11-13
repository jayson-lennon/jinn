//! Live MCP connection state, keyed by session.
//!
//! Durable server enablement remains part of the session snapshot. This value
//! stores only the independently live status and stderr projections produced by
//! MCP connection actors.

use std::collections::{BTreeMap, HashMap};

use jinn_core_types::SessionId;
use jinn_slices::SlotKey;

use crate::McpConnectionStatus;

/// Runtime-only MCP state for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpSessionRuntimeState {
    /// Connection status by configured server name.
    pub status: BTreeMap<String, McpConnectionStatus>,
    /// Captured stderr tail by configured server name.
    pub stderr: BTreeMap<String, String>,
}

/// MCP runtime projections for all loaded sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpRuntimeState {
    sessions: HashMap<SessionId, McpSessionRuntimeState>,
    /// Runtime status and stderr for singleton servers, keyed by server name.
    ///
    /// A singleton serves every session through one connection, so its
    /// projections are not owned by any session: they live here, readable
    /// without a session id, and are never removed by session teardown.
    singletons: BTreeMap<String, McpSessionRuntimeState>,
}

impl McpRuntimeState {
    /// Sets one server's connection status for a session.
    pub fn set_status(
        &mut self,
        session_id: &SessionId,
        server: &str,
        status: McpConnectionStatus,
    ) {
        self.sessions
            .entry(session_id.clone())
            .or_default()
            .status
            .insert(server.to_owned(), status);
    }

    /// Returns one server's connection status.
    #[must_use]
    pub fn status(&self, session_id: &SessionId, server: &str) -> Option<McpConnectionStatus> {
        self.sessions
            .get(session_id)
            .and_then(|session| session.status.get(server))
            .copied()
    }

    /// Returns a coherent copy of one session's status projection.
    #[must_use]
    pub fn statuses(&self, session_id: &SessionId) -> BTreeMap<String, McpConnectionStatus> {
        self.sessions
            .get(session_id)
            .map_or_default(|session| session.status.clone())
    }

    /// Returns the status projection a session actually observes: its own
    /// per-session statuses with every singleton overlaid.
    ///
    /// A singleton serves every session through one shared connection, so its
    /// status is not owned by the session that spawned it — every session
    /// sees the same value. Overlaying (rather than merging into one bucket)
    /// keeps `statuses` a pure per-session projection for callers that need
    /// exactly that.
    #[must_use]
    pub fn effective_statuses(
        &self,
        session_id: &SessionId,
    ) -> BTreeMap<String, McpConnectionStatus> {
        let mut merged = self.statuses(session_id);
        for (server, state) in &self.singletons {
            if let Some(status) = state.status.get(server) {
                merged.insert(server.clone(), *status);
            }
        }
        merged
    }

    /// Replaces one server's captured stderr tail for a session.
    pub fn set_stderr(&mut self, session_id: &SessionId, server: &str, tail: String) {
        self.sessions
            .entry(session_id.clone())
            .or_default()
            .stderr
            .insert(server.to_owned(), tail);
    }

    /// Returns the stderr projection a session actually observes: its own
    /// per-session tails with every singleton overlaid. See
    /// [`Self::effective_statuses`] for why singletons overlay.
    #[must_use]
    pub fn effective_stderr(&self, session_id: &SessionId) -> BTreeMap<String, String> {
        let mut merged = self
            .sessions
            .get(session_id)
            .map_or_default(|session| session.stderr.clone());
        for (server, state) in &self.singletons {
            if let Some(tail) = state.stderr.get(server) {
                merged.insert(server.clone(), tail.clone());
            }
        }
        merged
    }

    /// Returns one server's captured stderr tail.
    #[must_use]
    pub fn stderr(&self, session_id: &SessionId, server: &str) -> Option<&str> {
        self.sessions
            .get(session_id)
            .and_then(|session| session.stderr.get(server))
            .map(String::as_str)
    }

    /// Returns a coherent copy of one session's stderr projection.
    #[must_use]
    pub fn stderr_tails(&self, session_id: &SessionId) -> BTreeMap<String, String> {
        self.sessions
            .get(session_id)
            .map_or_default(|session| session.stderr.clone())
    }

    /// Removes all runtime data for one server in one session.
    pub fn clear_server(&mut self, session_id: &SessionId, server: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.status.remove(server);
            session.stderr.remove(server);
            if session.status.is_empty() && session.stderr.is_empty() {
                self.sessions.remove(session_id);
            }
        }
    }

    /// Removes all runtime data for one session.
    pub fn clear_session(&mut self, session_id: &SessionId) {
        self.sessions.remove(session_id);
    }

    /// Sets one singleton server's connection status.
    ///
    /// Deliberately ignores any session-scoped entry the same server name
    /// might carry: a server is either singleton or per-session, never both.
    pub fn set_singleton_status(&mut self, server: &str, status: McpConnectionStatus) {
        self.singletons
            .entry(server.to_owned())
            .or_default()
            .status
            .insert(server.to_owned(), status);
    }

    /// Returns one singleton server's connection status.
    #[must_use]
    pub fn singleton_status(&self, server: &str) -> Option<McpConnectionStatus> {
        self.singletons
            .get(server)
            .and_then(|s| s.status.get(server))
            .copied()
    }

    /// Replaces one singleton server's captured stderr tail.
    pub fn set_singleton_stderr(&mut self, server: &str, tail: String) {
        self.singletons
            .entry(server.to_owned())
            .or_default()
            .stderr
            .insert(server.to_owned(), tail);
    }

    /// Returns one singleton server's captured stderr tail.
    #[must_use]
    pub fn singleton_stderr(&self, server: &str) -> Option<&str> {
        self.singletons
            .get(server)
            .and_then(|s| s.stderr.get(server))
            .map(String::as_str)
    }

    /// Clears one singleton server's runtime data (restart, disable, stop).
    pub fn clear_singleton(&mut self, server: &str) {
        self.singletons.remove(server);
    }

    /// Returns whether the named server has a singleton runtime entry.
    #[must_use]
    pub fn is_singleton_tracked(&self, server: &str) -> bool {
        self.singletons.contains_key(server)
    }
}

/// Stable cell slot for the live MCP runtime projection.
#[must_use]
pub fn mcp_runtime_slot() -> SlotKey {
    SlotKey::builtin("mcp", "runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    fn set_status_is_visible_for_one_session_and_server() {
        // Given an empty MCP runtime projection.
        let mut runtime = McpRuntimeState::default();
        let session_id = SessionId::new();

        // When setting one server's status.
        runtime.set_status(&session_id, "weather", McpConnectionStatus::Running);

        // Then that status is readable for the same session and server.
        assert_eq!(
            runtime.status(&session_id, "weather"),
            Some(McpConnectionStatus::Running)
        );
    }

    #[rstest::rstest]
    fn effective_statuses_overlay_singletons_for_any_session() {
        // Given a singleton Running and a per-session server Starting.
        let mut runtime = McpRuntimeState::default();
        runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);
        let session_id = SessionId::new();
        runtime.set_status(&session_id, "weather", McpConnectionStatus::Starting);

        // When reading the effective projection for that session...
        let statuses = runtime.effective_statuses(&session_id);

        // Then the singleton's status appears in it.
        assert_eq!(
            statuses.get("bundled-example"),
            Some(&McpConnectionStatus::Running)
        );
        // And the per-session status is preserved.
        assert_eq!(
            statuses.get("weather"),
            Some(&McpConnectionStatus::Starting)
        );
    }

    #[rstest::rstest]
    fn effective_statuses_are_session_independent_for_singletons() {
        // Given a singleton Running, spawned "under" session A.
        let mut runtime = McpRuntimeState::default();
        runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);

        // When reading the effective projection for an unrelated session.
        let statuses = runtime.effective_statuses(&SessionId::new());

        // Then the singleton's status resolves for it too.
        assert_eq!(
            statuses.get("bundled-example"),
            Some(&McpConnectionStatus::Running)
        );
    }

    #[rstest::rstest]
    fn effective_stderr_overlays_singletons() {
        // Given a singleton stderr tail and a per-session one.
        let mut runtime = McpRuntimeState::default();
        runtime.set_singleton_stderr("bundled-example", "singleton tail".to_owned());
        let session_id = SessionId::new();
        runtime.set_stderr(&session_id, "weather", "session tail".to_owned());

        // When reading the effective stderr projection.
        let stderr = runtime.effective_stderr(&session_id);

        // Then both are present.
        assert_eq!(
            stderr.get("bundled-example").map(String::as_str),
            Some("singleton tail")
        );
        assert_eq!(
            stderr.get("weather").map(String::as_str),
            Some("session tail")
        );
    }

    #[rstest::rstest]
    fn clear_server_removes_status_and_stderr_for_that_server() {
        // Given a session with two MCP servers carrying runtime data.
        let session_id = SessionId::new();
        let mut runtime = McpRuntimeState::default();
        runtime.set_status(&session_id, "weather", McpConnectionStatus::Running);
        runtime.set_stderr(&session_id, "weather", "ready".to_owned());
        runtime.set_status(&session_id, "search", McpConnectionStatus::Starting);
        runtime.set_stderr(&session_id, "search", "starting".to_owned());

        // When clearing one server.
        runtime.clear_server(&session_id, "weather");

        // Then only that server's status and stderr are absent.
        assert_eq!(runtime.status(&session_id, "weather"), None);
        assert_eq!(runtime.stderr(&session_id, "weather"), None);
        assert!(runtime.statuses(&session_id).contains_key("search"));
        assert!(runtime.stderr_tails(&session_id).contains_key("search"));
    }

    #[rstest::rstest]
    fn clear_session_removes_all_runtime_data() {
        // Given runtime data for two sessions.
        let first = SessionId::new();
        let second = SessionId::new();
        let mut runtime = McpRuntimeState::default();
        runtime.set_status(&first, "weather", McpConnectionStatus::Running);
        runtime.set_status(&second, "weather", McpConnectionStatus::Dead);

        // When clearing the first session.
        runtime.clear_session(&first);

        // Then its projection is empty and the other session remains.
        assert!(runtime.statuses(&first).is_empty());
        assert_eq!(
            runtime.status(&second, "weather"),
            Some(McpConnectionStatus::Dead)
        );
    }

    #[rstest::rstest]
    fn singleton_status_is_readable_without_a_session() {
        // Given an empty MCP runtime projection.
        let mut runtime = McpRuntimeState::default();

        // When setting one singleton server's status.
        runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);

        // Then that status reads back under the server name alone.
        assert_eq!(
            runtime.singleton_status("bundled-example"),
            Some(McpConnectionStatus::Running)
        );
    }

    #[rstest::rstest]
    fn clear_session_leaves_singleton_data_alone() {
        // Given a singleton server's runtime data.
        let mut runtime = McpRuntimeState::default();
        runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);
        runtime.set_singleton_stderr("bundled-example", "listening".to_owned());
        let session_id = SessionId::new();
        runtime.set_status(&session_id, "other", McpConnectionStatus::Dead);

        // When any session's runtime data is cleared.
        runtime.clear_session(&session_id);

        // Then the singleton's projections survive untouched.
        assert_eq!(
            runtime.singleton_status("bundled-example"),
            Some(McpConnectionStatus::Running)
        );
        assert_eq!(
            runtime.singleton_stderr("bundled-example"),
            Some("listening")
        );
    }

    #[rstest::rstest]
    fn clear_singleton_removes_only_that_server() {
        // Given two singleton servers carrying runtime data.
        let mut runtime = McpRuntimeState::default();
        runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);
        runtime.set_singleton_status("other", McpConnectionStatus::Dead);

        // When clearing one singleton server.
        runtime.clear_singleton("bundled-example");

        // Then its projections are gone and the other server's remain.
        assert_eq!(runtime.singleton_status("bundled-example"), None);
        assert_eq!(
            runtime.singleton_status("other"),
            Some(McpConnectionStatus::Dead)
        );
    }

    #[rstest::rstest]
    fn singleton_stderr_round_trips() {
        // Given an empty projection.
        let mut runtime = McpRuntimeState::default();

        // When writing a singleton stderr tail.
        runtime.set_singleton_stderr("bundled-example", "npm warn".to_owned());

        // Then the tail reads back under the server name.
        assert_eq!(
            runtime.singleton_stderr("bundled-example"),
            Some("npm warn")
        );
    }
}
