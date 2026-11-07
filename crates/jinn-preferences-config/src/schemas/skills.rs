//! The `[skills]` section — the skills slice's own configuration.

use jinn_core_types::NameFilter;
use serde::{Deserialize, Serialize};

/// The `[skills]` section.
///
/// # Behavior: Frozen.
///
/// `skill_filter` is stamped into the session profile at session
/// creation; the live session never sees an edit to it, and the skills
/// picker is its live-session control. A session created after the edit
/// carries the new filter.
///
/// The filter has the same mode-plus-glob shape as `[tools]`, and covers the
/// same breaking rename: a file still carrying `disabled` parses without it
/// and every skill comes back. See [`jinn_core_types::NameFilter`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsConfig {
    /// Which skills a session may load, stamped at its creation.
    ///
    /// Frozen: copied into the session profile at creation and never
    /// re-read — a live config edit does not move a session's filter,
    /// and the skills picker is its live-session control. A session
    /// created after the edit carries the new filter.
    ///
    /// Absent means a new session starts unrestricted. Present over no names
    /// is not absent: an allow list naming nothing starts it with no skills.
    /// The value a session carries is the file's value at that session's
    /// creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_filter: Option<NameFilter>,
}

impl jinn_config::Configurable for SkillsConfig {
    const KEY: &'static str = "skills";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]
    use std::collections::BTreeSet;

    use jinn_config::Configurable;
    use jinn_core_types::{FilterMode, NameFilter};

    use super::SkillsConfig;

    /// The section's filter, which a test always expects to be present.
    fn filter(config: &SkillsConfig) -> &NameFilter {
        config.skill_filter.as_ref().expect("filter present")
    }

    #[rstest::rstest]
    fn a_denied_skill_is_read_from_the_filter() {
        // Given a table denying one skill.
        let table: toml::Table = toml::from_str(
            r#"
            [skill_filter]
            mode = "deny"
            names = ["web-search"]
        "#,
        )
        .expect("test TOML parses");

        // When reading the section.
        let config = SkillsConfig::from_table(&table).expect("section deserializes");

        // Then the skill reads as withheld.
        assert!(!filter(&config).permits("web-search"));
    }

    #[rstest::rstest]
    fn an_allow_filter_is_read_from_the_section() {
        // Given a table allowing two skills by glob.
        let table: toml::Table = toml::from_str(
            r#"
            [skill_filter]
            mode = "allow"
            names = ["web-*", "dataviz"]
        "#,
        )
        .expect("test TOML parses");

        // When reading the section.
        let config = SkillsConfig::from_table(&table).expect("section deserializes");

        // Then the mode survives, and only a listed skill is permitted.
        assert_eq!(filter(&config).mode, FilterMode::Allow);
        assert!(filter(&config).permits("web-search"));
        assert!(!filter(&config).permits("phased-task-loop"));
    }

    #[rstest::rstest]
    fn a_section_with_no_filter_configures_none() {
        // Given a table carrying only an unrelated key.
        let table: toml::Table =
            toml::from_str("default_timeout_secs = 60").expect("test TOML parses");

        // When reading the section.
        let config = SkillsConfig::from_table(&table).expect("section deserializes");

        // Then no filter is configured, exactly as before filters existed.
        assert!(config.skill_filter.is_none());
    }

    #[rstest::rstest]
    fn a_stale_disabled_key_reads_as_no_filter() {
        // Given a file written before the rename, still carrying `disabled`.
        let table: toml::Table =
            toml::from_str("disabled = [\"web-search\"]").expect("test TOML parses");

        // When reading the section.
        let config = SkillsConfig::from_table(&table).expect("section deserializes");

        // Then the old key is unknown, so the skill comes back. This is the
        // accepted silent failure of the breaking rename: an existing
        // blocklist stops applying without anything reporting it.
        assert!(config.skill_filter.is_none());
    }

    #[rstest::rstest]
    fn an_absent_filter_is_omitted_when_serialized() {
        // Given a default section, which configures no filter.
        let config = SkillsConfig::default();

        // When serializing it.
        let table = toml::Value::try_from(&config).expect("serializes");

        // Then no filter key is written, so a user's file does not gain an
        // empty table on every save.
        assert!(table.get("skill_filter").is_none());
    }

    /// A present filter is present on save even when it names nothing: an
    /// allow list over no skills is the only way to say "no skills at all".
    #[rstest::rstest]
    fn a_present_empty_allow_filter_still_writes_its_key() {
        // Given a section whose filter is an allow list naming nothing.
        let config = SkillsConfig {
            skill_filter: Some(NameFilter {
                mode: FilterMode::Allow,
                names: BTreeSet::default(),
            }),
        };

        // When serializing it.
        let table = toml::Value::try_from(&config).expect("serializes");

        // Then the key is present, carrying an empty name list.
        let written = table.get("skill_filter").expect("filter key written");
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
