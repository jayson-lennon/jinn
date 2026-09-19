//! Skill data model.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a [`Skill`] was discovered from.
///
/// Used to badge entries in the skill picker (global vs project-scoped)
/// and to resolve provenance when a project skill overrides a global one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum SkillSource {
    /// Discovered from the user-global skills dir (`~/.agents/skills`).
    #[default]
    Global,
    /// Discovered from a project-local `.agents/skills` dir.
    Project {
        /// The walked directory (the ancestor containing `.agents/`),
        /// not the `.agents/skills` subdirectory.
        dir: PathBuf,
    },
}

/// A discovered agent skill.
///
/// Parsed from `SKILL.md` files in `~/.agents/skills/<name>/`.
/// The name comes from the YAML frontmatter (must match the parent directory name).
/// The description comes from the YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Skill {
    /// The skill name (from frontmatter, must match parent directory).
    pub name: String,
    /// Human-readable description of what the skill does.
    pub description: String,
    /// The markdown body content (after stripping YAML frontmatter).
    /// Not serialized - loaded fresh from disk on each scan.
    #[serde(skip)]
    pub body: String,
    /// Absolute path to the SKILL.md file.
    pub file_path: PathBuf,
    /// Absolute path to the skill's base directory (parent of SKILL.md).
    pub base_dir: PathBuf,
    /// Where this skill was discovered from.
    #[serde(default)]
    pub source: SkillSource,
}
