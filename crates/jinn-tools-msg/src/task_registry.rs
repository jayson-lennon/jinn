//! In-flight subagent registry — tracks which parent sessions are blocked on a
//! `task` tool call, and which child sessions those calls spawned.
//!
//! The `task` tool registers each parent→child pair when it spawns the child
//! and removes the pair when the call resolves (success, failure, or abort).
//! The stall watchdog reads the registry to skip sessions that are healthy
//! but suspended waiting on a subagent — without this, a long-running child
//! would make its waiting parent look stalled.

#![allow(
    clippy::expect_used,
    reason = "poisoned registry lock is unrecoverable, matching session_map.rs"
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use jinn_core_types::SessionId;

/// Shared map of parent session → set of in-flight child sessions.
///
/// Cheap to clone (all clones share the same inner map). Written only by the
/// `task` tool through its [`TaskSpawnGuard`]; read by the stall watchdog.
#[derive(Debug, Clone, Default)]
pub struct TaskSpawnRegistry {
    inner: Arc<Mutex<HashMap<SessionId, HashSet<SessionId>>>>,
}

impl TaskSpawnRegistry {
    /// Records a parent→child pair for an in-flight `task` call.
    ///
    /// # Panics
    ///
    /// Panics if the registry lock is poisoned.
    pub fn register(&self, parent: SessionId, child: SessionId) {
        self.inner
            .lock()
            .expect("task spawn registry poisoned")
            .entry(parent)
            .or_default()
            .insert(child);
    }

    /// Removes a parent→child pair. Idempotent: removing an unknown pair
    /// (or one already removed) is a no-op.
    ///
    /// # Panics
    ///
    /// Panics if the registry lock is poisoned.
    pub fn unregister(&self, parent: &SessionId, child: &SessionId) {
        let mut map = self.inner.lock().expect("task spawn registry poisoned");
        if let Some(children) = map.get_mut(parent) {
            children.remove(child);
            if children.is_empty() {
                map.remove(parent);
            }
        }
    }

    /// Whether the given session has at least one in-flight `task` call.
    ///
    /// # Panics
    ///
    /// Panics if the registry lock is poisoned.
    pub fn has_in_flight(&self, parent: &SessionId) -> bool {
        self.inner
            .lock()
            .expect("task spawn registry poisoned")
            .contains_key(parent)
    }
}

/// Drop-guard for a registered parent→child pair.
///
/// Created by [`TaskSpawnRegistry::guard`] after registration; unregisters the
/// pair when dropped. This covers both the normal completion path (the tool
/// drops the guard after forwarding the result) and the abort path (the parent
/// tool-call future is cancelled while awaiting the child).
#[derive(Debug)]
pub struct TaskSpawnGuard {
    registry: TaskSpawnRegistry,
    parent: SessionId,
    child: SessionId,
}

impl TaskSpawnGuard {
    /// Unregisters the pair early, before the guard is dropped.
    ///
    /// Consumes the guard so the eventual `Drop` becomes a no-op.
    pub fn defuse(self) {
        // Registration is removed here; Drop then holds nothing.
        self.registry.unregister(&self.parent, &self.child);
    }
}

impl Drop for TaskSpawnGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.parent, &self.child);
    }
}

impl TaskSpawnRegistry {
    /// Registers the pair and returns a guard that unregisters it on drop.
    pub fn guard(&self, parent: SessionId, child: SessionId) -> TaskSpawnGuard {
        self.register(parent.clone(), child.clone());
        TaskSpawnGuard {
            registry: self.clone(),
            parent,
            child,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "no slicing in this module")]

    use super::TaskSpawnRegistry;
    use jinn_core_types::SessionId;

    #[rstest::rstest]
    #[test]
    fn has_in_flight_reflects_registration() {
        // Given an empty registry.
        let registry = TaskSpawnRegistry::default();
        let parent = SessionId::new();
        let child = SessionId::new();

        // Then the parent has nothing in flight.
        assert!(!registry.has_in_flight(&parent));

        // When registering a pair.
        let _guard = registry.guard(parent.clone(), child);

        // Then the parent has an in-flight task.
        assert!(registry.has_in_flight(&parent));
    }

    #[rstest::rstest]
    #[test]
    fn guard_drop_unregisters_pair() {
        // Given a registry with a registered pair.
        let registry = TaskSpawnRegistry::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        {
            let _guard = registry.guard(parent.clone(), child);
            assert!(registry.has_in_flight(&parent));
        }

        // Then dropping the guard unregisters it.
        assert!(!registry.has_in_flight(&parent));
    }

    #[rstest::rstest]
    #[test]
    fn defused_guard_does_not_double_unregister() {
        // Given a guard that was defused after normal completion.
        let registry = TaskSpawnRegistry::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        let guard = registry.guard(parent.clone(), child);
        guard.defuse();

        // Then the pair is gone while the guard is still alive.
        assert!(!registry.has_in_flight(&parent));
    }

    #[rstest::rstest]
    #[test]
    fn unregister_is_idempotent_for_unknown_pairs() {
        // Given an empty registry.
        let registry = TaskSpawnRegistry::default();
        let parent = SessionId::new();
        let child = SessionId::new();

        // When unregistering a pair that was never registered.
        registry.unregister(&parent, &child);

        // Then nothing panics and the registry stays empty.
        assert!(!registry.has_in_flight(&parent));
    }

    #[rstest::rstest]
    #[test]
    fn sibling_children_are_tracked_independently() {
        // Given a parent with two in-flight children.
        let registry = TaskSpawnRegistry::default();
        let parent = SessionId::new();
        let child_a = SessionId::new();
        let child_b = SessionId::new();
        let guard_a = registry.guard(parent.clone(), child_a.clone());
        let guard_b = registry.guard(parent.clone(), child_b);

        // When one child resolves.
        guard_a.defuse();

        // Then the parent still has the other in flight.
        assert!(registry.has_in_flight(&parent));
        // And when the second resolves too, the parent entry disappears.
        guard_b.defuse();
        assert!(!registry.has_in_flight(&parent));
    }
}
