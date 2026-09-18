//! CWD selector configuration schema — the `jinn.toml` `[cwd_selector]`
//! section.
//!
//! Pure serde data; the in-app cwd input popup lives in the `jinn-cwd`
//! slice and the TUI suspend flow that runs the selector command stays in
//! the kernel — both import the shape from here.

use serde::{Deserialize, Serialize};

/// CWD selector configuration.
///
/// Serialized as `[cwd_selector]` in `jinn.toml`.
/// Controls the shell command used to select a new working directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CwdSelectorConfig {
    /// Shell command template. `{path}` is replaced with the search root.
    /// Default: `find -L {path} -type d 2>/dev/null | fzf --no-multi`
    #[serde(default = "CwdSelectorConfig::default_command")]
    pub command: String,
}

impl CwdSelectorConfig {
    /// Returns the default picker command.
    fn default_command() -> String {
        "find -L {path} -type d 2>/dev/null | fzf --no-multi".to_owned()
    }
}

impl Default for CwdSelectorConfig {
    fn default() -> Self {
        Self {
            command: Self::default_command(),
        }
    }
}
