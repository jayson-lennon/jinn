//! Curated project directories - the list surfaced by the project picker.
//!
//! A "project" is simply a directory the user wants to keep on file so they can
//! spin up a new session rooted there in one step (see the project picker, bound
//! to `<leader>so`). Unlike an auto-tracked MRU, this list is purely curated: the
//! user adds and removes entries explicitly, so it never drifts with usage.
//!
//! Defined in `jinn.toml` under `[[project]]` and persisted comment-preserving
//! via the [`DocumentPatcher`](crate::common::toml_patch::DocumentPatcher).

pub mod picker_entry;
pub mod resolver;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One blocked-command rule enforced by the bash tool inside a project.
///
/// A rule pairs a user-authored regex with the corrective message returned
/// when the regex matches a command. Rules are advisory-strength by design:
/// they exist to stop well-trained habits (like `cargo test -p` in a
/// whole-workspace repo), not to resist a determined actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPolicyRule {
    /// Regex matched against the full command string.
    pub pattern: String,
    /// Message returned in the failed tool result when [`Self::pattern`] matches.
    pub message: String,
}

/// A curated project directory shown in the project picker.
///
/// Defined in `jinn.toml` under `[[project]]`. The `path` field is the array
/// key the `DocumentPatcher` matches entries by, so add/remove operations
/// target a single table without disturbing siblings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// The absolute (or `~`-prefixed) directory path.
    pub path: PathBuf,
    /// Blocked-command rules the bash tool enforces for commands whose cwd
    /// falls inside this project. Empty means no policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_policy: Vec<CommandPolicyRule>,
}
