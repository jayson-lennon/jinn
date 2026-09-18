//! Project configuration schema — the `jinn.toml` `[[projects]]` entries.
//!
//! Pure serde data; the CWD resolver and the project picker spec stay in
//! the kernel and import the shape from here.

use std::path::PathBuf;

use jinn_tools_msg::CommandPolicyRule;
use serde::{Deserialize, Serialize};

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
