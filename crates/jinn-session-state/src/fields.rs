//! Cohesive durable field groups composed by [`SessionCore`](crate::SessionCore).
//!
//! Every group is flattened into `SessionCore`, preserving the established flat
//! persisted schema while keeping the live values under one authoritative lock.

use marked_path::{Absolute, MarkedPathBuf};
use std::collections::{BTreeSet, HashMap};

use jiff::Timestamp;
use jinn_bot_msg::{BotBehavior, BotModelSetting, BotReport, BotTrigger, default_seed_template};
use jinn_core_types::{ChatHistory, SessionId};
use jinn_session_lifecycle_msg::LifecycleScriptState;
use jinn_session_msg::SessionOrigin;
use jinn_session_store_msg::SessionState;
use jinn_token_count_msg::TokenRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub use jinn_core_types::SessionProfile;

/// Serde default for sessions that opt into persistence.
#[must_use]
pub fn default_persist() -> bool {
    true
}

/// Serde default for a working directory.
///
/// # Panics
///
/// Panics if the resolved directory is not absolute, which cannot happen on
/// supported platforms: both `std::env::current_dir()` and the temp-dir
/// fallback return absolute paths.
#[expect(
    clippy::expect_used,
    reason = "cwd and temp-dir fallback are absolute on supported platforms"
)]
#[must_use]
pub fn default_cwd() -> MarkedPathBuf<Absolute> {
    // `.` has always meant "the launch directory" — resolve it now so the
    // field is absolute for the session's whole life.
    let cwd = std::env::current_dir().unwrap_or_else(|_| tempfile::env::temp_dir());
    MarkedPathBuf::<Absolute>::new(cwd).expect("cwd and temp dir are absolute")
}

/// Deserializes a session cwd that may predate the absolute-by-construction
/// rule: a bare `.` (the pre-marker wire form of "the launch directory")
/// resolves against the process launch directory instead of failing the
/// load. Every other value is validated as [`Absolute`] normally.
///
/// # Errors
///
/// Returns the deserializer's error when the value is neither `.` nor an
/// absolute path.
pub fn deserialize_cwd_resolving_legacy_dot<'de, D>(
    deserializer: D,
) -> Result<MarkedPathBuf<Absolute>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    match raw.as_str() {
        Some(".") => Ok(default_cwd()),
        _ => MarkedPathBuf::<Absolute>::deserialize(raw).map_err(serde::de::Error::custom),
    }
}

/// A session's recorded lifecycle arguments: ordered name-value pairs.
///
/// Order is the setup template's first-appearance placeholder order, so the
/// builtin teardown bridge can drop the keys and hand the values to
/// `BuiltinHandler::teardown` in template order. Every consumer renders
/// through `CommandTemplate::render_stored`, which keys by placeholder name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LifecycleArgs(
    #[serde(deserialize_with = "deserialize_lifecycle_args")] pub Vec<(String, String)>,
);

impl LifecycleArgs {
    /// Builds recorded arguments from name-value pairs.
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    /// Whether every recorded key is a `$1`-style positional key.
    ///
    /// True only for sessions whose arguments were recorded as bare values
    /// before pairs existed; such sessions keep today's positional render.
    #[must_use]
    pub fn is_legacy_positional(&self) -> bool {
        self.0
            .iter()
            .all(|(key, _)| jinn_session_lifecycle_msg::command_template::is_positional_key(key))
    }

    /// The recorded values in recorded order, keys dropped.
    ///
    /// The builtin handlers' `&[String]` argument shape, and nothing else.
    #[must_use]
    pub fn values(&self) -> Vec<String> {
        self.0.iter().map(|(_, value)| value.clone()).collect()
    }

    /// The recorded pairs.
    #[must_use]
    pub fn as_pairs(&self) -> &[(String, String)] {
        &self.0
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Deserializes persisted lifecycle arguments in either persisted shape.
///
/// - an array of `[name, value]` arrays — the pairs form, taken as-is
/// - an array of bare strings — the legacy form, keyed `$1`, `$2`, ... by
///   index so the session keeps the positional render it was recorded with
///
/// Anything else is a real error: silently defaulting to empty would render
/// a template full of raw placeholders into a shell.
fn deserialize_lifecycle_args<'de, D>(deserializer: D) -> Result<Vec<(String, String)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Pairs(Vec<(String, String)>),
        Bare(Vec<String>),
    }
    match Raw::deserialize(deserializer)? {
        Raw::Pairs(pairs) => Ok(pairs),
        Raw::Bare(values) => Ok(values
            .into_iter()
            .enumerate()
            .map(|(idx, value)| (format!("${}", idx + 1), value))
            .collect()),
    }
}

/// Identity, tree metadata, and runtime interaction state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIdentityMetadataFields {
    /// Unique session identifier.
    pub session_id: SessionId,
    /// Last durable update timestamp.
    pub updated_at: Timestamp,
    /// Most recent local history mutation, used by the stall watchdog.
    #[serde(skip)]
    pub last_history_activity_at: Timestamp,
    /// Most recent provider output, used by the stall watchdog.
    #[serde(skip)]
    pub last_provider_activity_at: Timestamp,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// User-visible session title.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Parent session for forks and subagent lineage.
    #[serde(default)]
    pub parent_session: Option<SessionId>,
    /// Highest inherited entry ordinal after a fork.
    #[serde(default)]
    pub fork_ordinal: Option<usize>,
    /// Creation path.
    #[serde(default)]
    pub origin: SessionOrigin,
    /// Project association stamped at creation.
    #[serde(default)]
    pub project: Option<MarkedPathBuf<Absolute>>,
    /// Runtime persistence eligibility.
    #[serde(default)]
    pub has_interacted: bool,
}

impl Default for SessionIdentityMetadataFields {
    fn default() -> Self {
        Self {
            session_id: SessionId::new(),
            updated_at: Timestamp::now(),
            last_history_activity_at: Timestamp::now(),
            last_provider_activity_at: Timestamp::now(),
            created_at: Timestamp::now(),
            title: None,
            parent_session: None,
            fork_ordinal: None,
            origin: SessionOrigin::User,
            project: None,
            has_interacted: false,
        }
    }
}

/// Location, lifecycle name, arguments, and script progression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLifecycleLocationFields {
    /// Working directory used by tools and scans.
    #[serde(
        default = "default_cwd",
        deserialize_with = "deserialize_cwd_resolving_legacy_dot"
    )]
    pub cwd: MarkedPathBuf<Absolute>,
    /// Runtime home directory for attachment resolution.
    #[serde(skip, default = "default_cwd")]
    pub home: MarkedPathBuf<Absolute>,
    /// Lifecycle selected at creation.
    #[serde(default)]
    pub lifecycle_name: Option<String>,
    /// Arguments replayed during teardown, as ordered name-value pairs.
    #[serde(default)]
    pub lifecycle_args: LifecycleArgs,
    /// The workflow-local TOML file this session's lifecycle was resolved
    /// from, when it came from a workflow rather than from the user's
    /// jinn.toml.
    ///
    /// `None` for jinn.toml lifecycles. When set, teardown resolves the
    /// entry named by `lifecycle_name` from this file instead of from
    /// jinn.toml, so closing the session re-reads the workflow's own
    /// script. Omitted when absent, so a session that never carried a
    /// source persists a blob byte-identical to one written before the
    /// field existed. The marker crosses forks together with the name and
    /// args it qualifies: a fork that inherits the replayed lifecycle must
    /// resolve it from the same file, or its close would look the name up
    /// in the wrong place and skip silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_source: Option<String>,
    /// One-way lifecycle script progression.
    #[serde(default)]
    pub lifecycle_script_state: LifecycleScriptState,
}

impl Default for SessionLifecycleLocationFields {
    fn default() -> Self {
        Self {
            cwd: default_cwd(),
            home: default_cwd(),
            lifecycle_name: None,
            lifecycle_args: LifecycleArgs(Vec::new()),
            lifecycle_source: None,
            lifecycle_script_state: LifecycleScriptState::NothingRan,
        }
    }
}

/// History, token accounting, and task planning for one session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionHistoryWorkFields {
    /// Conversation history coordinated with active turn state.
    pub history: ChatHistory,
    /// Request/response token ledger.
    #[serde(default)]
    pub token_ledger: Vec<TokenRecord>,
    /// Durable task-plan state.
    #[serde(default)]
    pub task_list: jinn_tools_msg::TaskList,
}

/// Provider policy, durable integration configuration, and compatibility data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionIntegrationFields {
    /// Per-session provider and context strategy.
    pub profile: SessionProfile,
    /// Legacy serialized extension values retained for schema compatibility.
    #[serde(default)]
    pub blobs: HashMap<String, JsonValue>,
    /// Persisted MCP server enablement.
    #[serde(default)]
    pub enabled_mcp_servers: BTreeSet<String>,
}

/// Loaded/archived state, library membership, and persistence policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStorageFields {
    /// Current storage state.
    #[serde(default)]
    pub session_state: SessionState,
    /// Whether this session belongs to the sidebar's Library section.
    ///
    /// Membership is orthogonal to `session_state`: a member stays listed in
    /// the Library whether loaded or archived, and is hidden from the
    /// sidebar's Sessions list either way.
    #[serde(default)]
    pub in_library: bool,
    /// Whether this session participates in durable persistence.
    #[serde(default = "default_persist")]
    pub persist: bool,
}

impl Default for SessionStorageFields {
    fn default() -> Self {
        Self {
            session_state: SessionState::Loaded,
            in_library: false,
            persist: true,
        }
    }
}

/// Bot run parameters and the report log.
///
/// Every field carries `#[serde(default)]`, so a session written before
/// bots existed deserializes without a migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBotFields {
    /// What a run in this session sees of the conversation.
    #[serde(default)]
    pub behavior: BotBehavior,
    /// The condition that causes an automatic re-run.
    #[serde(default)]
    pub trigger: BotTrigger,
    /// Whether the bot is still being composed. While it is, nothing
    /// dispatches and the two settings above govern nothing.
    #[serde(default = "crate::fields::default_prep_mode")]
    pub prep_mode: bool,
    /// User-editable text used to inject the prior report on each run.
    #[serde(default = "default_seed_template")]
    pub seed_template: String,
    /// Whether the session's model is this bot's own or the copy it
    /// inherited at creation.
    ///
    /// The model itself lives on the profile and is always concrete; this
    /// only records whether the bot *owns* it, which is what decides
    /// whether a save writes a `model` key into the entry.
    #[serde(default)]
    pub model_setting: BotModelSetting,
    /// Append-only; the harness never removes or edits a report.
    #[serde(default)]
    pub reports: Vec<BotReport>,
    /// The workflow-local TOML file this bot was loaded from, when
    /// it belongs to a workflow rather than to the user's jinn.toml.
    ///
    /// `None` for personal bots. A bot with a source refuses
    /// to be saved into jinn.toml — its source file is the only writer of
    /// its configuration — and the marker never crosses to children: an
    /// bot created by a workflow bot is an ordinary personal
    /// one. Omitted when absent, so a session that never carried the
    /// marker persists a blob byte-identical to one written before it
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<jinn_core_types::BotSourcePath>,
}

impl Default for SessionBotFields {
    fn default() -> Self {
        Self {
            behavior: BotBehavior::default(),
            trigger: BotTrigger::default(),
            prep_mode: default_prep_mode(),
            seed_template: default_seed_template(),
            model_setting: BotModelSetting::default(),
            reports: Vec::new(),
            workflow_source: None,
        }
    }
}

/// The default for [`SessionBotFields::prep_mode`].
///
/// Composition is the state a bot is *born* in — `N` creates one the
/// user has not finished writing — so a session with no stored answer is
/// composing. A bool has no third value to say "not set" with, so the
/// default *is* the answer rather than a sentinel that means it; that is why
/// this is a function rather than a bare `true` on the attribute.
pub const fn default_prep_mode() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_bot_msg::BotModelSetting;

    use super::LifecycleArgs;
    use super::SessionBotFields;

    #[rstest::rstest]
    fn an_bot_blob_written_before_the_model_row_inherits() {
        // Given a blob holding every bot field as they were written
        // before the model setting existed.
        let blob = serde_json::json!({
            "behavior": "reset",
            "trigger": "parent_completed",
            "prep_mode": false,
            "seed_template": "review: <prior_report>",
            "reports": [],
        });

        // When it is deserialized as a bot's fields.
        let fields: SessionBotFields =
            serde_json::from_value(blob).expect("a pre-change blob deserializes");

        // Then the setting reads inherit, so a bot that predates the
        // row saves without a model key rather than pinning one.
        assert_eq!(fields.model_setting, BotModelSetting::Inherit);
    }

    #[rstest::rstest]
    fn lifecycle_args_deserialize_the_pairs_form() {
        // Given a persisted blob carrying ordered name-value pairs.
        let blob = serde_json::json!([["dir", "/wt"], ["parent", "trunk"]]);

        // When it is deserialized.
        let args: LifecycleArgs = serde_json::from_value(blob).expect("pairs deserialize");

        // Then the pairs come back in order.
        assert_eq!(
            args.as_pairs(),
            &[
                ("dir".to_owned(), "/wt".to_owned()),
                ("parent".to_owned(), "trunk".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    fn lifecycle_args_deserialize_a_legacy_bare_array_as_positional() {
        // Given a persisted blob in the pre-pairs bare-string shape.
        let blob = serde_json::json!(["feature-x", "--fast"]);

        // When it is deserialized.
        let args: LifecycleArgs = serde_json::from_value(blob).expect("legacy array deserializes");

        // Then each value is keyed $1, $2 by position, so the session keeps
        // the positional render it was recorded with.
        assert!(args.is_legacy_positional());
        assert_eq!(
            args.as_pairs(),
            &[
                ("$1".to_owned(), "feature-x".to_owned()),
                ("$2".to_owned(), "--fast".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    fn lifecycle_args_reject_an_unrecognized_payload() {
        // Given a persisted blob that is neither pairs nor bare strings.
        let blob = serde_json::json!([{"name": "dir"}]);

        // When it is deserialized.
        let result: Result<LifecycleArgs, _> = serde_json::from_value(blob);

        // Then it is a real error, not a silent empty — an empty would
        // render a template full of raw placeholders into a shell.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn lifecycle_args_serialize_back_as_pairs() {
        // Given a legacy array deserialized into $n-keyed pairs.
        let args: LifecycleArgs =
            serde_json::from_value(serde_json::json!(["only"])).expect("legacy deserializes");

        // When it is serialized.
        let out = serde_json::to_value(&args).expect("serialize");

        // Then it persists as the pairs form.
        assert_eq!(out, serde_json::json!([["$1", "only"]]));
    }

    #[rstest::rstest]
    fn lifecycle_args_values_drop_the_keys_in_order() {
        // Given named pairs in a non-alphabetical recorded order.
        let args = LifecycleArgs::from_pairs([("zeta", "1"), ("alpha", "2")]);

        // When the values are read.
        let values = args.values();

        // Then the recorded order is preserved, keys dropped.
        assert_eq!(values, vec!["1".to_owned(), "2".to_owned()]);
    }
}
