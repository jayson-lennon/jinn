//! Skill picker entry type and rendering.

use crate::feat::skills::SkillSource;
use crate::feat::theme::Theme;

/// A skill entry ready for display in the skill picker.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill name (unique identifier, e.g., "phased-task-loop", "web-coder").
    pub name: String,
    /// Human-readable skill description.
    pub description: String,
    /// Markdown body content (from SKILL.md, after stripping frontmatter).
    pub body: String,
    /// Whether the skill is currently enabled for this session.
    pub enabled: bool,
    /// Where this skill was discovered from (global vs project).
    pub source: SkillSource,
    /// Theme for styling.
    pub theme: Theme,
}

/// Stable cache key for a skill body: the decimal content hash.
///
/// Keyed on body content (not name) so that editing a SKILL.md or a project
/// skill shadowing a global of the same name produces a distinct cache entry
/// — the render cache never serves the wrong markdown. The skill spec's
/// `.preview_key` hook delegates here.
pub(crate) fn body_hash_key(body: &str) -> String {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(body.as_bytes());
    hasher.finish().to_string()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::single_range_in_vec_init,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn body_hash_key_distinguishes_body_not_name() {
        // Given two entries with the same name but different bodies, and a
        // third with a different name but the same body as the first.
        let a = body_hash_key("# body one");
        let b = body_hash_key("# body two");
        let c = body_hash_key("# body one");

        // When hashing the bodies.
        // Then same body -> same key, different body -> different key,
        // regardless of skill name.
        assert_eq!(a, c);
        assert_ne!(a, b);
    }
}
