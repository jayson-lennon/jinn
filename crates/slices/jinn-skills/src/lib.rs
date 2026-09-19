//! The skills slice — the portable core of agent-skill support.
//!
//! Owns the skill data model ([`Skill`], [`SkillSource`]), YAML
//! frontmatter parsing, directory scanning, prompt formatting, and the
//! loaded-skill label vocabulary. Everything here is UI-free and
//! kernel-free: the session-init slice scans with it, the tools slice
//! loads skills with it, and the kernel's UI-bound trio (picker entry,
//! preview cache, picker reload) consumes it.
//!
//! Crossing contracts ([`SkillsLoaded`](jinn_skills_msg::SkillsLoaded),
//! [`ScanSkills`](jinn_skills_msg::ScanSkills)) live in `jinn-skills-msg`.

pub mod format;
pub mod frontmatter;
pub mod loaded_name;
pub mod scan;
pub mod skill;

pub use format::format_skills_for_prompt;
pub use loaded_name::loaded_skill_summary_label;
pub use loaded_name::parse_loaded_skill_name;
pub use loaded_name::SKILL_CONTENT_PREFIX;
pub use loaded_name::SKILL_ICON;
pub use frontmatter::SkillFrontmatter;
pub use scan::scan_skills;
pub use skill::{Skill, SkillSource};
