//! YAML frontmatter parser for SKILL.md files.
//!
//! Extracts the YAML block between `---` delimiters at the start of a markdown file.
//! Uses a minimal hand-rolled parser instead of a full YAML library -
//! we only need `name` and `description` string fields.

/// Parsed frontmatter fields from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct SkillFrontmatter {
    /// The skill name (must match parent directory).
    pub name: Option<String>,
    /// The skill description.
    pub description: Option<String>,
}

/// Extracts and parses YAML frontmatter from a markdown file.
///
/// Frontmatter is delimited by `---` at the start of the file:
///
/// ```markdown
/// ---
/// name: my-skill
/// description: A description
/// ---
/// ```
///
/// Returns `None` if no frontmatter block is found.
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
pub fn parse_frontmatter(content: &str) -> Option<SkillFrontmatter> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Skip the opening ---
    let after_opening = trimmed.get(3..).unwrap_or("");
    let rest = after_opening
        .trim_start_matches('\n')
        .trim_start_matches('\r');

    // Find the closing ---
    let close_offset = rest.find("\n---")?;

    let yaml_block = rest.get(..close_offset)?;

    let mut name = None;
    let mut description = None;

    for line in yaml_block.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_owned());
        }
    }

    Some(SkillFrontmatter { name, description })
}

/// Strips YAML frontmatter from a markdown file, returning only the body.
pub fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_owned();
    }

    let after_opening = trimmed.get(3..).unwrap_or("");
    let rest = after_opening
        .trim_start_matches('\n')
        .trim_start_matches('\r');

    let Some(close_offset) = rest.find("\n---") else {
        return content.to_owned();
    };

    let body = rest.get(close_offset + 4..).unwrap_or("");
    body.trim_start().to_owned()
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
    fn parse_frontmatter_extracts_name_and_description() {
        // Given a SKILL.md with frontmatter.
        let content = "---\nname: my-skill\ndescription: A cool skill\n---\n\n# Content";

        // When parsing frontmatter.
        let fm = parse_frontmatter(content).expect("should have frontmatter");

        // Then name and description are extracted.
        assert_eq!(fm.name, Some("my-skill".to_owned()));
        assert_eq!(fm.description, Some("A cool skill".to_owned()));
    }

    #[rstest::rstest]
    fn parse_frontmatter_returns_none_without_delimiters() {
        // Given content without frontmatter.
        let content = "# Just markdown\n\nNo frontmatter here.";

        // When parsing frontmatter.
        let result = parse_frontmatter(content);

        // Then no frontmatter is found.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn parse_frontmatter_handles_missing_name() {
        // Given frontmatter with only description.
        let content = "---\ndescription: Only description\n---\n\n# Content";

        // When parsing frontmatter.
        let fm = parse_frontmatter(content).expect("should have frontmatter");

        // Then name is None and description is present.
        assert_eq!(fm.name, None);
        assert_eq!(fm.description, Some("Only description".to_owned()));
    }

    #[rstest::rstest]
    fn parse_frontmatter_handles_missing_description() {
        // Given frontmatter with only name.
        let content = "---\nname: only-name\n---\n\n# Content";

        // When parsing frontmatter.
        let fm = parse_frontmatter(content).expect("should have frontmatter");

        // Then name is present and description is None.
        assert_eq!(fm.name, Some("only-name".to_owned()));
        assert_eq!(fm.description, None);
    }

    #[rstest::rstest]
    fn strip_frontmatter_removes_yaml_block() {
        // Given a SKILL.md with frontmatter.
        let content =
            "---\nname: test\ndescription: test desc\n---\n\n# Skill Body\n\nSome content.";

        // When stripping frontmatter.
        let body = strip_frontmatter(content);

        // Then only the body remains.
        assert_eq!(body, "# Skill Body\n\nSome content.");
    }

    #[rstest::rstest]
    fn strip_frontmatter_returns_full_content_without_delimiters() {
        // Given content without frontmatter.
        let content = "# Just markdown\n\nNo frontmatter here.";

        // When stripping frontmatter.
        let body = strip_frontmatter(content);

        // Then the full content is returned unchanged.
        assert_eq!(body, content);
    }

    #[rstest::rstest]
    fn strip_frontmatter_handles_no_closing_delimiter() {
        // Given content with only opening delimiter.
        let content = "---\nname: broken\n\nNo closing delimiter.";

        // When stripping frontmatter.
        let body = strip_frontmatter(content);

        // Then the full content is returned (malformed frontmatter).
        assert_eq!(body, content);
    }
}
