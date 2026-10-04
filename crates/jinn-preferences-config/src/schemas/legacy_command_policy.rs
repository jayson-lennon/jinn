//! The removed `[[tools.bash_command_policy]]` section, as read from an
//! existing `jinn.toml`.
//!
//! This is the value shape of a section that no longer exists. It is kept
//! only so a file written before the removal keeps working: `jinn-stream-rules`
//! reads it at load time and converts each rule into the stream rule that
//! replaced it, carrying the pattern, the message, and `on_trigger =
//! "fail_tool"` across.
//!
//! Nothing writes this key, nothing enforces it directly, and nothing else
//! should depend on it — it exists at the boundary with an older file, and
//! goes away when that boundary does.

use serde::{Deserialize, Serialize};

/// The `jinn.toml` key the removed global rules lived at.
///
/// Named only so the migration warning can quote the key it is replacing.
pub const LEGACY_COMMAND_POLICY_KEY: &str = "tools.bash_command_policy";

/// A legacy blocked-command rule: a regex and the message returned on a match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyCommandPolicyRule {
    /// Regex matched against the full command string.
    pub pattern: String,
    /// Message returned in the failed tool result when [`Self::pattern`] matches.
    pub message: String,
}

impl jinn_config::ConfigList for LegacyCommandPolicyRule {
    const KEY: &'static str = LEGACY_COMMAND_POLICY_KEY;
    const ENTRY_KEY: &'static str = "pattern";
    const ENTRY_FIELDS: &'static [&'static str] = &["pattern", "message"];
}

/// A `[[project.entry]]` as an older file wrote it, with its own rules.
///
/// Separate from the live [`ProjectConfig`](super::ProjectConfig) because the
/// `command_policy` field is gone from that struct — the rules became stream
/// rules scoped by their `project` field. This shape exists so the nested list
/// can still be deserialized out of an old file and converted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyProjectConfig {
    /// The absolute (or `~`-prefixed) directory path.
    pub path: std::path::PathBuf,
    /// The project's own blocked-command rules.
    #[serde(default)]
    pub command_policy: Vec<LegacyCommandPolicyRule>,
}

impl jinn_config::ConfigList for LegacyProjectConfig {
    const KEY: &'static str = "project.entry";
    const ENTRY_KEY: &'static str = "path";
    const ENTRY_FIELDS: &'static [&'static str] = &["path", "command_policy"];
}
