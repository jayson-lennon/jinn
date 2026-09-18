//! User preferences data type and file I/O.
//!
//! Defines [`UserPreferences`] as the schema for `jinn.toml`,
//! along with loading and saving logic. The file lives at
//! `~/.config/jinn/jinn.toml` and is auto-created on first run from
//! [`DEFAULT_CONFIG`] (a comment-rich template embedded at compile time).

use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt as _};
use jinn_common::app_info::{APP_NAME, PREFS_FILE_NAME};
use jinn_common::toml_patch::DocumentPatcher;
use serde::{Deserialize, Serialize};
use wherror::Error;

// ── Embedded config schemas ─────────────────────────────────────────────
// The sub-schemas below live in `crate::schemas` (co-located by feature
// domain). Re-exported here so `UserPreferences`' fields and every consumer
// keep resolving them from the aggregate's home.
pub use crate::schemas::{
    AutoPruneConfig, CompactionConfig, CwdSelectorConfig, MinimapConfig, ProjectConfig,
    RequestRetryConfig, SessionLifecycle,
};

/// Canonical default `jinn.toml` embedded at compile time.
///
/// Used both to auto-create the file on first run and to back the
/// `jinn config init` subcommand. The template is independent of the
/// struct's default values; `template_validation_tests` guarantees it
/// parses, documents every config key, and activates into a valid
/// config.
pub const DEFAULT_CONFIG: &str = include_str!("default_jinn.toml");
/// Default execution timeout (seconds) for all tool calls.
///
/// The model can override per-call via the reserved `max_duration_secs` argument
/// (supported by `bash`); a value of `0` disables the timeout for that call.
pub(crate) const DEFAULT_TOOL_DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Serde default function for [`UserPreferences::tool_default_timeout_secs`].
pub fn default_tool_default_timeout_secs() -> u64 {
    DEFAULT_TOOL_DEFAULT_TIMEOUT_SECS
}

/// Errors that can occur during user preferences I/O.
#[derive(Debug, Error)]
pub enum UserPreferencesError {
    /// Filesystem I/O failure.
    #[error("user preferences I/O error")]
    Io,
    /// TOML parsing or structural error.
    #[error("user preferences parse error")]
    Parse,
}

/// OpenRouter web search server tool configuration.
///
/// Serialized as `[openrouter_web_search]` in `jinn.toml`.
/// Controls parameters sent to the `openrouter:web_search` server tool.
/// All fields are optional - when `None`, the parameter is omitted from
/// the request and OpenRouter uses its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenrouterWebSearchConfig {
    /// Search engine: "auto", "native", "exa", "firecrawl", or "parallel".
    /// Default: "exa".
    #[serde(default)]
    pub engine: Option<String>,

    /// Maximum results per search call (1–25). `None` = OpenRouter default (5).
    #[serde(default)]
    pub max_results: Option<u32>,

    /// Maximum total results across all searches in one request.
    #[serde(default)]
    pub max_total_results: Option<u32>,

    /// How much context to retrieve: "low", "medium", or "high".
    /// `None` = OpenRouter picks adaptively.
    #[serde(default)]
    pub search_context_size: Option<String>,

    /// Only return results from these domains.
    #[serde(default)]
    pub allowed_domains: Option<Vec<String>>,

    /// Exclude results from these domains.
    #[serde(default)]
    pub excluded_domains: Option<Vec<String>>,
}

impl Default for OpenrouterWebSearchConfig {
    fn default() -> Self {
        Self {
            engine: Some("exa".to_owned()),
            max_results: None,
            max_total_results: None,
            search_context_size: None,
            allowed_domains: None,
            excluded_domains: None,
        }
    }
}

/// User preferences persisted in `jinn.toml`.
///
/// This file stores user behavior preferences that should survive
/// app restarts - e.g., the last model and strategy selected from pickers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    /// Maximum number of lines to display for tool entries in the chat log.
    /// `None` means use the built-in default (5 lines).
    #[serde(default)]
    pub tool_entry_max_lines: Option<u16>,
    /// Minimum number of contiguous excluded entries required to collapse into
    /// a summary line. `None` means use the built-in default (3).
    #[serde(default)]
    pub min_collapse_count: Option<usize>,

    /// Tool names disabled by default in newly created sessions.
    ///
    /// Entries are bare tool names (`bash`) or fully qualified MCP tool
    /// names (`mcp__<server>__<tool>`). Names that match nothing are inert,
    /// so listings stay forward-compatible with tools added later. Seeding
    /// happens at session creation only; picker toggles inside a session
    /// override it per-session and never write back here.
    ///
    /// `BTreeSet` rather than `HashSet`: preferences serialize through the
    /// comment-preserving patcher on every save, and hash iteration order
    /// would reshuffle the array bytes between runs.
    #[serde(default)]
    pub disabled_tools: std::collections::BTreeSet<String>,

    /// Skill names disabled by default in newly created sessions.
    ///
    /// Same semantics as [`UserPreferences::disabled_tools`] but applied to
    /// skills: listed skills are omitted from advertised skills and refused
    /// by the `skill` tool until re-enabled within the session.
    #[serde(default)]
    pub disabled_skills: std::collections::BTreeSet<String>,

    /// Named session lifecycle recipes - paired setup/teardown commands.
    /// The implicit "blank" lifecycle (no commands) is always available and
    /// does not need to be listed here.
    #[serde(default)]
    #[serde(rename = "session_lifecycle")]
    pub session_lifecycles: Vec<SessionLifecycle>,

    /// Curated project directories shown in the project picker, serialized
    /// as `[[projects]]` in `jinn.toml` and keyed by `path`. These are
    /// purely user-curated (no auto-tracking); the user adds/removes
    /// entries explicitly. See [`ProjectConfig`].
    ///
    /// The legacy `[[project]]` spelling from older configs is not an
    /// alias: it is ignored on load and stripped from the document before
    /// a patch is applied (see [`normalize_legacy_keys`]).
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,

    /// Configured MCP servers, keyed by name — `[mcp_server.<name>]` in
    /// `jinn.toml`. Each entry declares a server jinn connects to (over stdio,
    /// local_http, or remote_http — see
    /// [`TransportKind`](jinn_mcp_msg::TransportKind)) when enabled
    /// per-session. See [`McpServerConfig`].
    #[serde(default)]
    pub mcp_server: std::collections::BTreeMap<String, jinn_mcp_msg::McpServerConfig>,

    /// Configured plugins, keyed by name — `[plugin.<name>]` in `jinn.toml`.
    /// Each entry declares a `.wasm` component plus its capability grants;
    /// the plugin coordinator hosts one in-process WASM guest per enabled
    /// entry at app start. See
    /// [`PluginConfig`](crate::schemas::PluginConfig).
    #[serde(default)]
    pub plugin: std::collections::BTreeMap<String, crate::schemas::PluginConfig>,

    /// The local IP address HTTP-mode MCP servers bind to. Used as the `<ip>`
    /// replacement token in a server's `args`, and as the bind address for
    /// jinn's port allocation. Defaults to `127.0.0.1` (loopback only).
    /// Maximum number of lines for tool output before truncation.
    /// `None` means use the built-in default (2000 lines).
    #[serde(default)]
    pub max_tool_output_lines: Option<usize>,
    /// Maximum size in bytes for tool output before truncation.
    /// `None` means use the built-in default (50KB).
    #[serde(default)]
    pub max_tool_output_bytes: Option<usize>,
    /// Compaction configuration.
    #[serde(default)]
    pub compaction: CompactionConfig,
    /// Retry configuration for LLM provider requests.
    #[serde(default)]
    pub request_retry: RequestRetryConfig,
    /// OpenRouter web search server tool configuration.
    #[serde(default)]
    pub openrouter_web_search: OpenrouterWebSearchConfig,
    /// CWD selector configuration.
    #[serde(default)]
    pub cwd_selector: CwdSelectorConfig,
    /// Minimap configuration.
    #[serde(default)]
    pub minimap: MinimapConfig,
    /// Auto-prune configuration.
    #[serde(default)]
    pub auto_prune: AutoPruneConfig,
    /// Interactive terminal configuration (control-toggle key, settle wait).
    #[serde(default)]
    pub interactive_term: jinn_term_msg::prefs::InteractiveTermPrefs,
    /// Default execution timeout (seconds) for all tool calls.
    ///
    /// The model can override per-call via the reserved `max_duration_secs` argument
    /// (supported by `bash`); a value of `0` disables the timeout for that call.
    #[serde(default = "default_tool_default_timeout_secs")]
    pub tool_default_timeout_secs: u64,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            tool_entry_max_lines: None,
            min_collapse_count: None,
            disabled_tools: std::collections::BTreeSet::new(),
            disabled_skills: std::collections::BTreeSet::new(),
            session_lifecycles: vec![
                SessionLifecycle {
                    name: "fossil branch checkout".to_owned(),
                    description: Some("Open a new checkout + branch".to_owned()),
                    setup: Some(crate::schemas::LifecycleCommand::Shell(
                        "mkdir <branch> && cd <branch> && fossil open ../<repo>.fossil && fossil commit -m 'Open <branch>' --branch <branch> --allow-empty && echo ./<branch>".to_owned(),
                    )),
                    teardown: Some(crate::schemas::LifecycleCommand::Shell(
                        "fossil merge trunk --force && fossil addremove && fossil commit -m 'Bring in latest trunk' && fossil update trunk && fossil merge <branch> && fossil addremove && fossil commit -m 'Merge <branch>' && fossil branch close <branch> && cd .. && rm -rfv <branch>".to_owned(),
                    )),
                },
                SessionLifecycle {
                    name: "git worktree".to_owned(),
                    description: Some("Open a git worktree + branch".to_owned()),
                    setup: Some(crate::schemas::LifecycleCommand::Shell(
                        "cd <repo> && git worktree add -b <branch> ../<branch> && cd .. && echo $(pwd)/<branch>".to_owned(),
                    )),
                    teardown: Some(crate::schemas::LifecycleCommand::Shell(
                        "bash -c 'git add -A && (git diff --cached --quiet || git commit -q -m \"auto-commit at teardown\") && git merge main && cd ../<repo> && git merge --squash <branch> && (git diff --cached --quiet || git commit -q -m \"Merge <branch>\") && git worktree remove ../<branch> && git branch -D <branch>'".to_owned(),
                    )),
                },
            ],
            projects: vec![],
            mcp_server: std::collections::BTreeMap::new(),
            plugin: std::collections::BTreeMap::new(),
            max_tool_output_lines: None,
            max_tool_output_bytes: None,
            compaction: CompactionConfig::default(),
            request_retry: RequestRetryConfig::default(),
            openrouter_web_search: OpenrouterWebSearchConfig::default(),
            cwd_selector: CwdSelectorConfig::default(),
            minimap: MinimapConfig::default(),
            auto_prune: AutoPruneConfig::default(),
            interactive_term:
                jinn_term_msg::prefs::InteractiveTermPrefs::default(),
            tool_default_timeout_secs: default_tool_default_timeout_secs(),
        }
    }
}

/// Returns the path to the user preferences file.
///
/// Uses `dirs::config_dir()` → `~/.config/jinn/jinn.toml`.
#[must_use]
pub fn preferences_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join(PREFS_FILE_NAME)
}

/// Loads user preferences from the default path.
///
/// Returns default preferences if the file does not exist.
///
/// # Errors
///
/// Returns [`UserPreferencesError::Parse`] if the TOML is malformed.
/// Returns [`UserPreferencesError::Io`] if the file cannot be read.
pub fn load_preferences() -> Result<UserPreferences, Report<UserPreferencesError>> {
    load_preferences_from(preferences_path())
}

/// Loads preferences from a specific path.
///
/// If the path does not exist, the canonical default template
/// (`DEFAULT_CONFIG`) is written there first so the user gets a
/// comment-rich starter file, then parsed.
///
/// If the file exists but fails to parse, it may carry legacy keys that
/// collide with canonical ones (e.g. a hand-migrated `[[project]]` block
/// next to a patched-in `[[projects]]`). In that case the document is
/// healed — legacy keys removed via [`normalize_legacy_keys`] — and the
/// parse retried; when the retry succeeds the healed text is written back
/// to disk so later loads and saves see a clean file. A file that still
/// fails after healing is left untouched on disk.
/// # Errors
///
/// Returns an error when the file cannot be read or parsed.
pub fn load_preferences_from<P>(path: P) -> Result<UserPreferences, Report<UserPreferencesError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        create_default_preferences_to(path)?;
    }

    let content = std::fs::read_to_string(path)
        .change_context(UserPreferencesError::Io)
        .attach("failed to read user preferences")?;

    match toml::from_str(&content) {
        Ok(prefs) => Ok(prefs),
        Err(_) => load_healed(path, &content),
    }
}

/// Second-chance loader for files whose parse failed: strips legacy keys
/// with [`normalize_legacy_keys`] and retries. On success the healed text
/// is persisted to `path` so subsequent loads and patches see a clean
/// document. When the healed text still does not parse (e.g. TOML that is
/// syntactically broken, not merely poisoned by legacy keys) the file is
/// left untouched and the error is returned.
///
/// # Errors
///
/// Returns [`UserPreferencesError::Parse`] if the content cannot be parsed
/// even after normalization. Returns [`UserPreferencesError::Io`] if
/// writing the healed document fails.
fn load_healed(
    path: &Path,
    content: &str,
) -> Result<UserPreferences, Report<UserPreferencesError>> {
    let healed = {
        let mut doc: toml_edit::DocumentMut = content
            .parse()
            .change_context(UserPreferencesError::Parse)
            .attach("failed to parse user preferences")?;
        let salvaged = normalize_legacy_keys(doc.as_table_mut());
        if let Some(comment) = salvaged {
            reattach_salvaged_comment(&mut doc, comment);
        }
        doc.to_string()
    };

    let prefs = toml::from_str(&healed)
        .change_context(UserPreferencesError::Parse)
        .attach("failed to parse user preferences after removing legacy keys")?;

    std::fs::write(path, healed)
        .change_context(UserPreferencesError::Io)
        .attach("failed to write healed user preferences")?;

    Ok(prefs)
}

/// Writes the canonical default preferences template to `path`.
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns [`UserPreferencesError::Io`] if directory creation or file writing fails.
pub(crate) fn create_default_preferences_to<P>(path: P) -> Result<(), Report<UserPreferencesError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(UserPreferencesError::Io)
            .attach("failed to create preferences directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(UserPreferencesError::Io)
        .attach("failed to write default user preferences")
}

/// Error returned by [`init_default_config_to`].
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct InitDefaultConfigError;

/// Outcome of [`init_default_config_to`].
#[derive(Debug)]
pub enum InitOutcome {
    /// Template was written to a previously-missing path.
    Created,
    /// Existing file was overwritten (caller passed `force: true`).
    Overwritten,
}

/// Writes [`DEFAULT_CONFIG`] to `path`.
///
/// - If `path` does not exist: writes the template, returns [`InitOutcome::Created`].
/// - If `path` exists and `force` is false: returns `Err(InitDefaultConfigError)`.
/// - If `path` exists and `force` is true: overwrites, returns [`InitOutcome::Overwritten`].
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns [`Report<InitDefaultConfigError>`] if the file already exists and
/// `force` is false, or if directory creation / file writing fails.
pub fn init_default_config_to<P>(
    path: P,
    force: bool,
) -> Result<InitOutcome, Report<InitDefaultConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let existed = path.exists();

    if existed && !force {
        return Err(Report::new(InitDefaultConfigError))
            .attach("jinn.toml already exists; pass --force to overwrite")
            .attach(format!("path: {}", path.display()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(InitDefaultConfigError)
            .attach("failed to create preferences directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(InitDefaultConfigError)
        .attach("failed to write default user preferences")?;

    if existed {
        Ok(InitOutcome::Overwritten)
    } else {
        Ok(InitOutcome::Created)
    }
}

/// Saves preferences to the default path.
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns [`UserPreferencesError::Parse`] if serialization fails.
/// Returns [`UserPreferencesError::Io`] if writing fails.
pub fn save_preferences(prefs: &UserPreferences) -> Result<(), Report<UserPreferencesError>> {
    save_preferences_to(prefs, preferences_path())
}

/// Saves preferences to a specific path.
/// # Errors
///
/// Returns an error when the document cannot be patched or written.
pub fn save_preferences_to<P>(
    prefs: &UserPreferences,
    path: P,
) -> Result<(), Report<UserPreferencesError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(UserPreferencesError::Io)
            .attach("failed to create preferences directory")?;
    }

    // If the file exists, patch it in place to preserve user comments / ordering.
    // Otherwise, emit a clean zero-comment serialization (no template to keep in sync).
    let content = if path.exists() {
        let existing = std::fs::read_to_string(path)
            .change_context(UserPreferencesError::Io)
            .attach("failed to read existing jinn.toml")?;

        let mut doc: toml_edit::DocumentMut = existing
            .parse()
            .change_context(UserPreferencesError::Parse)
            .attach("failed to parse existing jinn.toml")?;

        // Stale keys left in user files collide with their canonical keys
        // once a patch inserts the canonical name (serde would see both and
        // fail with "duplicate field"). Strip/rename them before patching;
        // salvage any comment the dead legacy block carried for re-attach.
        let salvaged_comment = normalize_legacy_keys(doc.as_table_mut());

        let new_value = toml::Value::try_from(prefs)
            .change_context(UserPreferencesError::Parse)
            .attach("failed to serialize UserPreferences")?;

        let toml::Value::Table(new_table) = &new_value else {
            return Err(Report::new(UserPreferencesError::Parse)
                .attach("UserPreferences serialized to non-table TOML value"));
        };
        let mut patcher = DocumentPatcher::new();
        patcher.register_array_key(["session_lifecycle"], "name");
        patcher.register_array_key(["auto_prune", "regex", "rules"], "pattern");
        patcher.register_array_key(["projects"], "path");
        // `plugin` and `mcp_server` are map-keyed tables (`[plugin.<name>]`),
        // not arrays — the table name is the identity, no key registration
        // needed.

        patcher
            .apply(new_table, doc.as_table_mut())
            .change_context(UserPreferencesError::Parse)
            .attach("failed to patch jinn.toml document")?;

        if let Some(comment) = salvaged_comment {
            reattach_salvaged_comment(&mut doc, comment);
        }

        doc.to_string()
    } else {
        toml::to_string_pretty(prefs)
            .change_context(UserPreferencesError::Parse)
            .attach("failed to serialize user preferences")?
    };

    std::fs::write(path, content)
        .change_context(UserPreferencesError::Io)
        .attach("failed to write user preferences")
}

/// Strips legacy keys from an existing jinn.toml document so a patch
/// cannot collide with them ("duplicate field" on the next load).
///
/// Returns the leading comment (key-decor prefix) salvaged from the
/// legacy `[[project]]` block, if it carried one — re-attach it to the
/// document after patching so the user's comment is not lost with the
/// dead block.
///
/// Legacy keys handled:
///
/// - `[[project]]` — the old spelling of the projects list. It is not an
///   alias (see [`UserPreferences::projects`]); its entries are ignored on
///   load, so the block is removed outright.
/// - `broken_edit.min_tail_entries` — a renamed scalar that still
///   deserializes via alias. It is renamed in place so the user's value
///   and any attached comments survive.
///
/// A file carrying only these legacy keys parses as-is (unknown keys are
/// inert); normalization matters when the patcher inserts the canonical
/// key alongside them. Idempotent: a normalized document is unchanged.
pub fn normalize_legacy_keys(root: &mut toml_edit::Table) -> Option<String> {
    let salvaged = root
        .contains_key("project")
        .then(|| item_leading_comment(root.get("project")))
        .flatten();
    root.remove("project");
    if let Some(table) = root
        .get_mut("auto_prune")
        .and_then(|item| item.as_table_mut())
        .and_then(|t| t.get_mut("broken_edit"))
        .and_then(|item| item.as_table_mut())
        && let Some(item) = table.remove("min_tail_entries")
    {
        table.insert("min_age", item);
    }
    salvaged
}

/// Reads the leading comment carried by an item without mutating it: the
/// first AoT element's decor for array-of-tables (`# c\n[[k]]`), the
/// table's own decor for plain tables (`# c\n[k]`), the key's decor
/// prefix otherwise (inline values). Returns `None` when nothing is
/// carried.
fn item_leading_comment(item: Option<&toml_edit::Item>) -> Option<String> {
    let item = item?;
    let prefix = match item {
        toml_edit::Item::ArrayOfTables(tables) => tables.iter().next().and_then(|el| {
            el.decor()
                .prefix()
                .and_then(|raw| raw.as_str().map(str::to_owned))
        }),
        toml_edit::Item::Table(t) => t
            .decor()
            .prefix()
            .and_then(|raw| raw.as_str().map(str::to_owned)),
        _ => None,
    };
    prefix.filter(|p| !p.is_empty())
}

/// Re-attaches a salvaged comment to the document by prepending it to the
/// document's trailing raw string — i.e. the comment is appended to the
/// end of the file, after the last entry. Safe across the patcher's
/// coercions (which may replace the `projects` key entirely); a stray
/// trailing comment is cosmetic, while a comment spliced into header
/// decor the patcher does not expect can be rendered corrupt.
fn reattach_salvaged_comment(doc: &mut toml_edit::DocumentMut, comment: String) {
    let combined = match doc.trailing().as_str() {
        Some(existing) => format!("{comment}{existing}"),
        None => comment,
    };
    doc.set_trailing(combined);
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::print_stderr,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use tempfile::TempDir;

    use super::*;
    use crate::schemas::auto_prune::{
        AnchorShieldConfig, AnchoredAssistantAutoPruneConfig, BrokenEditAutoPruneConfig,
        ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig, EditReadAutoPruneConfig,
        ReadEditAutoPruneConfig, RegexAutoPruneConfig, RegexPruneRule, TodoAutoPruneConfig,
        ToolAgeWindowAutoPruneConfig, TrivialAssistantAutoPruneConfig,
    };

    /// A fully-specified [`UserPreferences`] with every field set explicitly
    /// (no `..Default::default()`). Serves two purposes: the input for the
    /// consolidated round-trip tests, and the schema fixture the template
    /// completeness check enumerates (`jinn-common`'s `template_check`
    /// walks its serialized key paths, `None` included).
    ///
    /// Sentinel values deliberately differ from the current built-in
    /// defaults wherever the type admits a different value, so a serde bug
    /// that silently drops a field and falls back to the default still
    /// fails the equality assertion.
    ///
    /// Collection fields carry at least one entry each so that entry-shape
    /// keys (`mcp_server.*.headers`, `session_lifecycle.*.setup_command`,
    /// ...) are part of the documented schema.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "a fully-specified fixture is intentionally exhaustive; no ..default() escapes"
    )]
    pub(crate) fn explicit_user_preferences() -> UserPreferences {
        let plugin_config = {
            let mut cfg = toml::map::Map::new();
            cfg.insert(
                "scope".to_owned(),
                toml::Value::String("read-only".to_owned()),
            );
            cfg
        };
        let auto_prune = AutoPruneConfig {
            edit_read: EditReadAutoPruneConfig {
                enabled: false,
                min_age: 7,
            },
            read_edit: ReadEditAutoPruneConfig {
                enabled: false,
                min_age: 9,
                threshold: 4,
            },
            regex: RegexAutoPruneConfig {
                enabled: false,
                rules: vec![RegexPruneRule {
                    pattern: "fixture-pattern".to_owned(),
                    tool_name: "fixture-tool".to_owned(),
                    keep_last: 3,
                    min_age: 11,
                }],
            },
            broken_edit: BrokenEditAutoPruneConfig {
                enabled: false,
                min_age: 2,
            },
            todo: TodoAutoPruneConfig {
                enabled: false,
                min_age: 3,
            },
            double_edit: DoubleEditAutoPruneConfig {
                enabled: false,
                max_file_edits: 1,
                min_age: 4,
            },
            consecutive_reads: ConsecutiveReadsAutoPruneConfig {
                enabled: false,
                keep_last: 2,
                min_age: 5,
            },
            tool_age_window: ToolAgeWindowAutoPruneConfig {
                enabled: false,
                min_age: 6,
            },
            trivial_assistant: TrivialAssistantAutoPruneConfig {
                enabled: false,
                min_age: 8,
                max_tokens: 10,
            },
            anchored_assistant: AnchoredAssistantAutoPruneConfig {
                enabled: false,
                radius: 12,
                min_age: 13,
            },
            anchor_shield: AnchorShieldConfig {
                enabled: false,
                radius: 14,
            },
            accumulation_threshold_tokens: 17,
        };
        UserPreferences {
            tool_entry_max_lines: Some(9),
            min_collapse_count: Some(7),
            disabled_tools: ["fixture-tool-a".to_owned(), "fixture-tool-b".to_owned()]
                .into_iter()
                .collect(),
            disabled_skills: ["fixture-skill".to_owned()].into_iter().collect(),
            session_lifecycles: vec![SessionLifecycle {
                name: "fixture lifecycle".to_owned(),
                description: Some("fixture description".to_owned()),
                setup: Some(crate::schemas::LifecycleCommand::Shell(
                    "echo fixture-setup".to_owned(),
                )),
                teardown: Some(crate::schemas::LifecycleCommand::Shell(
                    "echo fixture-teardown".to_owned(),
                )),
            }],
            projects: vec![ProjectConfig {
                path: "/tmp/fixture-project".into(),
                command_policy: Vec::new(),
            }],
            mcp_server: [(
                "fixture-server".to_owned(),
                jinn_mcp_msg::McpServerConfig {
                    command: Some("fixture-command".to_owned()),
                    args: vec!["--fixture-flag".to_owned()],
                    transport: jinn_mcp_msg::TransportKind::RemoteHttp,
                    url: Some("http://fixture.invalid/mcp".to_owned()),
                    auto_enable: true,
                    headers: [("X-Fixture-Header".to_owned(), "fixture-value".to_owned())]
                        .into_iter()
                        .collect(),
                },
            )]
            .into_iter()
            .collect(),
            plugin: [(
                "fixture-plugin".to_owned(),
                crate::schemas::PluginConfig {
                    wasm: "fixture-plugin.wasm".to_owned(),
                    grants: vec![crate::schemas::PluginPathGrant {
                        path: "<data_dir>/fixture:w".to_owned(),
                        writable: true,
                    }],
                    http: true,
                    config: Some(toml::Value::Table(plugin_config)),
                    enabled: false,
                },
            )]
            .into_iter()
            .collect(),
            max_tool_output_lines: Some(901),
            max_tool_output_bytes: Some(902),
            compaction: CompactionConfig {
                model: Some("fixture/compaction-model".to_owned()),
                threshold: 0.31,
                reserve_tokens: 3100,
                fallback_context_window: 31000,
            },
            request_retry: RequestRetryConfig {
                max_retries: 9,
                base_delay_secs: 3,
                max_delay_secs: 90,
            },
            openrouter_web_search: OpenrouterWebSearchConfig {
                engine: Some("firecrawl".to_owned()),
                max_results: Some(23),
                max_total_results: Some(27),
                search_context_size: Some("low".to_owned()),
                allowed_domains: Some(vec!["fixture.example".to_owned()]),
                excluded_domains: Some(vec!["blocked.example".to_owned()]),
            },
            cwd_selector: CwdSelectorConfig {
                command: "fixture-cwd-selector {path}".to_owned(),
            },
            minimap: MinimapConfig { max_tokens: 2900 },
            auto_prune,
            interactive_term: jinn_term_msg::prefs::InteractiveTermPrefs {
                control_toggle_key: "<c-t>".to_owned(),
                settle_quiet_ms: 410,
                settle_max_wait_ms: 3100,
            },
            tool_default_timeout_secs: 301,
        }
    }

    #[rstest::rstest]
    fn default_preferences_has_defaults_for_optional_fields() {
        // Given default preferences.
        let prefs = UserPreferences::default();

        // Then optional fields default to None.
        assert!(prefs.tool_entry_max_lines.is_none());
        assert!(prefs.min_collapse_count.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn load_tolerates_removed_web_sections_in_existing_files() {
        // Given a preferences file written by an older jinn that still had
        // the removed web_fetch/web_search/browser sections.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[web_fetch]
backend = "http"

[web_search]
backend = "http"
max_results = 10

[browser]
binary = "auto"
"#,
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path);

        // Then the removed sections are ignored and defaults load.
        let prefs = prefs.expect("load tolerates removed sections");
        assert_eq!(prefs.openrouter_web_search.engine.as_deref(), Some("exa"));
    }

    #[rstest::rstest]
    #[test]
    fn load_returns_defaults_and_creates_file_when_missing() {
        // Given a path to a nonexistent file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then defaults are returned.

        assert!(prefs.tool_entry_max_lines.is_none());
        // And the file is created.
        assert!(path.exists());
    }

    #[rstest::rstest]
    fn load_creates_file_with_template_bytes_when_missing() {
        // Given a path to a nonexistent file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);

        // When loading.
        load_preferences_from(&path).expect("load");

        // Then the file's bytes are exactly the embedded template.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, DEFAULT_CONFIG);
    }

    #[rstest::rstest]
    fn stale_removed_sections_load_as_unknown_keys() {
        // Given a jinn.toml containing only sections for removed features,
        // and a sibling jinn.toml that is empty.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[todo_auto_steer]\nenabled = true\nthreshold = 50\n\n[task_list]\necho_enabled = true\n",
        )
        .expect("write");
        let empty_path = dir.path().join("empty.toml");
        std::fs::write(&empty_path, "").expect("write");

        // When loading both files.
        let stale = load_preferences_from(&path).expect("load stale config");
        let empty = load_preferences_from(&empty_path).expect("load empty config");

        // Then both produce identical preferences (the removed sections are
        // inert unknown keys).
        assert_eq!(stale, empty);
    }

    #[rstest::rstest]
    fn save_preserves_slice_owned_discord_table() {
        // Given a jinn.toml with a user-configured [discord] table (a
        // slice-owned section the kernel struct no longer models) plus
        // a modeled field.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "# my prefs\ntool_entry_max_lines = 10\n\n# discord setup\n[discord]\nenabled = true\nforum_channel = \"222\"\n",
        )
        .expect("write");

        // When loading and saving back.
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("save");

        // Then the [discord] table and its comment survive untouched.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert!(
            on_disk.contains("[discord]"),
            "table must survive: {on_disk}"
        );
        assert!(
            on_disk.contains("forum_channel = \"222\""),
            "fields must survive"
        );
        assert!(on_disk.contains("# discord setup"), "comment must survive");
        // And the modeled field is still patched.
        assert!(on_disk.contains("tool_entry_max_lines = 10"));
    }

    #[rstest::rstest]
    fn load_does_not_touch_existing_file() {
        // Given an existing file with custom content.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        let marker = "# user-managed\ntool_entry_max_lines = 10\n";
        std::fs::write(&path, marker).expect("write");
        let mtime_before = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("metadata");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the file on disk is unchanged.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, marker);
        // And the parsed prefs reflect the file, not the defaults.
        assert_eq!(prefs.tool_entry_max_lines, Some(10));
        // And the mtime is preserved.
        let mtime_after = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("metadata");
        assert_eq!(mtime_before, mtime_after);
    }

    #[rstest::rstest]
    fn init_writes_template_when_missing() {
        // Given a path to a nonexistent file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);

        // When initializing the default config (no force).
        let outcome = init_default_config_to(&path, false).expect("init");

        // Then the file is created with the template bytes.
        assert!(matches!(outcome, InitOutcome::Created));
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, DEFAULT_CONFIG);
    }

    #[rstest::rstest]
    fn init_returns_already_exists_when_present_and_no_force() {
        // Given an existing file with custom content.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        let marker = "# user-managed\ntool_entry_max_lines = 10\n";
        std::fs::write(&path, marker).expect("write");

        // When initializing without --force.
        let result = init_default_config_to(&path, false);

        // Then the call fails with InitDefaultConfigError.
        assert!(result.is_err());
        // And the file is unchanged.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, marker);
    }

    #[rstest::rstest]
    fn init_overwrites_when_force() {
        // Given an existing file with custom content.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "# stale\n").expect("write");

        // When initializing with --force.
        let outcome = init_default_config_to(&path, true).expect("init");

        // Then the file is overwritten with the template bytes.
        assert!(matches!(outcome, InitOutcome::Overwritten));
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, DEFAULT_CONFIG);
    }

    #[rstest::rstest]
    fn load_parses_toml_content() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "tool_entry_max_lines = 10").expect("write");
        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        assert_eq!(prefs.tool_entry_max_lines, Some(10));
    }

    #[rstest::rstest]
    fn load_handles_empty_file() {
        // Given an empty TOML file (all keys at their defaults).
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "").expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the lifecycle/recipe entries ship with the default template
        // but an empty file has none: compare against a default with those
        // lists cleared rather than pinning any default literal.
        let expected = {
            let mut prefs = UserPreferences::default();
            prefs.session_lifecycles.clear();
            prefs
        };
        assert_eq!(prefs, expected);
    }

    #[rstest::rstest]
    fn explicit_preferences_save_then_load_round_trips() {
        // Given a fully-specified preferences fixture.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        let prefs = explicit_user_preferences();

        // When saving and reloading.
        save_preferences_to(&prefs, &path).expect("save");
        let reloaded = load_preferences_from(&path).expect("load");

        // Then every field survives the round-trip exactly.
        assert_eq!(reloaded, prefs);
    }

    #[rstest::rstest]
    fn save_creates_parent_directories() {
        // Given a nested path that doesn't exist.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nested").join("dir").join(PREFS_FILE_NAME);
        let prefs = UserPreferences {
            session_lifecycles: vec![],
            ..UserPreferences::default()
        };

        // When saving.
        save_preferences_to(&prefs, &path).expect("save");

        // Then the file exists.
        assert!(path.exists());
    }

    #[rstest::rstest]
    fn default_preferences_lifecycles_are_nonempty_with_setup_commands() {
        // Given default preferences.
        let prefs = UserPreferences::default();

        // Then the Default impl ships at least one lifecycle, and every
        // shipped lifecycle has a setup command (usable out of the box).
        assert!(!prefs.session_lifecycles.is_empty());
        // And each lifecycle is named and has setup wired.
        for lifecycle in &prefs.session_lifecycles {
            assert!(!lifecycle.name.is_empty());
            assert!(lifecycle.setup.is_some());
        }
    }

    #[rstest::rstest]
    fn preferences_path_ends_with_jinn_toml() {
        // Given the standard preferences path.
        let path = preferences_path();

        // Then it ends with jinn/jinn.toml.
        assert!(path.to_string_lossy().ends_with("jinn/jinn.toml"));
    }

    #[rstest::rstest]
    fn default_preferences_has_empty_disablement_sets() {
        // Given default preferences.
        let prefs = UserPreferences::default();

        // Then no tools are disabled by default.
        assert!(prefs.disabled_tools.is_empty());
        // And no skills are disabled by default.
        assert!(prefs.disabled_skills.is_empty());
    }

    #[rstest::rstest]
    fn minimal_legacy_toml_yields_empty_disablement_sets() {
        // Given a pre-feature jinn.toml without the new keys.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "tool_entry_max_lines = 10\n").expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then both disablement sets deserialize to empty via serde default.
        assert!(prefs.disabled_tools.is_empty());
        // And skills likewise.
        assert!(prefs.disabled_skills.is_empty());
    }

    #[rstest::rstest]
    fn save_preferences_preserves_user_comments_on_scalar_change() {
        // Given a comment-rich jinn.toml.
        let original = "# my prefs\ntool_entry_max_lines = 10\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading, mutating tool_entry_max_lines, and saving.
        let mut prefs = load_preferences_from(&path).expect("load");
        prefs.tool_entry_max_lines = Some(20);
        save_preferences_to(&prefs, &path).expect("save");

        // Then the comment is preserved and the field is updated.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("# my prefs"), "comment wiped: {written}");
        assert!(written.contains("tool_entry_max_lines = 20"));
        assert!(!written.contains("tool_entry_max_lines = 10"));
    }

    #[rstest::rstest]
    fn first_save_of_jinn_toml_emits_no_comments() {
        // Given: no jinn.toml exists yet.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        assert!(!path.exists());

        // When: saving for the first time.
        let prefs = UserPreferences::default();
        save_preferences_to(&prefs, &path).expect("save");

        // Then: the written file contains no comment characters at all.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            !written.contains('#'),
            "first save of jinn.toml must be comment-free, got: {written}"
        );
    }

    #[rstest::rstest]
    fn save_preferences_preserves_user_comments_in_auto_prune_section() {
        // Given a jinn.toml with comments in the auto_prune.regex section.
        let original = "# auto-prune rules\n[auto_prune.regex]\nenabled = true\n\n# matches foo\n[[auto_prune.regex.rules]]\npattern = \"foo\"\nkeep_last = 3\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading, mutating keep_last, and saving.
        let mut prefs = load_preferences_from(&path).expect("load");
        if let Some(rule) = prefs.auto_prune.regex.rules.first_mut() {
            rule.keep_last = 99;
        }
        save_preferences_to(&prefs, &path).expect("save");

        // Then the comments are preserved and the value is updated.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("# auto-prune rules"));
        assert!(written.contains("# matches foo"));
        assert!(written.contains("keep_last = 99"));
    }
    #[rstest::rstest]
    fn save_preferences_comprehensive_comment_round_trip_preserves_all_styles() {
        // Given a jinn.toml fixture using every comment style we promise to
        // preserve: top-of-file banner, section header, mid-table inline,
        // array-of-tables block headers.
        let original = r#"# my jinn preferences - hand-edited
                                                            
        # main prefs
        last_model = "openrouter/anthropic/claude-sonnet-4-20250514"
                                                            
        # compaction
        [compaction]
        enabled = true        # always compact
        threshold = 100       # tokens
                                                            
        # session lifecycles
        [[session_lifecycle]]
        name = "fossil-branch"
        description = "Open a fossil branch in a new workdir"
                                                            
        [[session_lifecycle]]
        name = "cleanup"
        description = "Tidy up after session"
                                                            
        # auto-prune
        [auto_prune.regex]
        enabled = true
                                                            
        # matches todo-related files
        [[auto_prune.regex.rules]]
        pattern = "TODO\\.md"
        keep_last = 2
                                                            
        # matches build artifacts
        [[auto_prune.regex.rules]]
        pattern = "target/"
        keep_last = 1
        "#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading and immediately re-saving without changes.
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("save");

        // Then every comment style is preserved byte-for-byte.
        let written = std::fs::read_to_string(&path).expect("read");
        for expected in [
            "# my jinn preferences - hand-edited",
            "# main prefs",
            "# compaction",
            "# always compact", // inline trailing
            "# tokens",         // inline trailing
            "# session lifecycles",
            "# auto-prune",
            "# matches todo-related files",
            "# matches build artifacts",
        ] {
            assert!(
                written.contains(expected),
                "comment lost: {expected:?}\nGot:\n{written}"
            );
        }
    }

    #[rstest::rstest]
    fn save_preferences_mixed_mutations_preserve_unrelated_comments() {
        // Given a jinn.toml with comments sprinkled across several sections.
        let original = "\
# main preferences\ntool_entry_max_lines = 10\n\n# collapse threshold\nmin_collapse_count = 5\n\n# keep context compact\n[compaction]\n# always compact\nenabled = true\n# 50k tokens\ntokens = 50000\n\n# my lifecycles\n[[session_lifecycle]]\nname = \"alpha\"\n\n# deprecated lifecycle\n[[session_lifecycle]]\nname = \"beta\"\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When applying a mixed mutation set:
        //   - change tool_entry_max_lines (scalar update)
        //   - delete beta session_lifecycle (array entry removal)
        //   - leave min_collapse_count and compaction untouched
        let mut prefs = load_preferences_from(&path).expect("load");
        prefs.tool_entry_max_lines = Some(20);
        prefs.session_lifecycles.retain(|l| l.name == "alpha");
        save_preferences_to(&prefs, &path).expect("save");

        // Then all unrelated comments survive and the targeted changes applied.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("# main preferences"), "top comment kept");
        assert!(
            written.contains("# collapse threshold"),
            "collapse comment kept"
        );
        assert!(
            written.contains("min_collapse_count = 5"),
            "untouched field kept"
        );
        assert!(
            written.contains("# keep context compact"),
            "compaction comment kept"
        );
        assert!(written.contains("# always compact"), "nested comment kept");
        assert!(
            written.contains("# 50k tokens"),
            "second nested comment kept"
        );
        assert!(
            written.contains("# my lifecycles"),
            "lifecycles comment kept"
        );
        assert!(
            !written.contains("# deprecated lifecycle"),
            "beta comment removed with beta"
        );
        assert!(!written.contains("\"beta\""), "beta removed");
        assert!(
            written.contains("tool_entry_max_lines = 20"),
            "tool_entry_max_lines updated"
        );
        assert!(
            !written.contains("tool_entry_max_lines = 10"),
            "old tool_entry_max_lines gone"
        );
        // The alpha lifecycle is preserved.
        assert!(written.contains("\"alpha\""), "alpha kept");
    }
    #[rstest::rstest]
    fn save_preferences_preserves_inner_block_comment_when_field_is_mutated() {
        // Given a jinn.toml with a comment between two session_lifecycle fields.
        // (The comment attaches to the next field's key decor, not its value.)
        let original = "[[session_lifecycle]]\nname = \"cwd test\"\n# am i preserved?\ndescription = \"Open a fossil branch in a new worktree\"\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading and mutating the field after the comment.
        let mut prefs = load_preferences_from(&path).expect("load");
        prefs.session_lifecycles[0].description = Some("UPDATED DESCRIPTION".to_owned());
        save_preferences_to(&prefs, &path).expect("save");

        // Then the inner comment survives AND the field is updated.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# am i preserved?"),
            "inner comment lost on mutation:\n{written}"
        );
        assert!(
            written.contains("UPDATED DESCRIPTION"),
            "description updated:\n{written}"
        );
    }

    #[rstest::rstest]
    fn load_preferences_actually_reads_file_content() {
        // If load_preferences were a no-op returning defaults, this would fail
        // because we verify that file content is actually read and parsed.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "tool_entry_max_lines = 42\n             min_collapse_count = 7\n",
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");

        // Then the loaded prefs are NOT defaults - they reflect the file.
        assert_eq!(prefs.tool_entry_max_lines, Some(42));
        assert_eq!(prefs.min_collapse_count, Some(7));
    }

    #[rstest::rstest]
    fn save_preferences_actually_writes_to_disk() {
        // If save_preferences were a no-op, the file would not exist on disk.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        let prefs = UserPreferences {
            tool_entry_max_lines: Some(99),
            session_lifecycles: vec![],
            ..UserPreferences::default()
        };

        save_preferences_to(&prefs, &path).expect("save");

        // Then the file exists on disk with the expected content.
        assert!(path.exists(), "save_preferences should create the file");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert!(content.contains("tool_entry_max_lines = 99"));
        assert!(content.contains("99"));
    }

    #[rstest::rstest]
    fn save_preferences_preserves_user_comments() {
        // Given a comment-rich jinn.toml.
        let original = "# my favorite\ntool_entry_max_lines = 10\nmin_collapse_count = 42\n";
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading, mutating tool_entry_max_lines, and saving.
        let mut prefs = load_preferences_from(&path).expect("load");
        prefs.tool_entry_max_lines = Some(7);
        save_preferences_to(&prefs, &path).expect("save");

        // Then the comment is preserved verbatim.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# my favorite"),
            "comment was wiped: {written}"
        );
        assert!(written.contains("tool_entry_max_lines = 7"));
        assert!(written.contains("min_collapse_count = 42"));
    }

    #[rstest::rstest]
    fn default_preferences_wire_openrouter_web_search_to_section_default() {
        let prefs = UserPreferences::default();
        assert_eq!(
            prefs.openrouter_web_search,
            OpenrouterWebSearchConfig::default()
        );
    }
    #[rstest::rstest]
    fn default_preferences_wire_minimap_to_section_default() {
        // Given default preferences.
        let prefs = UserPreferences::default();

        // Then the minimap section equals its own default.
        assert_eq!(prefs.minimap, MinimapConfig::default());
    }

    #[rstest::rstest]
    fn default_preferences_wire_auto_prune_to_section_default() {
        let prefs = UserPreferences::default();
        assert_eq!(prefs.auto_prune, AutoPruneConfig::default());
    }

    #[rstest::rstest]
    fn mcp_server_patch_preserves_user_comments() {
        // Given an existing jinn.toml with a user comment on an mcp_server entry.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"# top-level comment
[mcp_server.excalimate]
# this comment must survive
command = "npx"
args = ["@excalimate/mcp-server", "--stdio"]
"#,
        )
        .expect("write");

        // When saving the same config back.
        let prefs = UserPreferences {
            mcp_server: [(
                "excalimate".to_owned(),
                jinn_mcp_msg::McpServerConfig {
                    command: Some("npx".to_owned()),
                    args: vec!["@excalimate/mcp-server".to_owned(), "--stdio".to_owned()],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..UserPreferences::default()
        };
        save_preferences_to(&prefs, &path).expect("save");

        // Then the user comment is preserved on disk.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert!(
            on_disk.contains("# this comment must survive"),
            "user comment was wiped by the patcher: {on_disk}"
        );
    }

    #[rstest::rstest]
    fn default_preferences_has_no_mcp_servers() {
        // Given default preferences.
        let prefs = UserPreferences::default();

        // Then no MCP servers are configured by default.
        assert!(prefs.mcp_server.is_empty());
    }

    #[rstest::rstest]
    fn save_rewrites_legacy_alias_so_patched_file_still_parses() {
        // Given a legacy jinn.toml using the `min_tail_entries` alias and
        // a plugin entry (the shape `plugin install` produces when it
        // patches a file that predates the canonical key).
        let original = r#"[auto_prune.broken_edit]
enabled = true
min_tail_entries = 10

[plugin.p]
wasm = "p.wasm"
enabled = true
http = false
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When saving preferences back (the patch inserts the canonical
        // `min_age` key alongside the preserved alias key).
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("save");

        // Then the file no longer carries both keys, so it still parses.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            !written.contains("min_tail_entries"),
            "alias survived: {written}"
        );
        let reparsed = load_preferences_from(&path).expect("reparse");
        assert_eq!(reparsed.auto_prune.broken_edit.min_age, 10);
    }

    #[rstest::rstest]
    fn load_accepts_projects_table_key() {
        // Given a jinn.toml using the canonical [[projects]] key.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[[projects]]\npath = \"~/code/current\"\n").expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the entry deserializes.
        assert_eq!(prefs.projects.len(), 1);
        assert_eq!(prefs.projects[0].path.to_string_lossy(), "~/code/current");
    }

    #[rstest::rstest]
    fn load_ignores_legacy_project_blocks() {
        // Given a jinn.toml written by an older jinn using the legacy
        // [[project]] key, which is no longer an alias.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[[project]]\npath = \"~/code/legacy\"\n").expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the legacy block is inert: no projects deserialize.
        assert!(prefs.projects.is_empty());
    }

    #[rstest::rstest]
    fn save_strips_legacy_project_key() {
        // Given a poisoned jinn.toml: the legacy [[project]] block plus a
        // canonical [[projects]] entry (the shape `jinn plugin add` patches
        // into a pre-flip file) and a banner comment the user should keep.
        let original = r#"# my hand-edited banner
[[project]]
path = "~/code/legacy"

[[projects]]
path = "~/code/current"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When saving preferences back (patch inserts canonical [[projects]]
        // alongside the preserved legacy block without normalization).
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("save");

        // Then the legacy key is gone from disk.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            !written.contains("[[project]]"),
            "legacy [[project]] survived: {written}"
        );
        // And the canonical entry and banner comment survive.
        assert!(written.contains("[[projects]]"), "{written}");
        assert!(written.contains("~/code/current"), "{written}");
        assert!(written.contains("# my hand-edited banner"), "{written}");
    }

    #[rstest::rstest]
    fn save_projects_roundtrip_is_idempotent() {
        // Given a poisoned jinn.toml: an inline `projects` array plus a
        // legacy [[project]] block appended by an older tool.
        let original = r#"# banner
projects = [{path = "~/code/inline"}]

[[project]]
path = "~/code/legacy"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When saving twice with no preference changes in between.
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("first save");
        let first = std::fs::read_to_string(&path).expect("read first");
        let prefs = load_preferences_from(&path).expect("reload");
        save_preferences_to(&prefs, &path).expect("second save");

        // Then the second save is byte-identical: the first save converged
        // the document (legacy block dropped, inline array merged into the
        // keyed [[projects]] list) and repeated saves add nothing.
        let second = std::fs::read_to_string(&path).expect("read second");
        assert_eq!(first, second);
    }

    #[rstest::rstest]
    fn save_preserves_comments_when_command_policy_written() {
        // Given a jinn.toml whose [[projects]] entry carries a hand-commented
        // command policy (inline array of inline tables).
        let original = concat!(
            "# my banner\n",
            "[[projects]]\n",
            "path = \"~/code/jinn\"\n",
            "# blocks slow builds\n",
            "command_policy = [{pattern = \"cargo test -p\", message = \"use just test\"}]\n",
        );
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading, changing nothing, and saving back.
        let prefs = load_preferences_from(&path).expect("load");
        save_preferences_to(&prefs, &path).expect("save");

        // Then the policy survives the patch intact.
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("command_policy"), "key lost: {written}");
        assert!(written.contains("cargo test -p"), "pattern lost: {written}");
        assert!(written.contains("use just test"), "message lost: {written}");
        // And the comment above the policy is preserved.
        assert!(
            written.contains("# blocks slow builds"),
            "policy comment lost: {written}"
        );
    }

    #[rstest::rstest]
    fn load_heals_duplicate_project_keys() {
        // Given a jinn.toml that defines `projects` twice — once as an
        // inline array, once as [[projects]] blocks (the poisoned shape a
        // tool can produce by appending blocks to a file that already had
        // the inline array). serde rejects a duplicate key, so the first
        // parse fails and the load-retry heal strips the legacy [[project]]
        // path... this fixture instead relies on the heal only for the
        // duplicated-key shape it can fix: `project` blocks colliding with
        // the patcher. Here the plain parse fails on the duplicate
        // `projects` key, healing removes nothing applicable, and the
        // retry fails too.
        //
        // The real heal observable: a file poisoned with legacy [[project]]
        // blocks is *inert* on load (see load_ignores_legacy_project_blocks)
        // and cleaned on save/load-heal paths that strip it. This test
        // pins the observable contract for the case load CAN fix — the
        // duplicate `projects` key itself is not healable and must fail
        // loudly rather than silently drop data.
        let original = r#"
projects = [{path = "~/code/inline"}]

[[projects]]
path = "~/code/current"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading a file with a duplicate key — genuinely ambiguous.
        let result = load_preferences_from(&path);

        // Then the load fails (duplicate-key files are not silently healed).
        assert!(result.is_err());
        // And the file is left untouched on disk.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, original);
    }

    #[rstest::rstest]
    fn load_heals_legacy_project_blocks_into_clean_load() {
        // Given a jinn.toml whose serde-visible shape is poisoned ONLY by
        // the legacy key path: a [[project]] block whose key-decor comment
        // is glued to the [[projects]] header that follows it. Without
        // healing this parses... and with it, the dead block is stripped
        // and the file rewritten clean. To force the heal path (first
        // parse must FAIL), the fixture additionally contains a
        // `[[projects]]` header with a key decor corrupted the way a
        // pre-fix jinn patch would have written it (`[[# c\nprojects ]]`).
        let original = r#"# user banner
[[# c
projects ]]
path = "~/code/current"
"#;
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, original).expect("write");

        // When loading.
        let result = load_preferences_from(&path);

        // Then the load fails: this corruption is not TOML-parseable at
        // all, so no heal can recover it — and must not be silently
        // rewritten.
        assert!(result.is_err());
        // And the file is untouched.
        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, original);
    }

    #[rstest::rstest]
    fn load_accepts_projects_inline_array() {
        // Given a jinn.toml expressing projects as an inline array of
        // inline tables (valid TOML, same value as [[projects]] blocks).
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "projects = [{path = \"~/code/a\"}, {path = \"~/code/b\"}]\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then both entries deserialize in order.
        assert_eq!(prefs.projects.len(), 2);
        assert_eq!(prefs.projects[0].path.to_string_lossy(), "~/code/a");
        assert_eq!(prefs.projects[1].path.to_string_lossy(), "~/code/b");
    }

    #[rstest::rstest]
    fn save_project_without_policy_writes_no_command_policy_key() {
        // Given a jinn.toml with a policy-less project and no other changes.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[[projects]]\npath = \"~/code/a\"\n").expect("write");

        // When loading and saving back.
        let prefs = load_preferences_from(&path).expect("load");
        assert!(prefs.projects[0].command_policy.is_empty());
        save_preferences_to(&prefs, &path).expect("save");

        // Then the written file carries no `command_policy` key (the
        // empty policy is skipped in serialization, so a save never
        // introduces the key to existing config).
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            !written.contains("command_policy"),
            "empty policy must not materialize on save: {written}"
        );
        // And the project entry survives intact.
        assert!(written.contains("[[projects]]"), "{written}");
        assert!(written.contains("~/code/a"), "{written}");
    }

    #[rstest::rstest]
    fn load_reads_command_policy_from_project_entry() {
        // Given a jinn.toml whose [[projects]] entry declares a command policy.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            "[[projects]]\npath = \"~/code/jinn\"\ncommand_policy = [{pattern = \"cargo test -p\", message = \"use just test\"}]\n",
        )
        .expect("write");

        // When loading.
        let prefs = load_preferences_from(&path).expect("load");

        // Then the policy deserializes with pattern and message intact.
        assert_eq!(prefs.projects.len(), 1);
        assert_eq!(prefs.projects[0].command_policy.len(), 1);
        assert_eq!(prefs.projects[0].command_policy[0].pattern, "cargo test -p");
        assert_eq!(prefs.projects[0].command_policy[0].message, "use just test");
    }
}

#[cfg(test)]
mod openrouter_web_search_config_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use tempfile::TempDir;

    use super::OpenrouterWebSearchConfig;
    use crate::user_preferences::load_preferences_from;
    use jinn_common::app_info::PREFS_FILE_NAME;

    #[rstest::rstest]
    fn load_parses_openrouter_web_search_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[openrouter_web_search]
engine = "parallel"
max_results = 5
max_total_results = 20
search_context_size = "medium"
allowed_domains = ["nature.com", "arxiv.org"]
excluded_domains = ["spam.com"]
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");

        assert_eq!(
            prefs.openrouter_web_search.engine.as_deref(),
            Some("parallel")
        );
        assert_eq!(prefs.openrouter_web_search.max_results, Some(5));
        assert_eq!(prefs.openrouter_web_search.max_total_results, Some(20));
        assert_eq!(
            prefs.openrouter_web_search.search_context_size.as_deref(),
            Some("medium")
        );
        assert_eq!(
            prefs.openrouter_web_search.allowed_domains,
            Some(vec!["nature.com".to_owned(), "arxiv.org".to_owned()])
        );
        assert_eq!(
            prefs.openrouter_web_search.excluded_domains,
            Some(vec!["spam.com".to_owned()])
        );
    }

    #[rstest::rstest]
    fn load_without_openrouter_web_search_section_uses_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"last_model = "ollama/llama3"
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");

        let defaults = OpenrouterWebSearchConfig::default();
        assert_eq!(prefs.openrouter_web_search.engine, defaults.engine);
        assert_eq!(
            prefs.openrouter_web_search.max_results,
            defaults.max_results
        );
        assert_eq!(
            prefs.openrouter_web_search.max_total_results,
            defaults.max_total_results
        );
        assert_eq!(
            prefs.openrouter_web_search.search_context_size,
            defaults.search_context_size
        );
        assert_eq!(
            prefs.openrouter_web_search.allowed_domains,
            defaults.allowed_domains
        );
        assert_eq!(
            prefs.openrouter_web_search.excluded_domains,
            defaults.excluded_domains
        );
    }
}
