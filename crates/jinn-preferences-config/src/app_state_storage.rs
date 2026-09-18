//! App state storage abstraction - trait for app state I/O.
//!
//! Defines [`AppStateStorage`] as the abstraction for loading and saving
//! application runtime state. [`FilesystemAppStateStorage`] is the production
//! implementation; [`InMemoryAppStateStorage`] is for testing.

use std::path::PathBuf;
use std::sync::Arc;

use error_stack::{Report, ResultExt as _};
use parking_lot::RwLock;

use crate::app_state_file::{AppStateFile, AppStateFileError};

/// Trait for app state I/O.
///
/// Every external dependency must have a trait abstraction.
/// Filesystem I/O is an external dependency - this trait abstracts it so
/// tests can use in-memory storage instead of touching the real filesystem.
pub trait AppStateStorage: Send + Sync + 'static {
    /// Returns the storage backend name (for debugging).
    fn name(&self) -> &'static str;

    /// Reloads app state from the underlying storage.
    ///
    /// Always hits the underlying storage (filesystem, in-memory, etc.) — bypassing any
    /// cache in the service wrapper. Returns default state if none exist.
    ///
    /// # Errors
    ///
    /// Returns [`AppStateFileError::Io`] if the file cannot be read.
    /// Returns [`AppStateFileError::Parse`] if the TOML is malformed.
    fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>>;

    /// Saves app state.
    ///
    /// # Errors
    ///
    /// Returns [`AppStateFileError::Io`] if writing fails.
    /// Returns [`AppStateFileError::Parse`] if serialization fails.
    fn save(&self, state: &AppStateFile) -> Result<(), Report<AppStateFileError>>;
}

/// Filesystem-backed app state storage.
///
/// Reads from and writes to `state.toml` at a configurable path.
pub struct FilesystemAppStateStorage {
    /// Path to the state file.
    path: PathBuf,
}

impl FilesystemAppStateStorage {
    /// Creates a storage backed by an explicit path.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns the filesystem path this storage reads from and writes to.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl AppStateStorage for FilesystemAppStateStorage {
    fn name(&self) -> &'static str {
        "filesystem"
    }

    fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>> {
        crate::app_state_file::load_app_state_from(&self.path)
    }

    fn save(&self, state: &AppStateFile) -> Result<(), Report<AppStateFileError>> {
        crate::app_state_file::save_app_state_to(state, &self.path)
    }
}

/// In-memory app state storage for testing.
///
/// Stores the serialized TOML in memory. `reload()` returns the default
/// state if nothing has been saved. `save()` stores the serialized state.
pub struct InMemoryAppStateStorage {
    /// Serialized TOML content.
    content: Arc<RwLock<Option<String>>>,
}

impl InMemoryAppStateStorage {
    /// Creates an empty in-memory storage (loads default state).
    #[must_use]
    pub fn new() -> Self {
        Self {
            content: Arc::new(RwLock::new(None)),
        }
    }
}

impl Default for InMemoryAppStateStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStateStorage for InMemoryAppStateStorage {
    fn name(&self) -> &'static str {
        "in-memory"
    }

    fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>> {
        let guard = self.content.read();
        match guard.as_ref() {
            Some(content) => toml::from_str(content)
                .change_context(AppStateFileError::Parse)
                .attach("failed to parse in-memory app state"),
            None => Ok(AppStateFile::default()),
        }
    }

    fn save(&self, state: &AppStateFile) -> Result<(), Report<AppStateFileError>> {
        let content = toml::to_string_pretty(state)
            .change_context(AppStateFileError::Parse)
            .attach("failed to serialize app state")?;
        let mut guard = self.content.write();
        *guard = Some(content);
        Ok(())
    }
}

/// Service wrapper for app state storage.
///
/// Wraps `Arc<dyn AppStateStorage>` for shared ownership across the
/// application. Follows the service wrapper pattern from the project style guide.
/// Includes an in-memory cache so repeated reads don't hit disk.
#[derive(Debug, Clone)]
pub struct AppStateStorageService {
    /// The underlying app state storage implementation.
    svc: Arc<dyn AppStateStorage>,
    /// In-memory cache of the last loaded/saved state.
    cache: Arc<RwLock<Option<AppStateFile>>>,
}

impl AppStateStorageService {
    /// Creates a new app state storage service.
    #[must_use]
    pub fn new(storage: Arc<dyn AppStateStorage>) -> Self {
        Self {
            svc: storage,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Reads the cached app state.
    ///
    /// Infallible. Returns the value cached from the most recent successful
    /// `reload()` or `save()`. **Must** be called after a successful `reload()`
    /// (typically at app startup); panics with a programmer-error message otherwise.
    ///
    /// This method never touches the underlying storage.
    ///
    /// # Panics
    ///
    /// Panics if called before a successful `reload()` (or `save()`) has populated
    /// the cache.
    #[expect(clippy::expect_used, reason = "infallible")]
    pub fn read(&self) -> AppStateFile {
        self.cache
            .read()
            .clone()
            .expect("AppStateStorageService::read() called before reload() — programmer error: the service must be reloaded (typically at app startup) before any read")
    }

    /// Writes to storage and updates the in-memory cache.
    ///
    /// # Errors
    ///
    /// Returns [`AppStateFileError::Parse`] if serialization fails.
    /// Returns [`AppStateFileError::Io`] if writing fails.
    pub fn save(&self, state: &AppStateFile) -> Result<(), Report<AppStateFileError>> {
        self.svc.save(state)?;
        let mut guard = self.cache.write();
        *guard = Some(state.clone());
        Ok(())
    }

    /// Reloads state from the underlying storage, bypassing the cache.
    ///
    /// Reads fresh from storage and updates the cache:
    /// - On success, the cache is populated with the result.
    /// - On failure, the cache is **cleared** so that a subsequent `read()` panics.
    ///
    /// # Errors
    ///
    /// Returns [`AppStateFileError::Io`] if the underlying storage cannot be read.
    /// Returns [`AppStateFileError::Parse`] if the stored TOML is malformed.
    pub fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>> {
        let result = self.svc.reload();
        let mut guard = self.cache.write();
        match &result {
            Ok(state) => *guard = Some(state.clone()),
            Err(_) => *guard = None,
        }
        result
    }
}

impl std::fmt::Debug for dyn AppStateStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppStateStorage")
            .field("name", &self.name())
            .finish()
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

    use jinn_core_types::model_selection::ModelSelection;

    #[rstest::rstest]
    fn in_memory_load_returns_default_when_empty() {
        // Given an empty InMemoryAppStateStorage.
        let storage = InMemoryAppStateStorage::new();

        // When loading.
        let state = storage.reload().expect("reload");

        // Then defaults are returned.
        assert!(state.last_model.is_none());
    }

    #[rstest::rstest]
    fn in_memory_save_then_load_round_trips() {
        // Given an InMemoryAppStateStorage.
        let storage = InMemoryAppStateStorage::new();
        let state = AppStateFile {
            last_model: Some(ModelSelection::from_single("ollama/llama3".to_owned())),
            theme_name: None,
            persona_name: None,
            sidebar_width: Some(40),
            reasoning_effort: None,
        };

        // When saving and reloading.
        storage.save(&state).expect("save");
        let reloaded = storage.reload().expect("reload");

        // Then the round-tripped data matches.
        assert_eq!(
            reloaded.last_model,
            Some(ModelSelection::from_single("ollama/llama3".to_owned()))
        );
        assert_eq!(reloaded.sidebar_width, Some(40));
    }

    #[rstest::rstest]
    fn filesystem_load_returns_default_when_missing() {
        // Given a temp directory with no file.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        let storage = FilesystemAppStateStorage::new(path.clone());

        assert!(!path.exists());

        // When loading.
        let state = storage.reload().expect("reload");

        // Then defaults are returned AND the file was NOT auto-created.
        assert!(state.last_model.is_none());
        assert!(
            !path.exists(),
            "state file should not be auto-created on read"
        );
    }

    #[rstest::rstest]
    fn filesystem_save_then_load_round_trips() {
        // Given a FilesystemAppStateStorage in a temp dir.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        let storage = FilesystemAppStateStorage::new(path);

        let state = AppStateFile {
            last_model: Some(ModelSelection::from_single("test/model".to_owned())),
            theme_name: Some("gruvbox-dark".to_owned()),
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };

        // When saving and reloading.
        storage.save(&state).expect("save");
        let reloaded = storage.reload().expect("reload");

        // Then the round-tripped data matches.
        assert_eq!(
            reloaded.last_model,
            Some(ModelSelection::from_single("test/model".to_owned()))
        );
        assert_eq!(reloaded.theme_name.as_deref(), Some("gruvbox-dark"));
    }

    #[rstest::rstest]
    fn service_load_caches_result() {
        // Given an InMemoryAppStateStorage wrapped in a service.
        let storage = InMemoryAppStateStorage::new();
        let service = AppStateStorageService::new(Arc::new(storage));
        let state = AppStateFile {
            last_model: Some(ModelSelection::from_single("ollama/llama3".to_owned())),
            theme_name: None,
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };
        service.save(&state).expect("save");

        // When loading twice.
        let first = service.reload().expect("first reload");
        let second = service.reload().expect("second reload");

        // Then both return the same value.
        assert_eq!(first.last_model, second.last_model);
        assert_eq!(
            first.last_model,
            Some(ModelSelection::from_single("ollama/llama3".to_owned()))
        );
    }

    #[rstest::rstest]
    fn service_save_updates_cache() {
        // Given a service with cached state.
        let storage = InMemoryAppStateStorage::new();
        let service = AppStateStorageService::new(Arc::new(storage));
        let state = AppStateFile {
            last_model: Some(ModelSelection::from_single("ollama/llama3".to_owned())),
            theme_name: None,
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };
        service.save(&state).expect("save");

        // When saving new state.
        let updated = AppStateFile {
            last_model: Some(ModelSelection::from_single("openrouter/gpt-4".to_owned())),
            theme_name: None,
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };
        service.save(&updated).expect("save updated");

        // Then load returns the updated value.
        let loaded = service.reload().expect("reload");
        assert_eq!(
            loaded.last_model,
            Some(ModelSelection::from_single("openrouter/gpt-4".to_owned()))
        );
    }

    #[rstest::rstest]
    fn service_reload_clears_cache_and_reads_fresh() {
        // Given a service with cached state.
        let storage = InMemoryAppStateStorage::new();
        let service = AppStateStorageService::new(Arc::new(storage));
        let state = AppStateFile {
            last_model: Some(ModelSelection::from_single("ollama/llama3".to_owned())),
            theme_name: None,
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };
        service.save(&state).expect("save");

        // When reloading.
        let reloaded = service.reload().expect("reload");

        // Then fresh state is returned from storage.
        assert_eq!(
            reloaded.last_model,
            Some(ModelSelection::from_single("ollama/llama3".to_owned()))
        );
    }

    #[rstest::rstest]
    #[should_panic(expected = "AppStateStorageService::read() called before reload()")]
    fn read_before_reload_panics_with_precise_message() {
        // Given a service that has never been reloaded or saved.
        let storage = InMemoryAppStateStorage::new();
        let service = AppStateStorageService::new(Arc::new(storage));

        // When reading without prior reload.
        // Then panic with the documented programmer-error message.
        let _ = service.read();
    }

    #[rstest::rstest]
    fn read_after_reload_returns_cached_value() {
        // Given a service reloaded once.
        let storage = InMemoryAppStateStorage::new();
        let service = AppStateStorageService::new(Arc::new(storage));
        let first = service.reload().expect("initial reload");

        // When reading after reload.
        let second = service.read();

        // Then read returns the cached value.
        assert_eq!(first, second);
    }

    #[rstest::rstest]
    fn reload_failure_returns_err_and_leaves_cache_empty() {
        // Given a service backed by storage that always fails to reload.
        struct AlwaysFails;
        impl AppStateStorage for AlwaysFails {
            fn name(&self) -> &'static str {
                "always-fails"
            }
            fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>> {
                Err(Report::new(AppStateFileError::Parse))
            }
            fn save(&self, _: &AppStateFile) -> Result<(), Report<AppStateFileError>> {
                Ok(())
            }
        }
        let service = AppStateStorageService::new(Arc::new(AlwaysFails));

        // When reload is called.
        let result = service.reload();

        // Then it returns Err.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[should_panic(expected = "read() called before reload")]
    fn read_panics_after_failed_reload() {
        // Given a service backed by storage that always fails to reload.
        struct AlwaysFails;
        impl AppStateStorage for AlwaysFails {
            fn name(&self) -> &'static str {
                "always-fails"
            }
            fn reload(&self) -> Result<AppStateFile, Report<AppStateFileError>> {
                Err(Report::new(AppStateFileError::Parse))
            }
            fn save(&self, _: &AppStateFile) -> Result<(), Report<AppStateFileError>> {
                Ok(())
            }
        }
        let service = AppStateStorageService::new(Arc::new(AlwaysFails));

        // Given reload has failed (cache is empty).
        let _ = service.reload();

        // When read() is called.
        // Then it panics.
        let _ = service.read();
    }
}
