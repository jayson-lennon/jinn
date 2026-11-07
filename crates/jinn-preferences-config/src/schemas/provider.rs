//! The `[provider]` umbrella — provider-side tool configuration.
//!
//! Currently one section: the OpenRouter web-search tool definition the
//! tools slice publishes as `openrouter:web_search`.

use serde::{Deserialize, Serialize};

/// The `[provider.web_search]` section — how the OpenRouter web-search
/// tool is offered to the model.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each turn's tool offering, so an edit applies at the next
/// turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Search engine: "auto", "native", "exa", "firecrawl", or
    /// "parallel". Default: "exa".
    #[serde(default = "default_engine")]
    pub engine: Option<String>,

    /// Maximum results per search call (1-25). `None` = OpenRouter
    /// default (5).
    #[serde(default)]
    pub max_results: Option<u32>,

    /// Maximum total results across all searches in one request.
    #[serde(default)]
    pub max_total_results: Option<u32>,

    /// How much context to retrieve: "low", "medium", or "high".
    /// `None` = OpenRouter picks adaptively.
    #[serde(default)]
    pub search_context_size: Option<String>,

    /// Only return results from these domains.
    #[serde(default)]
    pub allowed_domains: Option<Vec<String>>,

    /// Exclude results from these domains.
    #[serde(default)]
    pub excluded_domains: Option<Vec<String>>,
}

// Named for its serde `default = "..."` role, which must return the
// shape the field declares.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a serde default fn returns the field's declared Option shape"
)]
fn default_engine() -> Option<String> {
    Some("exa".to_owned())
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            engine: default_engine(),
            max_results: None,
            max_total_results: None,
            search_context_size: None,
            allowed_domains: None,
            excluded_domains: None,
        }
    }
}

impl jinn_config::Configurable for WebSearchConfig {
    const KEY: &'static str = "provider.web_search";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_config::Configurable;

    use super::WebSearchConfig;

    #[rstest::rstest]
    fn a_partial_section_keeps_the_engine_default() {
        // Given a table that sets only one key.
        let table: toml::Table = toml::from_str("max_results = 5").expect("test TOML parses");

        // When reading the section.
        let config = WebSearchConfig::from_table(&table).expect("section deserializes");

        // Then the omitted key keeps its default rather than going None.
        assert_eq!(config.max_results, Some(5));
        // And the defaulted key survives.
        assert_eq!(config.engine.as_deref(), Some("exa"));
    }
}
