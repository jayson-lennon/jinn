//! Minimap configuration schema — the `jinn.toml` `[minimap]` section.
//!
//! Pure serde data; the minimap rendering stays in the kernel's UI feature
//! and imports the shape from here.

use serde::{Deserialize, Serialize};

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
