//! Auto-prune configuration schemas — the `jinn.toml` `[auto_prune]` section.
//!
//! One config struct per pruning strategy plus the [`AutoPruneConfig`]
//! aggregate. Pure serde data: the pruning *behavior* (the workers that
//! consume these settings) stays in the kernel's auto-prune feature; only
//! the shapes moved here so `jinn.toml` can be parsed kernel-free.

use serde::{Deserialize, Serialize};

/// Default token threshold at which accumulated pruner context-override
/// mutations flush. Default: 150 000.
pub const DEFAULT_ACCUMULATION_THRESHOLD_TOKENS: u32 = 150_000;

/// Default token threshold for the accumulation flush (serde default fn).
fn default_accumulation_threshold_tokens() -> u32 {
    DEFAULT_ACCUMULATION_THRESHOLD_TOKENS
}

/// Default: regex tool name (`bash`).
const DEFAULT_REGEX_TOOL_NAME: &str = "bash";
/// Default: regex keep-last count (1).
const DEFAULT_REGEX_KEEP_LAST: usize = 1;
/// Default: regex strategy enabled (true).
const DEFAULT_REGEX_ENABLED: bool = true;
/// Default: regex min-age (50).
const DEFAULT_REGEX_MIN_AGE: usize = 50;

/// Default: edit-read strategy enabled (true).
const DEFAULT_EDIT_READ_ENABLED: bool = true;
/// Default: edit-read min-age (50).
const DEFAULT_EDIT_READ_MIN_AGE: usize = 50;

/// Default: read-edit strategy enabled (true).
const DEFAULT_READ_EDIT_ENABLED: bool = true;
/// Default: read-edit min-age (50).
const DEFAULT_READ_EDIT_MIN_AGE: usize = 50;
/// Default: read-edit threshold (2).
const DEFAULT_READ_EDIT_THRESHOLD: usize = 2;

/// Default: broken-edit strategy enabled (true).
const DEFAULT_BROKEN_EDIT_ENABLED: bool = true;
/// Default: broken-edit min-age (10).
const DEFAULT_BROKEN_EDIT_MIN_AGE: usize = 10;

/// Default: todo strategy enabled (true).
const DEFAULT_TODO_ENABLED: bool = true;
/// Default: todo min-age (50).
const DEFAULT_TODO_MIN_AGE: usize = 50;

/// Default: double-edit max file edits (2).
const DEFAULT_DOUBLE_EDIT_MAX_FILE_EDITS: usize = 2;
/// Default: double-edit strategy enabled (true).
const DEFAULT_DOUBLE_EDIT_ENABLED: bool = true;
/// Default: double-edit min-age (20).
const DEFAULT_DOUBLE_EDIT_MIN_AGE: usize = 20;

/// Default: consecutive-reads keep-last (5).
const DEFAULT_CONSECUTIVE_READS_KEEP_LAST: usize = 5;
/// Default: consecutive-reads strategy enabled (true).
const DEFAULT_CONSECUTIVE_READS_ENABLED: bool = true;
/// Default: consecutive-reads min-age (80).
const DEFAULT_CONSECUTIVE_READS_MIN_AGE: usize = 80;

/// Default: tool-age-window strategy enabled (true).
const DEFAULT_TOOL_AGE_WINDOW_ENABLED: bool = true;
/// Default: tool-age-window min-age (150).
const DEFAULT_TOOL_AGE_WINDOW_MIN_AGE: usize = 150;

/// Default: trivial-assistant strategy enabled (true).
const DEFAULT_TRIVIAL_ASSISTANT_ENABLED: bool = true;
/// Default: trivial-assistant min-age (100).
const DEFAULT_TRIVIAL_ASSISTANT_MIN_AGE: usize = 100;
/// Default: trivial-assistant max tokens (80).
const DEFAULT_TRIVIAL_ASSISTANT_MAX_TOKENS: usize = 80;

/// Default: anchored-assistant strategy enabled (true).
const DEFAULT_ANCHORED_ASSISTANT_ENABLED: bool = true;
/// Default: anchored-assistant radius (100).
const DEFAULT_ANCHORED_ASSISTANT_RADIUS: usize = 100;
/// Default: anchored-assistant min-age (50).
const DEFAULT_ANCHORED_ASSISTANT_MIN_AGE: usize = 50;

/// Default: anchor-shield strategy enabled (true).
const DEFAULT_ANCHOR_SHIELD_ENABLED: bool = true;
/// Default: anchor-shield radius (20).
const DEFAULT_ANCHOR_SHIELD_RADIUS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditReadAutoPruneConfig {
    #[serde(default = "default_edit_read_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history that must
    /// appear after an edit/write call before it may be pruned when
    /// a same-file read follows. Counts every entry, regardless of
    /// in-context status. Set to 0 to disable protection.
    /// Default: 50.
    #[serde(default = "default_edit_read_min_age")]
    pub min_age: usize,
}

fn default_edit_read_enabled() -> bool {
    DEFAULT_EDIT_READ_ENABLED
}

fn default_edit_read_min_age() -> usize {
    DEFAULT_EDIT_READ_MIN_AGE
}

impl Default for EditReadAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_EDIT_READ_ENABLED,
            min_age: DEFAULT_EDIT_READ_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadEditAutoPruneConfig {
    #[serde(default = "default_read_edit_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history within which
    /// read call+result pairs are protected from pruning.
    /// Counts every entry, regardless of in-context status.
    /// Set to 0 to disable protection.
    /// Default: 50.
    #[serde(default = "default_read_edit_min_age")]
    pub min_age: usize,
    /// Number of edit/write operations on the same file required before
    /// pruning the prior read call+result pair.
    /// Default: 2.
    #[serde(default = "default_read_edit_threshold")]
    pub threshold: usize,
}

fn default_read_edit_enabled() -> bool {
    DEFAULT_READ_EDIT_ENABLED
}

fn default_read_edit_min_age() -> usize {
    DEFAULT_READ_EDIT_MIN_AGE
}

fn default_read_edit_threshold() -> usize {
    DEFAULT_READ_EDIT_THRESHOLD
}

impl Default for ReadEditAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_READ_EDIT_ENABLED,
            min_age: DEFAULT_READ_EDIT_MIN_AGE,
            threshold: DEFAULT_READ_EDIT_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegexPruneRule {
    /// Regex pattern to match against the tool call's text output.
    /// The regex is tested against `"{name}: {arguments}"`.
    pub pattern: String,
    /// Tool name to filter by. Only tool calls with this name are considered.
    /// Default: `"bash"`.
    #[serde(default = "default_regex_tool_name")]
    pub tool_name: String,
    /// Number of most recent matching pairs to keep in context.
    /// Minimum 1 (clamped at worker construction).
    /// Default: 1.
    #[serde(default = "default_regex_keep_last")]
    pub keep_last: usize,
    /// Raw-distance protection floor: matching pairs whose `ToolCall` is within
    /// `min_age` slots of the end of history are never pruned by this rule.
    /// With `min_age = 0` no pair is protected (back-compat baseline).
    /// Default: 50.
    #[serde(default = "default_regex_min_age")]
    pub min_age: usize,
}

pub fn default_regex_tool_name() -> String {
    DEFAULT_REGEX_TOOL_NAME.to_owned()
}

pub fn default_regex_keep_last() -> usize {
    DEFAULT_REGEX_KEEP_LAST
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegexAutoPruneConfig {
    /// Whether the regex auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_regex_enabled")]
    pub enabled: bool,
    /// List of regex prune rules.
    /// Default: empty (no rules).
    #[serde(default)]
    pub rules: Vec<RegexPruneRule>,
}

pub fn default_regex_min_age() -> usize {
    DEFAULT_REGEX_MIN_AGE
}

impl Default for RegexAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_REGEX_ENABLED,
            rules: vec![
                RegexPruneRule {
                    pattern: "cargo test".to_owned(),
                    tool_name: DEFAULT_REGEX_TOOL_NAME.to_owned(),
                    keep_last: 2,
                    min_age: DEFAULT_REGEX_MIN_AGE,
                },
                RegexPruneRule {
                    pattern: "cargo check".to_owned(),
                    tool_name: DEFAULT_REGEX_TOOL_NAME.to_owned(),
                    keep_last: 1,
                    min_age: DEFAULT_REGEX_MIN_AGE,
                },
                RegexPruneRule {
                    pattern: "cargo clippy".to_owned(),
                    tool_name: DEFAULT_REGEX_TOOL_NAME.to_owned(),
                    keep_last: 1,
                    min_age: DEFAULT_REGEX_MIN_AGE,
                },
            ],
        }
    }
}

fn default_regex_enabled() -> bool {
    DEFAULT_REGEX_ENABLED
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokenEditAutoPruneConfig {
    /// Whether the broken-edit auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_broken_edit_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history that must
    /// appear after the failed edit ToolCall before the call+result
    /// pair may be pruned. Counts every entry, regardless of in-context
    /// status. Set to 0 to disable protection.
    /// Default: 10.
    #[serde(default = "default_broken_edit_min_age", alias = "min_tail_entries")]
    pub min_age: usize,
}

fn default_broken_edit_enabled() -> bool {
    DEFAULT_BROKEN_EDIT_ENABLED
}

fn default_broken_edit_min_age() -> usize {
    DEFAULT_BROKEN_EDIT_MIN_AGE
}

impl Default for BrokenEditAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_BROKEN_EDIT_ENABLED,
            min_age: DEFAULT_BROKEN_EDIT_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoAutoPruneConfig {
    /// Whether the todo auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_todo_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history that must
    /// appear after a todo tool call before pruning may exclude the
    /// call+result pair. Counts every entry, regardless of in-context
    /// status. Set to 0 to disable protection.
    /// Default: 50.
    #[serde(default = "default_todo_min_age")]
    pub min_age: usize,
}

fn default_todo_enabled() -> bool {
    DEFAULT_TODO_ENABLED
}

fn default_todo_min_age() -> usize {
    DEFAULT_TODO_MIN_AGE
}

impl Default for TodoAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_TODO_ENABLED,
            min_age: DEFAULT_TODO_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubleEditAutoPruneConfig {
    /// Whether the double-edit auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_double_edit_enabled")]
    pub enabled: bool,
    /// Maximum number of edit/write tool call+result pairs to keep per file path.
    /// Oldest pairs are pruned when this limit is exceeded.
    /// Set to 0 to disable pruning (no limit).
    /// Default: 2.
    #[serde(default = "default_double_edit_max_file_edits")]
    pub max_file_edits: usize,
    /// Minimum number of entries from the end of history that must
    /// appear after an edit/write call before it may be pruned.
    /// Counts every entry, regardless of in-context status.
    /// Set to 0 to disable protection (preserves pre-`min_age` behavior).
    /// Default: 20.
    #[serde(default = "default_double_edit_min_age")]
    pub min_age: usize,
}

fn default_double_edit_enabled() -> bool {
    DEFAULT_DOUBLE_EDIT_ENABLED
}

fn default_double_edit_max_file_edits() -> usize {
    DEFAULT_DOUBLE_EDIT_MAX_FILE_EDITS
}

fn default_double_edit_min_age() -> usize {
    DEFAULT_DOUBLE_EDIT_MIN_AGE
}

impl Default for DoubleEditAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_DOUBLE_EDIT_ENABLED,
            max_file_edits: DEFAULT_DOUBLE_EDIT_MAX_FILE_EDITS,
            min_age: DEFAULT_DOUBLE_EDIT_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsecutiveReadsAutoPruneConfig {
    /// Whether the consecutive-reads auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_consecutive_reads_enabled")]
    pub enabled: bool,
    /// Number of most recent `read` tool call+result pairs to keep per file path.
    /// Older pairs are pruned when this limit is exceeded.
    /// Minimum 1 (clamped during worker construction).
    /// Default: 3.
    #[serde(default = "default_consecutive_reads_keep_last")]
    pub keep_last: usize,
    /// Minimum number of entries from the end of history within which
    /// read pairs are protected from pruning even when they would
    /// otherwise be pruned by `keep_last`. Counts every entry, regardless
    /// of in-context status. Set to 0 to disable protection.
    /// Default: `50`.
    #[serde(default = "default_consecutive_reads_min_age")]
    pub min_age: usize,
}

fn default_consecutive_reads_enabled() -> bool {
    DEFAULT_CONSECUTIVE_READS_ENABLED
}

fn default_consecutive_reads_keep_last() -> usize {
    DEFAULT_CONSECUTIVE_READS_KEEP_LAST
}

fn default_consecutive_reads_min_age() -> usize {
    DEFAULT_CONSECUTIVE_READS_MIN_AGE
}

impl Default for ConsecutiveReadsAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_CONSECUTIVE_READS_ENABLED,
            keep_last: DEFAULT_CONSECUTIVE_READS_KEEP_LAST,
            min_age: DEFAULT_CONSECUTIVE_READS_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolAgeWindowAutoPruneConfig {
    /// Whether the tool-age-window auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_tool_age_window_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history within which
    /// `ToolCall`/`ToolResult` pairs are protected from pruning.
    /// Counts every entry, regardless of in-context status.
    /// Minimum 1 (clamped at worker construction).
    /// Default: 100.
    #[serde(default = "default_tool_age_window_min_age")]
    pub min_age: usize,
}

fn default_tool_age_window_enabled() -> bool {
    DEFAULT_TOOL_AGE_WINDOW_ENABLED
}

fn default_tool_age_window_min_age() -> usize {
    DEFAULT_TOOL_AGE_WINDOW_MIN_AGE
}

impl Default for ToolAgeWindowAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_TOOL_AGE_WINDOW_ENABLED,
            min_age: DEFAULT_TOOL_AGE_WINDOW_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrivialAssistantAutoPruneConfig {
    /// Whether the trivial-assistant auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_trivial_assistant_enabled")]
    pub enabled: bool,
    /// Minimum number of entries from the end of history within which
    /// assistant entries are protected from pruning even when they would
    /// otherwise qualify as trivial. Counts every entry, regardless of
    /// in-context status. Set to 0 to disable protection.
    /// Default: `50`.
    #[serde(
        default = "default_trivial_assistant_min_age",
        alias = "max_age_entries"
    )]
    pub min_age: usize,
    /// Maximum number of tokens (tiktoken `o200k_base`) below which an
    /// `Assistant` entry is considered trivial. Minimum 1 (clamped at
    /// evaluation time).
    /// Default: `80`.
    #[serde(default = "default_trivial_assistant_max_tokens")]
    pub max_tokens: usize,
}

fn default_trivial_assistant_enabled() -> bool {
    DEFAULT_TRIVIAL_ASSISTANT_ENABLED
}

fn default_trivial_assistant_min_age() -> usize {
    DEFAULT_TRIVIAL_ASSISTANT_MIN_AGE
}

fn default_trivial_assistant_max_tokens() -> usize {
    DEFAULT_TRIVIAL_ASSISTANT_MAX_TOKENS
}

impl Default for TrivialAssistantAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_TRIVIAL_ASSISTANT_ENABLED,
            min_age: DEFAULT_TRIVIAL_ASSISTANT_MIN_AGE,
            max_tokens: DEFAULT_TRIVIAL_ASSISTANT_MAX_TOKENS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchoredAssistantAutoPruneConfig {
    /// Whether the anchored-assistant auto-prune worker is active.
    /// Default: `true`.
    #[serde(default = "default_anchored_assistant_enabled")]
    pub enabled: bool,
    /// **Deprecated:** the radius value is now sourced from `AnchorShieldConfig.radius`.
    /// This field is kept for backward compatibility with existing `jinn.toml` files
    /// but its value is ignored at wiring time.
    ///
    /// Radius (in raw chat entries) within which an `Assistant` entry is
    /// protected from pruning, regardless of distance to any User entry.
    /// Distance strictly greater than this radius marks the entry as a
    /// prune candidate (subject to the `>80` token threshold).
    /// Minimum 1 (clamped at evaluation time).
    /// Default: `100`.
    #[serde(default = "default_anchored_assistant_radius")]
    pub radius: usize,
    /// Minimum number of entries from the end of history within which
    /// Assistant entries are protected from pruning even when both anchor
    /// distances exceed the radius. Counts every entry, regardless of
    /// in-context status. Set to 0 to disable protection.
    /// Default: `50`.
    #[serde(default = "default_anchored_assistant_min_age")]
    pub min_age: usize,
}

fn default_anchored_assistant_enabled() -> bool {
    DEFAULT_ANCHORED_ASSISTANT_ENABLED
}

fn default_anchored_assistant_radius() -> usize {
    DEFAULT_ANCHORED_ASSISTANT_RADIUS
}

fn default_anchored_assistant_min_age() -> usize {
    DEFAULT_ANCHORED_ASSISTANT_MIN_AGE
}

impl Default for AnchoredAssistantAutoPruneConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_ANCHORED_ASSISTANT_ENABLED,
            radius: DEFAULT_ANCHORED_ASSISTANT_RADIUS,
            min_age: DEFAULT_ANCHORED_ASSISTANT_MIN_AGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchorShieldConfig {
    /// Whether the anchor-shield worker is active.
    /// Default: `true`.
    #[serde(default = "default_anchor_shield_enabled")]
    pub enabled: bool,
    /// Radius (in raw chat entries) within which in-context entries
    /// are shielded from exclusion by other workers.
    /// This value is also used by the `AnchoredAssistantAutoPruneWorker`
    /// so the shield boundary and prune boundary always align.
    /// Minimum 1 (clamped at evaluation time).
    /// Default: `20`.
    #[serde(default = "default_anchor_shield_radius")]
    pub radius: usize,
}

fn default_anchor_shield_enabled() -> bool {
    DEFAULT_ANCHOR_SHIELD_ENABLED
}

fn default_anchor_shield_radius() -> usize {
    DEFAULT_ANCHOR_SHIELD_RADIUS
}

impl Default for AnchorShieldConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_ANCHOR_SHIELD_ENABLED,
            radius: DEFAULT_ANCHOR_SHIELD_RADIUS,
        }
    }
}

/// Auto-prune configuration.
///
/// Serialized as `[auto_prune]` in `jinn.toml`.
/// Groups all auto-prune strategy configurations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoPruneConfig {
    /// Edit-read auto-prune strategy configuration.
    #[serde(default)]
    pub edit_read: EditReadAutoPruneConfig,
    /// Read-edit auto-prune strategy configuration.
    #[serde(default)]
    pub read_edit: ReadEditAutoPruneConfig,
    /// Regex-based auto-prune strategy configuration.
    #[serde(default)]
    pub regex: RegexAutoPruneConfig,
    /// Broken-edit auto-prune strategy configuration.
    #[serde(default)]
    pub broken_edit: BrokenEditAutoPruneConfig,
    /// Todo auto-prune strategy configuration.
    #[serde(default)]
    pub todo: TodoAutoPruneConfig,
    /// Double-edit auto-prune strategy configuration.
    #[serde(default)]
    pub double_edit: DoubleEditAutoPruneConfig,
    /// Consecutive-reads auto-prune strategy configuration.
    #[serde(default)]
    pub consecutive_reads: ConsecutiveReadsAutoPruneConfig,
    /// Tool-age-window auto-prune strategy configuration.
    #[serde(default)]
    pub tool_age_window: ToolAgeWindowAutoPruneConfig,
    /// Trivial-assistant auto-prune strategy configuration.
    #[serde(default)]
    pub trivial_assistant: TrivialAssistantAutoPruneConfig,
    /// Anchored-assistant auto-prune strategy configuration.
    #[serde(default)]
    pub anchored_assistant: AnchoredAssistantAutoPruneConfig,
    /// Anchor-shield auto-prune strategy configuration.
    #[serde(default)]
    pub anchor_shield: AnchorShieldConfig,
    /// Token threshold at which accumulated pruner context-override mutations flush.
    ///
    /// Pruner `SetContextOverride` mutations are held in a per-session buffer until
    /// their deduplicated token total reaches this value, reducing server-side
    /// KV-cache rebuilds from frequent small prunes. Default: 10 000.
    #[serde(default = "default_accumulation_threshold_tokens")]
    pub accumulation_threshold_tokens: u32,
}

impl Default for AutoPruneConfig {
    fn default() -> Self {
        Self {
            edit_read: EditReadAutoPruneConfig::default(),
            read_edit: ReadEditAutoPruneConfig::default(),
            regex: RegexAutoPruneConfig::default(),
            broken_edit: BrokenEditAutoPruneConfig::default(),
            todo: TodoAutoPruneConfig::default(),
            double_edit: DoubleEditAutoPruneConfig::default(),
            consecutive_reads: ConsecutiveReadsAutoPruneConfig::default(),
            tool_age_window: ToolAgeWindowAutoPruneConfig::default(),
            trivial_assistant: TrivialAssistantAutoPruneConfig::default(),
            anchored_assistant: AnchoredAssistantAutoPruneConfig::default(),
            anchor_shield: AnchorShieldConfig::default(),
            accumulation_threshold_tokens: DEFAULT_ACCUMULATION_THRESHOLD_TOKENS,
        }
    }
}
