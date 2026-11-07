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
//!
//! # Behavior vocabulary
//!
//! Every section struct declares exactly one behavior in its docs:
//!
//! - **Snapshot Read** — the consumer owns nothing; it re-reads the value
//!   from the layer's current snapshot each time it does its job. An edit
//!   applies at the next natural use (next check, next compaction, next
//!   render).
//! - **Event-Dependent** — the consumer owns an artifact (a bound socket,
//!   a running process, a resolved binary, a registry) and declares a bus
//!   handler that rebuilds it when the config-watch slice publishes its
//!   reload event. Applies immediately or at the artifact's natural
//!   boundary.
//! - **Frozen** — a copy captured into the session profile at session
//!   creation; the live session never sees config edits to it, and the
//!   corresponding picker is its live-session control. A new session
//!   picks up the new value.
//!
//! Live reload itself is the config-watch slice's pipeline: a notify
//! event → a 250 ms debounce → a content-hash dedupe → a validate-gated
//! snapshot swap → the reload event. Snapshot Read sections ride the
//! swap alone; Event-Dependent sections add a bus handler; Frozen
//! sections ignore the pipeline entirely.

pub mod applets;
pub mod auto_prune;
pub mod bot;
pub mod canvas;
pub mod chat_log;
pub mod compaction;
pub mod cwd_selector;
pub mod dictation;
pub mod discord;
pub mod legacy_command_policy;
pub mod maintenance;
pub mod mcp;
pub mod minimap;
pub mod project;
pub mod provider;
pub mod provider_error_watchdog;
pub mod remote;
pub mod request_retry;
pub mod session_lifecycle;
pub mod shell;
pub mod skills;
pub mod stall_watchdog;
pub mod stream_rule_watchdog;
pub mod stream_rules;
pub mod stuck_loop_watchdog;
pub mod term;
pub mod tool_call_watchdog;
pub mod tools;

pub use applets::{AppletsBindError, AppletsConfig, DEFAULT_APPLETS_BIND};
pub use auto_prune::{
    AnchoredAssistantAutoPruneConfig, AutoPruneConfig, BrokenEditAutoPruneConfig,
    ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig, EditReadAutoPruneConfig,
    ReadEditAutoPruneConfig, RegexAutoPruneConfig, RegexPruneRule, TodoAutoPruneConfig,
    ToolAgeWindowAutoPruneConfig, TrivialAssistantAutoPruneConfig,
};
pub use bot::{BotEntryConfig, BotPinConfig, BotPinRole};
pub use canvas::{CanvasBindError, CanvasConfig, DEFAULT_CANVAS_BIND};
pub use chat_log::ChatLogConfig;
pub use compaction::CompactionConfig;
pub use cwd_selector::CwdSelectorConfig;
pub use dictation::DictationConfig;
pub use discord::DiscordConfig;
pub use legacy_command_policy::{
    LEGACY_COMMAND_POLICY_KEY, LegacyCommandPolicyRule, LegacyProjectConfig,
};
pub use maintenance::{MaintenanceConfig, retention_duration};
pub use mcp::{
    HeaderExpandError, McpServerConfig, McpServersConfig, TransportKind, expand_header_value,
    expand_mcp_headers, referenced_header_variables,
};
pub use minimap::MinimapConfig;
pub use project::ProjectConfig;
pub use provider::WebSearchConfig;
pub use provider_error_watchdog::ProviderErrorWatchdogConfig;
pub use remote::{DEFAULT_REMOTE_BIND, RemoteBindError, RemoteConfig};
pub use request_retry::RequestRetryConfig;
pub use session_lifecycle::{BuiltinId, LifecycleCommand, SessionLifecycle};
pub use shell::ShellConfig;
pub use skills::SkillsConfig;
pub use stall_watchdog::StallWatchdogConfig;
pub use stream_rule_watchdog::{STREAM_RULE_WATCHDOG_KEY, StreamRuleWatchdogConfig};
pub use stream_rules::{
    LegacyStreamRulesBudget, STREAM_RULES_BUDGET_KEY, STREAM_RULES_KEY, StreamRuleConfig,
    StreamRuleFileEntry, StreamRulesConfig, StreamRulesFile, parse_stream_rules_file,
};
pub use stuck_loop_watchdog::StuckLoopWatchdogConfig;
pub use term::{
    DEFAULT_CONTROL_TOGGLE_KEY, DEFAULT_SETTLE_MAX_WAIT_MS, DEFAULT_SETTLE_QUIET_MS,
    InteractiveTermPrefs,
};
pub use tool_call_watchdog::ToolCallWatchdogConfig;
pub use tools::ToolsConfig;
