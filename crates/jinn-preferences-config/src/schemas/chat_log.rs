//! The `[chat_log]` section — the transcript's own display configuration.

use serde::{Deserialize, Serialize};

/// The `[chat_log]` section.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each render, so an edit applies at the next render.
///
/// Both fields are optional so an absent key falls through to the
/// reader's own built-in default — which is not the same number in
/// every reader, and is deliberately left that way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatLogConfig {
    /// How many lines a tool entry may contribute before it is folded.
    /// `None` = the reader's built-in default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_entry_max_lines: Option<u16>,

    /// How many consecutive tool entries collapse into one summary.
    /// `None` = the reader's built-in default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_collapse_count: Option<usize>,
}

impl jinn_config::Configurable for ChatLogConfig {
    const KEY: &'static str = "chat_log";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_config::Configurable;

    use super::ChatLogConfig;

    #[rstest::rstest]
    fn an_unset_key_falls_through_to_the_readers_default() {
        // Given a table that sets only the line cap.
        let table: toml::Table =
            toml::from_str("tool_entry_max_lines = 40").expect("test TOML parses");

        // When reading the section.
        let config = ChatLogConfig::from_table(&table).expect("section deserializes");

        // Then the set key is the document's and the unset one is None,
        // which the reader maps to its own default.
        assert_eq!(config.tool_entry_max_lines, Some(40));
        assert_eq!(config.min_collapse_count, None);
    }
}
