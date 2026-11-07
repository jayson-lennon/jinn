//! Session-creation seed derived from the user's configuration.

use std::collections::BTreeSet;

use jinn_core_types::NameFilter;

/// Per-session defaults derived from the user's configuration at session
/// creation.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSeed {
    /// Which tools the session starts permitted or withheld.
    ///
    /// `None` configures no filter: the session starts unrestricted, which
    /// is what a `[tools]` section that says nothing about the filter means.
    /// `Some` is a filter the user wrote, including an allow list naming
    /// nothing — which starts the session with no tools.
    pub tool_filter: Option<NameFilter>,
    /// Which skills the session starts permitted or withheld, as above.
    pub skill_filter: Option<NameFilter>,
    /// MCP servers to start the session with enabled.
    pub enabled_mcp: BTreeSet<String>,
}

impl SessionSeed {
    /// Derives a new-session seed by reading three sections from the
    /// configuration layer.
    ///
    /// The three reads stamp **Frozen** values: tool filter, skill
    /// filter, and MCP enablement are copied into the session profile
    /// here, at creation, and no reload event re-seeds a live session —
    /// the corresponding pickers are those controls. A session created
    /// after an edit carries the new values.
    ///
    /// Three reads rather than one aggregate read: the seed's inputs are
    /// owned by three different slices, and a kernel-side struct
    /// collecting them is what this work is removing.
    #[must_use]
    pub fn from_config(config: &jinn_config::ConfigLayer) -> Self {
        let tools = config.read::<jinn_preferences_config::schemas::ToolsConfig>();
        let skills = config.read::<jinn_preferences_config::schemas::SkillsConfig>();
        let mcp = config.read::<jinn_preferences_config::schemas::mcp::McpServersConfig>();
        Self {
            tool_filter: tools.tool_filter.clone(),
            skill_filter: skills.skill_filter.clone(),
            enabled_mcp: mcp
                .iter()
                .filter(|(_, server)| server.auto_enable)
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }

    /// Returns whether the seed enables at least one MCP server.
    #[must_use]
    pub fn has_auto_enabled_mcp(&self) -> bool {
        !self.enabled_mcp.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;
    use std::sync::Arc;

    use jinn_preferences_config::schemas::mcp::McpServersConfig;
    use jinn_preferences_config::schemas::{SkillsConfig, ToolsConfig};

    /// A layer over `document`, for exercising the same read path
    /// production uses.
    fn layer(document: &str) -> jinn_config::ConfigLayer {
        let parsed = document.parse().expect("test TOML parses");
        jinn_config::ConfigLayer::load(Arc::new(jinn_config::InMemoryConfigStorage::new(parsed)))
            .expect("layer loads")
    }

    fn empty_layer() -> jinn_config::ConfigLayer {
        layer("")
    }

    /// Whether the seed's filter permits a name, with an absent filter
    /// inheriting — which at a gate is what permits everything.
    fn permits(filter: Option<&NameFilter>, name: &str) -> bool {
        filter.is_none_or(|filter| filter.permits(name))
    }

    #[rstest::rstest]
    fn an_unconfigured_document_leaves_everything_enabled() {
        // Given a layer with no configured sections.
        let config = empty_layer();

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then no tool or skill is withheld and no server is auto-enabled.
        assert!(permits(seed.tool_filter.as_ref(), "bash"));
        assert!(permits(seed.skill_filter.as_ref(), "any"));
        assert!(seed.enabled_mcp.is_empty());
    }

    #[rstest::rstest]
    fn the_tool_filter_comes_from_the_tools_section() {
        // Given a layer denying two tools by glob and literal.
        let config =
            layer("[tools.tool_filter]\nmode = \"deny\"\nnames = [\"bash\", \"mcp__github__*\"]\n");

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then exactly those are withheld, glob included.
        assert!(!permits(seed.tool_filter.as_ref(), "bash"));
        assert!(!permits(
            seed.tool_filter.as_ref(),
            "mcp__github__create_pr"
        ));
        assert!(permits(seed.tool_filter.as_ref(), "read"));
    }

    #[rstest::rstest]
    fn the_skill_filter_comes_from_the_skills_section() {
        // Given a layer denying one skill.
        let config =
            layer("[skills.skill_filter]\nmode = \"deny\"\nnames = [\"phased-task-loop\"]\n");

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then exactly that skill is withheld.
        assert!(!permits(seed.skill_filter.as_ref(), "phased-task-loop"));
        assert!(permits(seed.skill_filter.as_ref(), "micro-task-loop"));
    }

    #[rstest::rstest]
    fn an_allow_filter_seeds_as_absolute() {
        // Given a layer allowing only two tools.
        let config = layer("[tools.tool_filter]\nmode = \"allow\"\nnames = [\"read\", \"grep\"]\n");

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then every unlisted tool is withheld, MCP included — the case a
        // blocklist could not express.
        assert!(permits(seed.tool_filter.as_ref(), "read"));
        assert!(!permits(seed.tool_filter.as_ref(), "bash"));
        assert!(!permits(
            seed.tool_filter.as_ref(),
            "mcp__github__create_pr"
        ));
    }

    #[rstest::rstest]
    fn only_auto_enabling_servers_are_seeded_as_enabled() {
        // Given two configured servers, one with auto_enable set.
        let config = layer(
            "[mcp.alpha]\nauto_enable = true\nurl = \"http://localhost:1\"\n\
             [mcp.beta]\nauto_enable = false\nurl = \"http://localhost:2\"\n",
        );

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then only the auto-enabling one is seeded.
        assert_eq!(seed.enabled_mcp.iter().collect::<Vec<_>>(), vec!["alpha"]);
    }

    /// The three sections belong to three different slices; the seed is
    /// the one place that reads all three, and it must not silently
    /// collapse into reading one of them.
    #[rstest::rstest]
    fn the_seed_reads_all_three_sections_independently() {
        // Given a layer configuring each section differently.
        let config = layer(
            "[tools.tool_filter]\nmode = \"deny\"\nnames = [\"bash\"]\n\
             [skills.skill_filter]\nmode = \"deny\"\nnames = [\"micro-task-loop\"]\n\
             [mcp.gamma]\nauto_enable = true\nurl = \"http://localhost:3\"\n",
        );

        // When deriving the seed.
        let seed = SessionSeed::from_config(&config);

        // Then each section's value lands in its own field.
        assert!(!permits(seed.tool_filter.as_ref(), "bash"));
        assert!(!permits(seed.skill_filter.as_ref(), "micro-task-loop"));
        assert!(seed.enabled_mcp.contains("gamma"));
        assert!(seed.has_auto_enabled_mcp());
    }

    /// A `put` through the layer must round-trip a server table, since
    /// the seed is derived from exactly what `put` wrote.
    #[rstest::rstest]
    fn a_written_server_section_reads_back_identically() {
        // Given a server written through the layer.
        let config = layer("[mcp.delta]\nauto_enable = true\n");
        let before = config.read::<McpServersConfig>();
        config
            .put::<McpServersConfig>(&before)
            .expect("layer writes the mcp section");

        // When it is read again.
        let after = config.read::<McpServersConfig>();

        // Then nothing changed.
        assert_eq!(after, before);
        assert!(after.contains_key("delta"));
    }

    #[rstest::rstest]
    fn the_tools_and_skills_defaults_configure_no_filter() {
        // Given no configuration for the tools or skills sections.
        let tools = ToolsConfig::default();
        let skills = SkillsConfig::default();

        // When reading each section's filter.
        // Then both are absent, so a new session starts unrestricted.
        assert!(tools.tool_filter.is_none());
        assert!(skills.skill_filter.is_none());
    }

    /// A config save writes through the patcher, so an absent filter must
    /// contribute no key at all — otherwise every save of an unconfigured file
    /// would grow an empty filter table into it.
    #[rstest::rstest]
    fn a_written_absent_filter_writes_no_key() {
        // Given a layer over a file that configures no filter.
        let config = layer("[tools]\ndefault_timeout_secs = 60\n");

        // When the section is saved back.
        let section = config.read::<ToolsConfig>();
        config
            .put::<ToolsConfig>(&section)
            .expect("layer writes the tools section");

        // Then the document carries no filter key.
        assert!(
            !config.document_text().contains("tool_filter"),
            "an unconfigured filter grew a key:\n{}",
            config.document_text()
        );
    }

    #[rstest::rstest]
    fn a_written_tool_filter_reads_back_identically() {
        // Given a layer whose filter is written through it.
        let config = layer("[tools]\n");
        let before = config.read::<ToolsConfig>();
        config
            .put::<ToolsConfig>(&before)
            .expect("layer writes tools");

        // When it is read again.
        let after = config.read::<ToolsConfig>();

        // Then the filter survived the round trip rather than being dropped
        // as a key the patcher did not recognize.
        assert_eq!(after, before);
    }
}
