//! Service wrapper for resolved API keys.
//!
//! Wraps [`ApiKeys`] in a shared, cheap-to-clone container.
//! All clones of [`ApiKeysService`] share the same underlying
//! key store via `Arc<RwLock<...>>`.

use std::sync::Arc;

use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;

use super::api_keys::ApiKeys;

/// Shared service wrapper for resolved API keys.
///
/// Wraps `ApiKeys` in an `Arc<RwLock<...>>` so that all clones
/// share the same data. Cloning is cheap - only an Arc refcount bump.
///
/// Newtype for discoverability: makes it easy to locate API key usage
/// across the codebase by searching for `ApiKeysService`.
///
/// Follows the project's service wrapper pattern.
#[derive(Debug, Clone)]
pub struct ApiKeysService {
    /// The wrapped key store, protected by an [`RwLock`] for shared access.
    inner: Arc<RwLock<ApiKeys>>,
}

impl ApiKeysService {
    /// Creates a new service wrapper around the given key store.
    #[must_use]
    pub fn new(keys: ApiKeys) -> Self {
        Self {
            inner: Arc::new(RwLock::new(keys)),
        }
    }

    /// Returns a read guard to the underlying key store.
    pub fn read(&self) -> RwLockReadGuard<'_, ApiKeys> {
        self.inner.read()
    }

    /// Looks up a key by its environment variable name.
    ///
    /// Acquires a read guard and returns a cloned value if found.
    #[must_use]
    pub fn get<S>(&self, env_var: S) -> Option<String>
    where
        S: AsRef<str>,
    {
        self.read().get(env_var.as_ref()).map(String::from)
    }

    /// Returns `true` if a non-empty key exists for the given env var name.
    #[must_use]
    pub fn is_set<S>(&self, env_var: S) -> bool
    where
        S: AsRef<str>,
    {
        self.read().is_set(env_var.as_ref())
    }

    /// Inserts a resolved key.
    ///
    /// Acquires a write guard to update the store.
    pub fn insert(&self, env_var: String, value: String) {
        self.inner.write().insert(env_var, value);
    }

    /// Replaces the whole key store.
    ///
    /// The live-reload path needs this shape, not per-key inserts: a
    /// provider removed from `providers.toml` must drop its key too, or
    /// availability checks keep reporting a provider that no longer
    /// exists. Clear-then-repopulate over the shared lock.
    pub fn replace(&self, keys: ApiKeys) {
        *self.inner.write() = keys;
    }

    /// Returns `true` if no keys have been resolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
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
    use crate::api_keys_service::ApiKeysService;

    #[rstest::rstest]
    fn clone_shares_data() {
        // Given a service with a key.
        let mut keys = crate::api_keys::ApiKeys::new();
        keys.insert("MY_KEY".to_owned(), "sk-secret".to_owned());
        let service = ApiKeysService::new(keys);
        let clone = service.clone();

        // When reading from both.
        // Then both see the same data.
        assert_eq!(service.get("MY_KEY"), Some("sk-secret".to_owned()));
        assert_eq!(clone.get("MY_KEY"), Some("sk-secret".to_owned()));
    }

    #[rstest::rstest]
    fn insert_updates_all_clones() {
        // Given two clones of the same service.
        let keys = crate::api_keys::ApiKeys::new();
        let service = ApiKeysService::new(keys);
        let clone = service.clone();

        // When inserting via one clone.
        clone.insert("NEW_KEY".to_owned(), "value".to_owned());

        // Then both clones see the new key.
        assert_eq!(service.get("NEW_KEY"), Some("value".to_owned()));
        assert_eq!(clone.get("NEW_KEY"), Some("value".to_owned()));
    }

    #[rstest::rstest]
    fn is_set_returns_true_when_key_present() {
        // Given a service wrapping a key store holding "MY_KEY".
        let mut keys = crate::api_keys::ApiKeys::new();
        keys.insert("MY_KEY".to_owned(), "sk-secret".to_owned());
        let service = ApiKeysService::new(keys);

        // When checking whether the key is set.
        // Then it reports set.
        assert!(service.is_set("MY_KEY"));
    }

    #[rstest::rstest]
    fn is_set_returns_false_when_key_absent() {
        // Given a service wrapping an empty key store.
        let keys = crate::api_keys::ApiKeys::new();
        let service = ApiKeysService::new(keys);

        // When checking whether an unknown key is set.
        // Then it reports unset.
        assert!(!service.is_set("NONEXISTENT"));
    }

    #[rstest::rstest]
    fn is_empty_returns_false_when_keys_present() {
        // Given a service wrapping a key store holding one key.
        let mut keys = crate::api_keys::ApiKeys::new();
        keys.insert("KEY".to_owned(), "val".to_owned());
        let service = ApiKeysService::new(keys);

        // When checking whether the store is empty.
        // Then it reports non-empty.
        assert!(!service.is_empty());
    }
}
