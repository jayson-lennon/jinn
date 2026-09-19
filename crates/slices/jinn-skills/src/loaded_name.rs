//! Extracting the loaded-skill name from a pinned skill ToolResult's content.
//!
//! A successful `skill` tool load emits a ToolResult whose `content` is XML of
//! the form `<skill name="X" location="...">...</skill>`. This module parses
//! the name out of that content so renderers and `loaded_skills()` share one
//! source of truth instead of each inlining the prefix-strip logic.

/// The prefix that introduces a pinned skill's content.
pub const SKILL_CONTENT_PREFIX: &str = "<skill name=\"";

/// Extract the skill name from a pinned skill ToolResult's content.
///
/// Returns `None` if `content` does not begin with `<skill name="X"` for some
/// non-empty `X`, or if the closing quote is missing/truncated. Never panics.
///
/// # Examples
///
/// ```
/// # use jinn_skills::parse_loaded_skill_name;
/// let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";
/// assert_eq!(parse_loaded_skill_name(content), Some("phased-task-loop"));
/// ```
#[must_use]
pub fn parse_loaded_skill_name(content: &str) -> Option<&str> {
    let rest = content.strip_prefix(SKILL_CONTENT_PREFIX)?;
    let end = rest.find('"')?;
    let name = rest.get(..end)?;
    if name.is_empty() { None } else { Some(name) }
}

/// The single-cell diamond icon shown before a loaded skill name in the UI.
///
/// Uses U+2756 (BLACK DIAMOND MINUS WHITE X) so it occupies exactly one
/// terminal cell — unlike the earlier double-wide emoji — keeping layout
/// width math exact. Reused by both the pins sidebar and the chat log so the
/// rendered label is identical everywhere.
pub const SKILL_ICON: &str = "\u{2756}"; // ❖

/// Build the single-line summary label for a loaded skill, e.g.
/// `❖ phased-task-loop`.
///
/// Returns a graceful fallback (`❖ (skill)`) if the content is malformed
/// or the name cannot be parsed. Both UI renderers call this so they cannot
/// drift on label format.
///
/// # Examples
///
/// ```
/// # use jinn_skills::loaded_skill_summary_label;
/// let content = "<skill name=\"web-coder\" location=\"/x\">body</skill>";
/// assert!(loaded_skill_summary_label(content).contains("web-coder"));
/// ```
#[must_use]
pub fn loaded_skill_summary_label(content: &str) -> String {
    match parse_loaded_skill_name(content) {
        Some(name) => format!("{SKILL_ICON} {name}"),
        None => format!("{SKILL_ICON} (skill)"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    fn valid_content_returns_skill_name() {
        // Given a well-formed skill content string.
        let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then the skill name is returned.
        assert_eq!(name, Some("phased-task-loop"));
    }

    #[rstest::rstest]
    fn content_with_body_and_location_returns_name_only() {
        // Given a skill content with a long body and location path.
        let content = "<skill name=\"rust-programming\" location=\"/home/u/.agents/skills/.../SKILL.md\">\n\
             lots\nof\nbody\n</skill>";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then only the name is returned (not the body or location).
        assert_eq!(name, Some("rust-programming"));
    }

    #[rstest::rstest]
    fn content_without_prefix_returns_none() {
        // Given content that is not a skill XML.
        let content = "some bash output";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then nothing is returned.
        assert_eq!(name, None);
    }

    #[rstest::rstest]
    fn truncated_prefix_missing_closing_quote_returns_none() {
        // Given a truncated skill prefix with no closing quote.
        let content = "<skill name=\"ph";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then nothing is returned and it does not panic.
        assert_eq!(name, None);
    }

    #[rstest::rstest]
    fn empty_string_returns_none() {
        // Given an empty string.

        // When parsing.
        let name = parse_loaded_skill_name("");

        // Then nothing is returned.
        assert_eq!(name, None);
    }

    #[rstest::rstest]
    fn empty_name_returns_none() {
        // Given a skill content with an empty name.
        let content = "<skill name=\"\" location=\"/x\">body</skill>";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then nothing is returned (empty names are rejected by the guard).
        assert_eq!(name, None);
    }

    #[rstest::rstest]
    fn embedded_quote_after_name_returns_first_segment_only() {
        // Given a skill content whose body contains a quote.
        let content = "<skill name=\"web-coder\" location=\"/x\">a \"quoted\" body</skill>";

        // When parsing.
        let name = parse_loaded_skill_name(content);

        // Then only the segment up to the first closing quote is returned.
        assert_eq!(name, Some("web-coder"));
    }

    #[rstest::rstest]
    fn summary_label_contains_icon_and_name_for_valid_content() {
        // Given a well-formed skill content string.
        let content = "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>";

        // When building the summary label.
        let label = loaded_skill_summary_label(content);

        // Then it contains the icon and the skill name.
        assert!(
            label.contains('\u{2756}'),
            "label should contain the skill icon: {label}"
        );
        assert!(
            label.contains("phased-task-loop"),
            "label should contain the skill name: {label}"
        );
    }

    #[rstest::rstest]
    fn summary_label_uses_fallback_for_malformed_content() {
        // Given malformed (non-skill) content.
        let content = "not a skill";

        // When building the summary label.
        let label = loaded_skill_summary_label(content);

        // Then it contains the icon and the (skill) fallback.
        assert!(
            label.contains('\u{2756}'),
            "label should contain the skill icon: {label}"
        );
        assert!(
            label.contains("(skill)"),
            "label should contain the fallback: {label}"
        );
    }
}
