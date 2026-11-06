//! `state.toml` schema, the `jinn.toml` default-config bootstrap, and
//! every `jinn.toml` section type.
//!
//! `jinn.toml` itself has no aggregate struct: each section is declared
//! by a [`jinn_config::Configurable`] or [`jinn_config::ConfigList`] impl
//! in [`schemas`], and read through the configuration layer. What
//! remains here is the default-config template used to seed a fresh
//! install.
//!
//! Everything here is *data*: the actor that applies `state.toml` types
//! to `AppState` lives in the `jinn-preferences` slice, and the workers
//! and slices that read the embedded config sections (prune rules,
//! compaction, retry, MCP servers) keep their behavior in the kernel and
//! their own crates, importing the shapes from here.
//!
//! Kernel-free by design: the layer is handed to `jinn-slices`, so this
//! crate must not depend on the kernel or on any slice that depends on
//! it. That constraint is why sections are centralized here rather than
//! co-located with the code that runs them.

pub mod app_state_file;
pub mod app_state_storage;
pub mod config_template;

pub mod protocol;
pub mod registration;
pub mod schemas;
pub mod stream_rules_assembly;
pub mod stream_rules_storage;
#[cfg(test)]
mod template_validation_tests;

pub use app_state_file::{AppStateFile, AppStateFileError, load_app_state_from, save_app_state_to};
pub use app_state_storage::{
    AppStateStorage, AppStateStorageService, FilesystemAppStateStorage, InMemoryAppStateStorage,
};
pub use config_template::{
    DEFAULT_CONFIG, InitDefaultConfigError, InitOutcome, STREAM_RULES_FILE_NAME,
    STREAM_RULES_TEMPLATE, StreamRulesSeedOutcome, UserPreferencesError,
    create_default_preferences_to, init_default_config_to, init_stream_rules_for_install,
    init_stream_rules_to, preferences_path,
};
pub use registration::register_all_sections;
pub use stream_rules_assembly::assemble_stream_rules;
pub use stream_rules_storage::{StreamRulesStorage, StreamRulesStorageError};
// The stream-rule schemas: the entry list and the disabled-name array share
// the `[stream_rules]` section, so the write side reaches both through one
// import home.
pub use schemas::StreamRulesConfig;
// The `todo` + `anchored_assistant` auto-prune child configs are schema
// types re-exported so the kernel's workers and every consumer keep one
// import home for the `[context_curation.auto_prune]` section.
pub use schemas::auto_prune::{AnchoredAssistantAutoPruneConfig, TodoAutoPruneConfig};
// Watchdog sections: consumed by the `jinn-watchdog` slice at activation.
pub use schemas::ProviderErrorWatchdogConfig;
pub use schemas::StallWatchdogConfig;
pub use schemas::ToolCallWatchdogConfig;
pub use schemas::maintenance::{MaintenanceConfig, retention_duration};

// The configuration layer lives in its own kernel-free crate so
// `jinn-slices` can hold one without a dependency cycle. Re-exported here
// because this is where a consumer already looks for configuration.
pub use jinn_config::ConfigLayer;
