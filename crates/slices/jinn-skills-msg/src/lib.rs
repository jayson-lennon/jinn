//! Skills crossing contracts — the events and commands other slices
//! consume. The scanning actor lives in the session-init slice; kernel
//! consumers (the session actor, the task settle listener) reference
//! these types and the reverse bridge carries these exact Rust types.
//!
//! The crossing-schema ids ("SkillsLoaded", "ScanSkills") are wire
//! contract and must not be renamed.

mod frontmatter;
mod loaded_name;
mod skill;
mod skill_filter;
#[cfg(test)]
mod skill_filter_tests;
mod skill_picker_scope;
mod skill_picker_state;
mod skill_preview_cache;

pub use frontmatter::SkillFrontmatter;
pub use loaded_name::{
    SKILL_CONTENT_PREFIX, SKILL_ICON, loaded_skill_summary_label, parse_loaded_skill_name,
};
pub use skill::{Skill, SkillSource};
pub use skill_filter::{FilteredSkillRow, SkillRowSegments, filter_skills};
pub use skill_picker_scope::skill_picker_scope;
pub use skill_picker_state::{
    RESULTS_VIEWPORT_FALLBACK, SkillEntry, SkillPickerState, body_hash_key, skill_picker_slot,
    skill_row,
};
pub use skill_preview_cache::SkillPreviewCache;

use marked_path::{Absolute, MarkedPathBuf};
use serde::{Deserialize, Serialize};

/// Emitted when skills have been scanned and loaded.
///
/// On success, `skills` contains the discovered skills and `error` is `None`.
/// On failure, `skills` is empty and `error` contains a description.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Skills have been scanned and loaded for a session.")]
pub struct SkillsLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: jinn_core_types::SessionId,
    /// The discovered agent skills.
    pub skills: Vec<Skill>,
    /// Error message if scanning failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Command to trigger a skills scan for a specific session.
///
/// Carries the session's cwd: the discovery worker scans global +
/// project dirs discovered via the bounded walk, and writes the merged
/// result into that session's ephemeral discovered-skills set.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Trigger a skills scan for a session.")]
pub struct ScanSkills {
    /// The session whose scan this is.
    pub session_id: jinn_core_types::SessionId,
    /// The working directory driving the scan.
    pub cwd: MarkedPathBuf<Absolute>,
}

impl jinn_slices::BusMessage for SkillsLoaded {}

impl jinn_slices::BusMessage for ScanSkills {}
