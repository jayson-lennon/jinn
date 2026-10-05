//! Every `jinn.toml` section type, one module per section.
//!
//! The rule: a section's *declaration* lives here, wherever the feature
//! that runs it lives. Each submodule holds the pure serde *shape* of one
//! section plus its `Configurable` or `ConfigList` impl, which is the
//! whole declaration — the key it owns and, for a list, the field
//! identifying an entry.
//!
//! Two conventions follow from keeping every section in one crate:
//!
//! - A section's **value vocabulary** lives in the same module as the
//!   section, even when that type declares no trait of its own. The
//!   `[mcp]` section's `McpServerConfig` and `TransportKind` are its
//!   payload, not sections, and split across crates they would drift.
//! - The **behavior** that reads a section stays with the feature that
//!   runs it — kernel workers, or the slice that owns the runtime. A
//!   compiled rule matcher or an MCP connection belongs to its slice; only
//!   the data comes from here.
//!
//! Consumers read a section through the configuration layer
//! (`config.get::<T>()` / `get_list::<T>()`), so a section type is pure
//! data with no dependency back on the code that reads it.

pub mod attendant;
pub mod auto_prune;
pub mod chat_log;
pub mod compaction;
pub mod cwd_selector;
pub mod discord;
pub mod legacy_command_policy;
pub mod mcp;
pub mod minimap;
pub mod project;
pub mod provider;
pub mod request_retry;
pub mod session_lifecycle;
pub mod skills;
pub mod stall_watchdog;
pub mod stream_rules;
pub mod term;
pub mod tool_call_watchdog;
pub mod tools;

pub use attendant::{AttendantEntryConfig, AttendantPinConfig, AttendantPinRole};
pub use auto_prune::{
    AnchoredAssistantAutoPruneConfig, AutoPruneConfig, BrokenEditAutoPruneConfig,
    ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig, EditReadAutoPruneConfig,
    ReadEditAutoPruneConfig, RegexAutoPruneConfig, RegexPruneRule, TodoAutoPruneConfig,
    ToolAgeWindowAutoPruneConfig, TrivialAssistantAutoPruneConfig,
};
pub use chat_log::ChatLogConfig;
pub use compaction::CompactionConfig;
pub use cwd_selector::CwdSelectorConfig;
pub use discord::DiscordConfig;
pub use legacy_command_policy::{
    LEGACY_COMMAND_POLICY_KEY, LegacyCommandPolicyRule, LegacyProjectConfig,
};
pub use mcp::{
    HeaderExpandError, McpServerConfig, McpServersConfig, TransportKind, expand_header_value,
    expand_mcp_headers, referenced_header_variables,
};
pub use minimap::MinimapConfig;
pub use project::ProjectConfig;
pub use provider::WebSearchConfig;
pub use request_retry::RequestRetryConfig;
pub use session_lifecycle::{BuiltinId, LifecycleCommand, SessionLifecycle};
pub use skills::SkillsConfig;
pub use stall_watchdog::StallWatchdogConfig;
pub use stream_rules::{STREAM_RULES_KEY, StreamRuleConfig};
pub use term::{
    DEFAULT_CONTROL_TOGGLE_KEY, DEFAULT_SETTLE_MAX_WAIT_MS, DEFAULT_SETTLE_QUIET_MS,
    InteractiveTermPrefs,
};
pub use tool_call_watchdog::ToolCallWatchdogConfig;
pub use tools::ToolsConfig;
