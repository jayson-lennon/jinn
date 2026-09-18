//! Embedded configuration schemas for `jinn.toml` sections owned by other
//! features (prune, compaction, retry, lifecycles, projects, minimap, cwd
//! selector, plugins).
//!
//! Each submodule holds the pure serde *shape* of one config section; the
//! behavior that consumes it stays with its feature (kernel workers/actors
//! or slices). The [`UserPreferences`](crate::UserPreferences) aggregate
//! re-exports the section types at the crate root.

pub mod auto_prune;
pub mod compaction;
pub mod cwd_selector;
pub mod minimap;
pub mod plugin;
pub mod project;
pub mod request_retry;
pub mod session_lifecycle;

pub use auto_prune::{
    AnchorShieldConfig, AnchoredAssistantAutoPruneConfig, AutoPruneConfig,
    BrokenEditAutoPruneConfig, ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig,
    EditReadAutoPruneConfig, ReadEditAutoPruneConfig, RegexAutoPruneConfig, RegexPruneRule,
    TodoAutoPruneConfig, ToolAgeWindowAutoPruneConfig, TrivialAssistantAutoPruneConfig,
};
pub use compaction::CompactionConfig;
pub use cwd_selector::CwdSelectorConfig;
pub use minimap::MinimapConfig;
pub use plugin::{PluginConfig, PluginPathGrant};
pub use project::ProjectConfig;
pub use request_retry::RequestRetryConfig;
pub use session_lifecycle::{BuiltinId, LifecycleCommand, SessionLifecycle};
