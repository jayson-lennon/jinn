//! Persona markdown parsing.
//!
//! Ported from the retired `persona-loader` component: `+++`-delimited TOML
//! frontmatter (`name`, optional `description`) followed by the body.
//! The wire hop's `Option<String>` description is collapsed here — the
//! domain `Persona` carries an empty string when the frontmatter omits
//! the field, matching what the coordinator's translation produced.
//!
//! The body may also carry one `<compaction>` handoff block. It is extracted
//! into [`Persona::handoff_prompt`] and stripped from the body at parse time:
//! the body is sent verbatim as the system prompt, so the block must never
//! leak into it. Tags match exactly (`<compaction>`, no attributes); an
//! unclosed opening tag is treated as missing and stripped to end-of-body.

use jinn_slices::Persona;
use marked_path::{Absolute, MarkedPathBuf};
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
pub fn parse_persona_file(
    path: &MarkedPathBuf<Absolute>,
) -> Result<Persona, error_stack::Report<PersonaParseError>> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        error_stack::Report::new(PersonaParseError::Io)
            .attach(format!("failed to read {path}"))
            .attach(format!("{error}"))
    })?;
    parse_persona_content(&content)
}

/// The opening delimiter of a persona's `<compaction>` handoff block.
const COMPACTION_OPEN: &str = "<compaction>";
/// The closing delimiter of a persona's `<compaction>` handoff block.
const COMPACTION_CLOSE: &str = "</compaction>";

/// Splits a persona body into its clean system-prompt text and its handoff
/// block.
///
/// Returns `(stripped_body, handoff_block)`. The handoff block is the trimmed
/// text of the first well-formed `<compaction>…</compaction>` span, or empty
/// when no well-formed block exists. Every block span is removed from the
/// body — including unclosed tails, which are stripped to end-of-body — so
/// the stripped body is always safe to send verbatim as the system prompt.
///
/// Matching is exact: `<compaction foo>` or `</compaction >` do not match and
/// remain ordinary body text.
fn split_compaction_block(body: &str) -> (String, String) {
    /// Strips every block span from `body`, capturing the first block's text.
    ///
    /// One pass: split on the open tag, then split what follows on the close
    /// tag. A well-formed span contributes its inner text (captured once) and
    /// scanning resumes after the close tag; an unclosed tail is dropped
    /// whole — the dangling tag must never reach the system prompt.
    fn strip_blocks(body: &str) -> (String, Option<String>) {
        let mut stripped = String::with_capacity(body.len());
        let mut first_block = None;
        let mut rest = body;
        loop {
            let Some((before, after_open)) = rest.split_once(COMPACTION_OPEN) else {
                stripped.push_str(rest);
                break;
            };
            stripped.push_str(before);
            match after_open.split_once(COMPACTION_CLOSE) {
                Some((inner, after_close)) => {
                    if first_block.is_none() {
                        first_block = Some(inner.trim().to_owned());
                    }
                    rest = after_close;
                }
                None => break,
            }
        }
        (stripped, first_block)
    }

    let (stripped, handoff_block) = strip_blocks(body);
    let stripped = stripped.trim_start_matches('\n').trim_end().to_owned();
    (stripped, handoff_block.unwrap_or_default())
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

    let raw_body = body_rest.trim_start_matches('\n').trim_end();
    let (body, handoff_prompt) = split_compaction_block(raw_body);

    Ok(Persona {
        name: frontmatter.name,
        description: frontmatter.description,
        body,
        handoff_prompt,
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
    fn shipped_persona_shape_parses_with_its_grounding() {
        // Given persona content shaped like the shipped ones: quoted
        // frontmatter fields and a description naming the method it is
        // grounded in.
        let content = concat!(
            "+++\n",
            "name = \"learning-tutor\"\n",
            "description = \"Evidence-based tutor using intelligent-tutoring dialogue moves (AutoTutor)\"\n",
            "+++\n\n",
            "Body.\n",
        );

        // When parsing it.
        let persona = parse_persona_content(content).expect("parse");

        // Then it keeps its name and an evidence-grounded description.
        assert_eq!(persona.name, "learning-tutor");
        assert!(
            persona.description.contains("intelligent-tutoring"),
            "description should mention intelligent-tutoring, got: {}",
            persona.description
        );
    }

    #[rstest::rstest]
    fn parse_extracts_and_strips_a_well_formed_compaction_block() {
        // Given persona content whose body carries a well-formed block.
        let content = concat!(
            "+++\n",
            "name = \"brainstorm\"\n",
            "+++\n\n",
            "You facilitate brainstorms.\n\n",
            "<compaction>\n",
            "Recap the thesis, the critiques, and the open questions.\n",
            "</compaction>\n",
        );

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then the block text lands in handoff_prompt, trimmed.
        assert_eq!(
            persona.handoff_prompt,
            "Recap the thesis, the critiques, and the open questions."
        );
        // And the body carries neither the tags nor the block text.
        assert!(!persona.body.contains("<compaction>"));
        assert!(!persona.body.contains("</compaction>"));
        assert!(!persona.body.contains("Recap the thesis"));
    }

    #[rstest::rstest]
    fn parse_preserves_a_body_without_a_compaction_block() {
        // Given persona content whose body carries no block.
        let content = "+++\nname = \"plain\"\n+++\n\nJust the prompt text.\n";

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then handoff_prompt is empty and the body is unchanged.
        assert_eq!(persona.handoff_prompt, "");
        assert_eq!(persona.body, "Just the prompt text.");
    }

    #[rstest::rstest]
    fn parse_treats_an_unclosed_block_as_missing_but_strips_it() {
        // Given persona content whose body opens a block it never closes.
        let content = concat!(
            "+++\n",
            "name = \"broken\"\n",
            "+++\n\n",
            "Prompt prefix.\n\n",
            "<compaction>\n",
            "This block was never closed.",
        );

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then the block counts as missing (fallback applies).
        assert_eq!(persona.handoff_prompt, "");
        // And the dangling tag never reaches the body.
        assert!(!persona.body.contains("<compaction>"));
        assert_eq!(persona.body, "Prompt prefix.");
    }

    #[rstest::rstest]
    fn parse_captures_only_the_first_block_and_strips_all_of_them() {
        // Given persona content whose body carries two well-formed blocks.
        let content = concat!(
            "+++\n",
            "name = \"doubled\"\n",
            "+++\n\n",
            "Before one.\n\n",
            "<compaction>\nFirst block text.\n</compaction>\n\n",
            "Between.\n\n",
            "<compaction>\nSecond block text.\n</compaction>\n",
        );

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then only the first block is captured.
        assert_eq!(persona.handoff_prompt, "First block text.");
        // And every block span is gone from the body.
        assert!(!persona.body.contains("<compaction>"));
        assert!(!persona.body.contains("First block text."));
        assert!(!persona.body.contains("Second block text."));
        assert!(persona.body.contains("Before one."));
        assert!(persona.body.contains("Between."));
    }

    #[rstest::rstest]
    fn parse_leaves_an_inexact_opening_tag_as_ordinary_body_text() {
        // Given persona content with an attributed opening tag that does not
        // match exactly.
        let content = concat!(
            "+++\n",
            "name = \"attributed\"\n",
            "+++\n\n",
            "Prompt text.\n\n",
            "<compaction mode=\"strict\">\nclosed here\n</compaction>\n",
        );

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then the attributed tag is ordinary body text and no block exists.
        assert_eq!(persona.handoff_prompt, "");
        assert!(persona.body.contains("<compaction mode=\"strict\">"));
    }

    #[rstest::rstest]
    fn parse_handles_a_block_directly_adjacent_to_the_body() {
        // Given persona content where the block starts mid-line with no
        // separating blank line.
        let content = concat!(
            "+++\n",
            "name = \"adjacent\"\n",
            "+++\n\n",
            "Prompt text. <compaction>inline block</compaction> and more.\n",
        );

        // When parsing.
        let persona = parse_persona_content(content).expect("parse");

        // Then the block is still extracted and stripped cleanly.
        assert_eq!(persona.handoff_prompt, "inline block");
        assert_eq!(persona.body, "Prompt text.  and more.");
    }
}
