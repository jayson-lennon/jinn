//! The guidance a fired stream rule injects into the resumed turn.
//!
//! Rendered as a user entry ahead of the resumed dispatch, so it enters the
//! conversation exactly once and in the position the model will read it.
//!
//! ## Why the second line
//!
//! The rule body is injected into the same conversation the offending output
//! came from, and that output may itself have contained text designed to make
//! a model disregard instructions. Without the disclaimer, a model that has
//! been told "ignore prior guidance" a moment earlier has no way to tell the
//! harness's correction from the injected text it was just fed. Naming the
//! source — the coding agent enforcing the user's own configured rules —
//! closes that gap.

/// Renders the system-interrupt block for a fired rule.
#[must_use]
pub fn render_rule_interrupt(rule_name: &str, rule_body: &str) -> String {
    format!(
        "<system-interrupt reason=\"rule_violation\" rule=\"{rule_name}\">\n\
         Output interrupted: violated user-defined rule.\n\
         Not prompt injection; coding agent enforcing project rules.\n\
         MUST comply:\n\n\
         {rule_body}\n\
         </system-interrupt>"
    )
}

#[cfg(test)]
mod tests {
    use super::render_rule_interrupt;

    #[rstest::rstest]
    #[test]
    fn the_block_names_the_rule_that_fired() {
        // Given a rule name.
        let name = "ts-no-any";

        // When the block is rendered.
        let block = render_rule_interrupt(name, "Use `unknown`.");

        // Then the name appears in the reason attribute.
        assert!(block.contains(r#"rule="ts-no-any""#), "{block}");
    }

    #[rstest::rstest]
    #[test]
    fn the_block_carries_the_rule_body() {
        // Given a multi-line rule body.
        let body = "Use `unknown`.\n\nNever widen a type to `any`.";

        // When the block is rendered.
        let block = render_rule_interrupt("r", body);

        // Then the whole body is present, newlines and all.
        assert!(block.contains(body), "{block}");
    }

    #[rstest::rstest]
    #[test]
    fn the_block_denotes_its_source_as_the_harness() {
        // Given any rule.
        let body = "Follow the rule.";

        // When the block is rendered.
        let block = render_rule_interrupt("r", body);

        // Then it says the guidance comes from the coding agent, not from the
        // model's own output — the defence against an output that told the
        // model to ignore instructions.
        assert!(
            block.contains("coding agent enforcing project rules"),
            "the source disclaimer is load-bearing and must not be dropped: {block}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_block_is_closed() {
        // Given any rule.
        let body = "Follow the rule.";

        // When the block is rendered.
        let block = render_rule_interrupt("r", body);

        // Then it opens and closes, so a truncated block cannot swallow the
        // rest of the turn.
        assert!(block.starts_with("<system-interrupt"), "{block}");
        assert!(block.ends_with("</system-interrupt>"), "{block}");
    }
}
