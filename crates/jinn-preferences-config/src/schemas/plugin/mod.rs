//! Plugin configuration schema — the `jinn.toml` `[plugin.<name>]` entries.
//!
//! Pure serde data (the config crate has no dependency on the plugin
//! runner); the coordinator translates [`PluginPathGrant`] to
//! `jinn_plugin::PathGrant` at spawn time.

pub mod grant_serde;

use serde::{Deserialize, Serialize};

/// One configured plugin.
///
/// Declared in `jinn.toml` under `[plugin.<name>]` — the table name IS the
/// plugin's identity (contribution namespace + default scratch-dir
/// selector); there is no `name` field to drift out of sync with the key.
/// Grants are path templates with an optional `:w` suffix (`<config_dir>/themes:w`);
/// nothing is granted implicitly — a plugin wanting its own scratch dir
/// declares `"<plugin_data_dir>:w"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Path to the plugin's `.wasm` component. Relative paths resolve
    /// against jinn's plugin directory (`<data_dir>/plugins/`).
    pub wasm: String,
    /// Directory paths the plugin may access, as templates (e.g.
    /// `<config_dir>/themes`, `<data_dir>/notes:w`). `:w` marks a grant
    /// writable. See [`PluginPathGrant`].
    #[serde(default, with = "grant_serde")]
    pub grants: Vec<PluginPathGrant>,
    /// Whether the plugin may make network requests via `wasi:http`.
    #[serde(default)]
    pub http: bool,
    /// Free-form plugin config passed through to the guest.
    #[serde(default)]
    pub config: Option<toml::Value>,
    /// Set to `false` to disable this plugin without deleting its entry.
    /// The coordinator skips disabled entries entirely.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// The default for [`PluginConfig::enabled`] — `true` unless the user opts
/// out. A separate function (not `#[serde(default)]` on the field) because
/// `bool::default()` is `false`.
#[must_use]
pub fn default_true() -> bool {
    true
}

/// One path grant for a plugin.
///
/// Defined here (not re-exported from `jinn-plugin`) so `jinn.toml` parsing
/// has no dependency on the runner crate; the coordinator translates to
/// `jinn_plugin::PathGrant` at spawn time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPathGrant {
    /// Path template (e.g. `<config_dir>/themes`).
    pub path: String,
    /// Grant write access in addition to read.
    #[serde(default)]
    pub writable: bool,
}
