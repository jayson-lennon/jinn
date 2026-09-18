//! Builtin lifecycle commands - extending session lifecycles with compiled Rust handlers.
//!
//! Provides [`LifecycleCommand`] enum that supports both shell commands (backward compatible)
//! and builtin handlers identified by [`BuiltinId`]. The serde implementation ensures existing
//! TOML configs (bare string commands) continue to work.

use error_stack::Report;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::protocol::SessionId;

pub use jinn_preferences_config::schemas::BuiltinId;

/// Error type for builtin handler failures.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct BuiltinHandlerError;

/// A builtin lifecycle handler.
///
/// Each builtin lifecycle (e.g., bench tasks) registers a handler that provides
/// setup and teardown behavior. Setup returns a working directory path; teardown
/// performs cleanup and verification.
pub trait BuiltinHandler: Send + Sync {
    /// Returns a human-readable name for this handler, for debugging.
    fn name(&self) -> &'static str;

    /// Run setup for this builtin lifecycle.
    ///
    /// Returns the working directory path to set as the session's CWD.
    ///
    /// # Errors
    ///
    /// Returns an error if setup fails (e.g., fixture preparation fails).
    fn setup(
        &self,
        session_id: &SessionId,
        args: &[String],
    ) -> Result<PathBuf, Report<BuiltinHandlerError>>;

    /// Run teardown for this builtin lifecycle.
    ///
    /// Returns `true` if teardown succeeded, `false` if it failed.
    fn teardown(&self, session_id: &SessionId, args: &[String]) -> bool;
}

/// Registry of builtin lifecycle handlers, keyed by [`BuiltinId`].
///
/// Created empty and populated before the actor system starts. Passed to the
/// session actor via [`SessionPersistenceActorDeps`].
///
/// [`SessionPersistenceActorDeps`]: crate::feat::session::session_actor::SessionPersistenceActorDeps
#[derive(Clone, Default)]
pub struct BuiltinRegistry {
    handlers: HashMap<BuiltinId, Arc<dyn BuiltinHandler>>,
}

impl BuiltinRegistry {
    /// Creates a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a builtin handler under the given id.
    pub fn register<I>(&mut self, id: I, handler: Arc<dyn BuiltinHandler>)
    where
        I: Into<BuiltinId>,
    {
        self.handlers.insert(id.into(), handler);
    }

    /// Looks up a handler by id.
    #[must_use]
    pub fn get(&self, id: &BuiltinId) -> Option<&Arc<dyn BuiltinHandler>> {
        self.handlers.get(id)
    }

    /// Returns `true` if no handlers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl std::fmt::Debug for BuiltinRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinRegistry")
            .field("count", &self.handlers.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn registry_register_and_get_roundtrip() {
        // Given an empty registry.
        let mut registry = BuiltinRegistry::new();

        // When registering a handler.
        let handler = Arc::new(MockHandler);
        registry.register(BuiltinId("test-handler".to_owned()), handler);

        // Then get returns Some.
        let result = registry.get(&BuiltinId("test-handler".to_owned()));
        assert!(result.is_some());
    }

    #[rstest::rstest]
    #[test]
    fn registry_get_returns_none_for_unknown() {
        // Given a registry with one handler.
        let mut registry = BuiltinRegistry::new();
        registry.register(BuiltinId("known".to_owned()), Arc::new(MockHandler));

        // When looking up an unknown handler.
        let result = registry.get(&BuiltinId("unknown".to_owned()));

        // Then None is returned.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn registry_is_empty_true_when_no_handlers() {
        // Given an empty registry.
        let registry = BuiltinRegistry::new();

        // When checking is_empty.
        assert!(registry.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn registry_is_empty_false_after_register() {
        // Given a registry with one handler.
        let mut registry = BuiltinRegistry::new();
        registry.register(BuiltinId("test".to_owned()), Arc::new(MockHandler));

        // When checking is_empty.
        assert!(!registry.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn registry_debug_shows_count() {
        // Given a registry with two handlers.
        let mut registry = BuiltinRegistry::new();
        registry.register(BuiltinId("a".to_owned()), Arc::new(MockHandler));
        registry.register(BuiltinId("b".to_owned()), Arc::new(MockHandler));

        // When debugging.
        let debug_str = format!("{registry:?}");

        // Then the debug output contains "count" and a number.
        assert!(debug_str.contains("count"));
    }

    /// A minimal mock handler for testing the registry.
    struct MockHandler;

    impl BuiltinHandler for MockHandler {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn setup(
            &self,
            _session_id: &SessionId,
            _args: &[String],
        ) -> Result<PathBuf, Report<BuiltinHandlerError>> {
            Ok(PathBuf::from("/tmp/mock"))
        }

        fn teardown(&self, _session_id: &SessionId, _args: &[String]) -> bool {
            true
        }
    }
}
