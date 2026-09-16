//! Persona markdown parsing.
//!
//! Ported from the retired `persona-loader` plugin: `+++`-delimited TOML
//! frontmatter (`name`, optional `description`) followed by the body.
//! The wire hop's `Option<String>` description is collapsed here — the
//! domain `Persona` carries an empty string when the frontmatter omits
//! the field, matching what the coordinator's translation produced.

use std::path::Path;

use jinn_persona_msg::Persona;
use wherror::Error;

/// Persona-file parse failures (carried between parse helpers).
#[derive(Debug, Error)]
#[error(debug)]
pub enum PersonaParseError {
    /// Filesystem I/O failure.
    Io,
    /// Content does not start with `+++` or has no closing `+++`.
    Frontmatter,
    /// TOML parsing failed.
    Parse,
}

/// Frontmatter schema for persona files.
#[derive(Debug, serde::Deserialize)]
struct Frontmatter {
    /// Unique persona name.
    name: String,
    /// Short description.
    #[serde(default)]
    description: String,
}

/// Parses one persona file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or the frontmatter is malformed.
pub fn parse_persona_file(path: &Path) -> Result<Persona, error_stack::Report<PersonaParseError>> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        error_stack::Report::new(PersonaParseError::Io)
            .attach(format!("failed to read {}", path.display()))
            .attach(format!("{error}"))
    })?;
    parse_persona_content(&content)
}

/// Parses persona content (testable without the filesystem).
///
/// # Errors
///
/// Returns an error if the content has no `+++` frontmatter or malformed TOML.
pub fn parse_persona_content(
    content: &str,
) -> Result<Persona, error_stack::Report<PersonaParseError>> {
    use error_stack::ResultExt as _;

    let trimmed = content.trim_start();

    let Some(after_open) = trimmed.strip_prefix("+++") else {
        return Err(error_stack::Report::new(PersonaParseError::Frontmatter)
            .attach("content must start with +++ frontmatter delimiter"));
    };

    let Some((frontmatter_str, body_rest)) = after_open.split_once("\n+++") else {
        return Err(error_stack::Report::new(PersonaParseError::Frontmatter)
            .attach("missing closing +++ frontmatter delimiter"));
    };

    let frontmatter: Frontmatter = toml::from_str(frontmatter_str.trim())
        .change_context(PersonaParseError::Parse)
        .attach("failed to parse frontmatter TOML")?;

    let body = body_rest.trim_start_matches('\n').trim_end().to_owned();

    Ok(Persona {
        name: frontmatter.name,
        description: frontmatter.description,
        body,
    })
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
    fn parse_persona_content_with_valid_frontmatter() {
        // Given a valid persona file content.
        let content = "+++\nname = \"coding-assistant\"\ndescription = \"Expert coder\"\n+++\n\nYou are an expert coding assistant.\n";

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then fields are correctly extracted.
        assert_eq!(persona.name, "coding-assistant");
        assert_eq!(persona.description, "Expert coder");
        assert_eq!(persona.body, "You are an expert coding assistant.");
    }

    #[rstest::rstest]
    fn parse_persona_content_without_description_maps_to_empty() {
        // Given a persona without description.
        let content = "+++\nname = \"minimal\"\n+++\n\nBody text here.";

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then the description is the empty string (the domain contract).
        assert_eq!(persona.name, "minimal");
        assert_eq!(persona.description, "");
        assert_eq!(persona.body, "Body text here.");
    }

    #[rstest::rstest]
    fn parse_persona_content_fails_without_frontmatter() {
        // Given content without +++ delimiter.
        let content = "Just some text without frontmatter.";

        // When parsing.
        let result = parse_persona_content(content);

        // Then it fails with Frontmatter.
        assert!(matches!(
            result.unwrap_err().current_context(),
            PersonaParseError::Frontmatter
        ));
    }

    #[rstest::rstest]
    fn parse_persona_content_fails_without_closing_delimiter() {
        // Given content with opening +++ but no closing +++.
        let content = "+++\nname = \"test\"\nNo closing delimiter here.";

        // When parsing.
        let result = parse_persona_content(content);

        // Then it fails with Frontmatter.
        assert!(matches!(
            result.unwrap_err().current_context(),
            PersonaParseError::Frontmatter
        ));
    }

    #[rstest::rstest]
    fn parse_persona_content_fails_with_invalid_toml() {
        // Given content with invalid TOML in frontmatter.
        let content = "+++\nname = invalid toml\n+++\n\nBody.";

        // When parsing.
        let result = parse_persona_content(content);

        // Then it fails with Parse.
        assert!(matches!(
            result.unwrap_err().current_context(),
            PersonaParseError::Parse
        ));
    }

    #[rstest::rstest]
    fn parse_persona_content_preserves_multiline_body() {
        // Given a persona with multiline body.
        let content = "+++\nname = \"multi\"\n+++\n\nLine one.\nLine two.\nLine three.";

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then all body lines are preserved.
        assert!(persona.body.contains("Line one."));
        assert!(persona.body.contains("Line two."));
        assert!(persona.body.contains("Line three."));
    }

    #[rstest::rstest]
    fn shipped_learning_tutor_persona_parses_with_its_grounding() {
        // Given the shipped learning-tutor persona bundled into the repo.
        let content = include_str!("../../../../res/personas/learning-tutor.md");

        // When parsing it.
        let persona = parse_persona_content(content).expect("parse");

        // Then it keeps its shipped name and an evidence-grounded description.
        assert_eq!(persona.name, "learning-tutor");
        assert!(
            persona.description.contains("intelligent-tutoring"),
            "description should mention intelligent-tutoring, got: {}",
            persona.description
        );
    }
}
