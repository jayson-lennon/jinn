//! The missing-compaction-block message shared by both emission points.
//!
//! A persona without a well-formed `<compaction>` block is an error
//! condition the user must see: the entry names the persona and states
//! that handoff compaction will use the shipped default handoff prompt.
//! The message is one pure function so the switch path (persona picker)
//! and the load path (session store) emit byte-identical text — the
//! load-path dedupe compares this exact text.

/// The error text pushed into a session's chat history when its persona
/// carries no `<compaction>` handoff block.
///
/// Names the persona and states the fallback, per the feature's
/// missing-block decision: fallbacks apply for everything and the error
/// says so.
#[must_use]
pub fn missing_compaction_block_message(persona_name: &str) -> String {
    format!(
        "persona \"{persona_name}\" carries no <compaction> handoff block; \
         handoff compaction will use the default handoff prompt"
    )
}

#[cfg(test)]
mod tests {
    use super::missing_compaction_block_message;

    #[rstest::rstest]
    #[test]
    fn message_names_the_persona_and_states_the_fallback() {
        // Given a persona name.
        // When building the missing-block message.
        let message = missing_compaction_block_message("brainstorm");

        // Then it names the persona and states the default-prompt fallback.
        assert!(message.contains("\"brainstorm\""));
        assert!(message.contains("default handoff prompt"));
    }
}
