//! User preferences and app-state persistence schemas.
//!
//! The kernel-free home for the `jinn.toml` ([`UserPreferences`]) and
//! `state.toml` ([`AppStateFile`]) file schemas, their storage traits with
//! Filesystem/InMemory backends, the default-config bootstrap helpers, and
//! the preferences/app-state bus protocol types.
//!
//! Everything here is *data*: the actors that apply these types to
//! `AppState` live in the `jinn-preferences` slice, and the workers that
//! read the embedded config schemas (prune rules, compaction, retry)
//! keep their behavior in the kernel and import the shapes from here.

#[cfg(test)]
mod template_validation_tests;

pub mod app_state_file;
pub mod app_state_storage;
pub mod user_preferences;
pub mod user_preferences_storage;

pub mod protocol;
pub mod schemas;

pub use app_state_file::{AppStateFile, AppStateFileError, load_app_state_from, save_app_state_to};
pub use app_state_storage::{
    AppStateStorage, AppStateStorageService, FilesystemAppStateStorage, InMemoryAppStateStorage,
};
pub use user_preferences::{
    DEFAULT_CONFIG, InitDefaultConfigError, InitOutcome, OpenrouterWebSearchConfig,
    UserPreferences, UserPreferencesError, default_tool_default_timeout_secs,
    init_default_config_to, load_preferences, load_preferences_from, normalize_legacy_keys,
    preferences_path, save_preferences, save_preferences_to,
};
pub use user_preferences_storage::{
    FilesystemUserPreferencesStorage, InMemoryUserPreferencesStorage, UserPreferencesStorage,
    UserPreferencesStorageService,
};
