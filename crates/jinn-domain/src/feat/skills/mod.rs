//! Agent skills - the kernel UI-bound surface.
//!
//! The portable core (data model, frontmatter parsing, scanning, prompt
//! formatting, loaded-name vocabulary) lives in the `jinn-skills` slice;
//! the crossing contracts (`SkillsLoaded`, `ScanSkills`) live in
//! `jinn-skills-msg`. What remains here is the UI-bound trio: the picker
//! entry type, the preview cache, and the picker reload helper. The
//! re-exports below keep the long-standing `crate::feat::skills::X`
//! paths resolving inside the kernel.

pub mod reload;
pub mod skill_entry;
pub mod skill_preview_cache;

pub use jinn_skills::Skill;
pub use jinn_skills::SkillFrontmatter;
pub use jinn_skills::SkillSource;
pub use jinn_skills::format_skills_for_prompt;
pub use jinn_skills::frontmatter::strip_frontmatter;
pub use jinn_skills::loaded_skill_summary_label;
pub use jinn_skills::parse_loaded_skill_name;
pub use jinn_skills::scan_skills;
pub use jinn_skills_msg::{ScanSkills, SkillsLoaded};
pub use skill_entry::SkillEntry;
pub use skill_preview_cache::SkillPreviewCache;
