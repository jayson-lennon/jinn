//! Display-only UI elements.

pub mod chat_log;
pub mod frontend_state;
pub mod picker_states;
pub mod sidebar;
pub mod status_hint;
pub mod vertical_minimap;

use serde::{Deserialize, Serialize};

/// Default maximum token count for minimap color banding.
const DEFAULT_MINIMAP_MAX_TOKENS: u32 = 2000;

/// Minimap configuration.
///
/// Serialized as `[minimap]` in `jinn.toml`.
/// Controls the token-count range used for the vertical minimap color gradient.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinimapConfig {
    /// Maximum token count for the top band of the minimap gradient.
    /// Entries with more tokens than this get the last band color.
    /// Default: 2000.
    #[serde(default = "default_minimap_max_tokens")]
    pub max_tokens: u32,
}

fn default_minimap_max_tokens() -> u32 {
    DEFAULT_MINIMAP_MAX_TOKENS
}

impl Default for MinimapConfig {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MINIMAP_MAX_TOKENS,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use tempfile::TempDir;

    use super::MinimapConfig;
    use crate::common::app_info::PREFS_FILE_NAME;
    use crate::feat::preferences_actor::user_preferences::load_preferences_from;

    #[rstest::rstest]
    fn default_minimap_config_has_positive_token_bound() {
        // Given default minimap config.
        let config = MinimapConfig::default();
        // Then the band boundary is a positive token count (not pinned to a
        // specific value — that's the Default impl's choice, not a contract).
        assert!(config.max_tokens > 0);
    }

    #[rstest::rstest]
    fn load_parses_minimap_config() {
        // Given a TOML file with a minimap section.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, "[minimap]\nmax_tokens = 5000\n").expect("write");
        // When loading.
        let prefs = load_preferences_from(&path).expect("load");
        // Then minimap config is parsed.
        assert_eq!(prefs.minimap.max_tokens, 5000);
    }

    #[rstest::rstest]
    fn load_without_minimap_section_uses_defaults() {
        // Given a TOML file without a minimap section.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"last_model = "ollama/llama3"
"#,
        )
        .expect("write");
        // When loading.
        let prefs = load_preferences_from(&path).expect("load");
        // Then minimap uses defaults.
        assert_eq!(prefs.minimap, MinimapConfig::default());
    }
}
