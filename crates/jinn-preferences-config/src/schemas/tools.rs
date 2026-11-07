//! The `[tools]` section — the tools slice's own configuration.
//!
//! The fallback tool timeout, the two output caps the bash tool
//! applies, and the filter a new session starts under.

use jinn_core_types::NameFilter;
use serde::{Deserialize, Serialize};

/// The `[tools]` section.
///
/// # Behavior: Frozen (the `tool_filter` key) + Snapshot Read (the rest).
///
/// `tool_filter` is stamped into the session profile at session
/// creation; the live session never sees an edit to it, and the tools
/// picker is its live-session control. The timeout and output caps are
/// re-read from the layer's snapshot at each tool call, so an edit
/// applies at the next natural use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Which tools a session starts permitted or withheld.
    ///
    /// Frozen: stamped into the session profile at creation and never
    /// re-read — a live config edit does not move a session's filter,
    /// and the tools picker is its live-session control. A session
    /// created after the edit carries the new filter.
    ///
    /// ```toml
    /// # The former blocklist: withhold these two.
    /// [tools.tool_filter]
    /// mode = "deny"
    /// names = ["bash", "write"]
    ///
    /// # A bot that gets only what it is given, MCP included.
    /// [tools.tool_filter]
    /// mode = "allow"
    /// names = ["read", "grep", "mcp__github__*"]
    /// ```
    ///
    /// Replaces the former `disabled` key. A file still carrying `disabled`
    /// parses without it, so the filter reads as empty and permits
    /// everything — an existing blocklist silently stops applying. There is
    /// no migration; move the names under `tool_filter` yourself.
    ///
    /// Absent means a new session starts unrestricted. Present over no names
    /// is not absent: an allow list naming nothing starts it with no tools.
    /// The value a session carries is the file's value at that session's
    /// creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_filter: Option<NameFilter>,

    /// Fallback timeout for a tool that declares none of its own, in
    /// seconds. Default: 300.
    #[serde(default = "default_timeout_secs")]
    pub default_timeout_secs: u64,

    /// Cap on the lines a tool's output contributes to the transcript.
    /// `None` = no cap.
    #[serde(default)]
    pub max_output_lines: Option<usize>,

    /// Cap on the bytes a tool's output contributes to the transcript.
    /// `None` = no cap.
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

fn default_timeout_secs() -> u64 {
    300
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            tool_filter: None,
            default_timeout_secs: default_timeout_secs(),
            max_output_lines: None,
            max_output_bytes: None,
        }
    }
}

impl jinn_config::Configurable for ToolsConfig {
    const KEY: &'static str = "tools";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]
    use std::collections::BTreeSet;

    use jinn_config::Configurable;
    use jinn_core_types::{FilterMode, NameFilter};

    use super::ToolsConfig;

    /// The section's filter, which a test always expects to be present.
    fn filter(config: &ToolsConfig) -> &NameFilter {
        config.tool_filter.as_ref().expect("filter present")
    }

    #[rstest::rstest]
    fn a_denied_tool_is_read_from_the_filter() {
        // Given a table denying two tools.
        let table: toml::Table =
            toml::from_str("[tool_filter]\nmode = \"deny\"\nnames = [\"bash\", \"web-search\"]\n")
                .expect("test TOML parses");

        // When reading the section.
        let config = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then both are withheld and the third is not.
        assert!(!filter(&config).permits("bash"));
        assert!(!filter(&config).permits("web-search"));
        assert!(filter(&config).permits("read"));
    }

    /// The breaking rename's silent failure, pinned where it bites: a user's
    /// `jinn.toml` blocklist stops applying and every tool comes back. There
    /// is no migration, so this is the documented outcome rather than a bug —
    /// but it is worth a test so a future change to it is deliberate.
    #[rstest::rstest]
    fn a_stale_disabled_key_reads_as_no_filter() {
        // Given a file written before the rename, still carrying `disabled`.
        let table: toml::Table = toml::from_str("disabled = [\"bash\"]").expect("test TOML parses");

        // When reading the section.
        let config = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then the old key is unknown, so no filter is configured at all.
        assert!(config.tool_filter.is_none());
    }

    #[rstest::rstest]
    fn an_allow_filter_is_read_from_the_section() {
        // Given a table allowing one tool by glob.
        let table: toml::Table =
            toml::from_str("[tool_filter]\nmode = \"allow\"\nnames = [\"mcp__github__*\"]\n")
                .expect("test TOML parses");

        // When reading the section.
        let config = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then only a matching tool is permitted — MCP included, which is the
        // case a blocklist could not express.
        assert_eq!(filter(&config).mode, FilterMode::Allow);
        assert!(filter(&config).permits("mcp__github__create_pr"));
        assert!(!filter(&config).permits("bash"));
    }

    /// A hand-written allow list naming nothing is how a user says "no tools
    /// at all", and it has to survive the read as a filter that permits
    /// nothing rather than collapsing into an absent one.
    #[rstest::rstest]
    fn a_present_empty_allow_filter_permits_no_tool() {
        // Given a table configuring an allow list with no names.
        let table: toml::Table = toml::from_str("[tool_filter]\nmode = \"allow\"\nnames = []\n")
            .expect("test TOML parses");

        // When reading the section.
        let config = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then the filter is present and withholds every tool.
        assert_eq!(filter(&config).mode, FilterMode::Allow);
        assert!(!filter(&config).permits("bash"));
    }

    /// The round trip end to end, which is the only thing that actually
    /// proves the two cases survive a user's save. Serializing must write
    /// the present empty filter as a key, and reading it back must not
    /// collapse it into absence -- otherwise "no tools" silently becomes
    /// "all tools" on the first save.
    #[rstest::rstest]
    fn a_present_empty_allow_filter_survives_a_round_trip() {
        // Given a section whose allow filter names nothing.
        let config = ToolsConfig {
            tool_filter: Some(NameFilter {
                mode: FilterMode::Allow,
                names: BTreeSet::default(),
            }),
            ..ToolsConfig::default()
        };

        // When it is serialized and read back.
        let written = toml::to_string(&config).expect("serializes");
        let table: toml::Table = toml::from_str(&written).expect("written TOML parses");
        let read_back = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then the filter is still present, still allow mode, and still
        // withholds every tool.
        let restored = filter(&read_back);
        assert_eq!(restored.mode, FilterMode::Allow);
        assert!(restored.names.is_empty());
        assert!(!restored.permits("bash"));
        // And the written document carries the key, so the next save has
        // something to patch rather than a filter that vanished.
        assert!(
            written.contains("tool_filter"),
            "the filter key was dropped on save:\n{written}"
        );
    }

    /// The other half of the round trip: absence must stay absence, or
    /// every unconfigured session would grow a filter on its first save.
    #[rstest::rstest]
    fn an_absent_filter_survives_a_round_trip_as_absent() {
        // Given a section configuring no filter.
        let config = ToolsConfig::default();

        // When it is serialized and read back.
        let written = toml::to_string(&config).expect("serializes");
        let table: toml::Table = toml::from_str(&written).expect("written TOML parses");
        let read_back = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then no filter is configured, and the document never grew one.
        assert!(read_back.tool_filter.is_none());
        assert!(!written.contains("tool_filter"));
    }

    #[rstest::rstest]
    fn a_section_with_no_filter_configures_none() {
        // Given a table carrying only an unrelated key.
        let table: toml::Table =
            toml::from_str("default_timeout_secs = 60").expect("test TOML parses");

        // When reading the section.
        let config = ToolsConfig::from_table(&table).expect("section deserializes");

        // Then no filter is configured, exactly as before filters existed.
        assert!(config.tool_filter.is_none());
    }

    #[rstest::rstest]
    fn an_absent_filter_writes_no_key() {
        // Given a default section, which configures no filter.
        let config = ToolsConfig::default();

        // When serializing it.
        let table = toml::Value::try_from(&config).expect("serializes");

        // Then no filter key is written, so a user's file does not gain an
        // empty table on every save.
        assert!(table.get("tool_filter").is_none());
    }

    /// The mirror of the absent case: a filter the user did write must not
    /// be dropped on save, even with nothing in it.
    #[rstest::rstest]
    fn a_present_empty_allow_filter_still_writes_its_key() {
        // Given a section whose filter is an allow list naming nothing.
        let config = ToolsConfig {
            tool_filter: Some(NameFilter {
                mode: FilterMode::Allow,
                names: BTreeSet::default(),
            }),
            ..ToolsConfig::default()
        };

        // When serializing it.
        let table = toml::Value::try_from(&config).expect("serializes");

        // Then the key is present, carrying an empty name list.
        let written = table.get("tool_filter").expect("filter key written");
        assert_eq!(
            written
                .get("names")
                .and_then(toml::Value::as_array)
                .map(Vec::len),
            Some(0),
            "written: {written:?}"
        );
    }
}
