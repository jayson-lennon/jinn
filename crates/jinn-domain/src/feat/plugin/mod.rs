//! User-declared plugins — WASM components jinn hosts in-process.
//!
//! Plugins are declared in `jinn.toml` under `[[plugin]]`. Each entry names
//! a `.wasm` file (path relative to jinn's plugin dir or absolute) plus the
//! capability grants the plugin receives. The coordinator actor spawns one
//! in-process guest per entry at app start; see `feat/plugin_coordinator_actor`.

pub mod grant_serde;
pub mod install;
pub mod manifest;

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
    #[serde(default, with = "crate::feat::plugin::grant_serde")]
    pub grants: Vec<crate::feat::plugin::PluginPathGrant>,
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

use jinn_theme::Theme;
use std::collections::BTreeMap;

use crate::feat::plugin_coordinator_actor::protocol::PluginPhase;

/// The plugin contribution cache — data pushed by plugins, held for
/// synchronous consumers (pickers, renderer).
///
/// Written only by the plugin coordinator actor (via [`PluginsCap`]
/// ([`crate::common::tcaps::plugins`]); read by anyone. Contributions are
/// push-only: a stale cache means a dead plugin, never a blocked
/// consumer.
#[derive(Debug, Default)]
pub struct PluginContributions {
    /// Latest known phase per plugin name.
    phases: BTreeMap<String, PluginPhase>,
}

impl PluginContributions {
    /// Records a plugin's latest phase.
    pub fn set_phase(&mut self, name: String, phase: PluginPhase) {
        self.phases.insert(name, phase);
    }

    /// The latest known phase for a plugin.
    #[must_use]
    pub fn phase(&self, name: &str) -> Option<PluginPhase> {
        self.phases.get(name).copied()
    }

    /// All known plugins with their latest phase, ordered by name
    /// (BTreeMap iteration order).
    pub fn phases(&self) -> impl Iterator<Item = (&str, PluginPhase)> {
        self.phases.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// One loaded plugin ready for display in the plugin picker.
///
/// A read-only row: the plugin's name plus its lifecycle phase,
/// snapshotted from [`PluginContributions::phases`] at picker-open time.
#[derive(Debug, Clone)]
pub struct PluginPickerEntry {
    /// The configured plugin name (the `jinn.toml` `[plugin.<name>]` key).
    pub name: String,
    /// The plugin's latest lifecycle phase.
    pub phase: PluginPhase,
    /// Theme for styling.
    pub theme: Theme,
}

impl PluginPickerEntry {
    /// Builds an entry from a plugin name and its phase.
    #[must_use]
    pub fn new(name: String, phase: PluginPhase, theme: Theme) -> Self {
        Self { name, phase, theme }
    }
}

impl jinn_selection_widget::TreeItem for PluginPickerEntry {
    fn id(&self) -> &str {
        &self.name
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, is_selected: bool) -> ratatui::text::Line<'static> {
        crate::feat::picker::plugin_spec::plugin_row(
            self,
            &jinn_picker::RowCtx::flat(is_selected, &[]),
        )
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[std::ops::Range<usize>],
    ) -> ratatui::text::Line<'static> {
        crate::feat::picker::plugin_spec::plugin_row(
            self,
            &jinn_picker::RowCtx::flat(is_selected, match_indices),
        )
    }
}

/// Manifest path grant: a template string plus write intent.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    fn row_text(entry: &PluginPickerEntry) -> String {
        let line = crate::feat::picker::plugin_spec::plugin_row(
            entry,
            &jinn_picker::RowCtx::flat(false, &[]),
        );
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[rstest::rstest]
    #[case::starting(PluginPhase::Starting, "Starting")]
    #[case::running(PluginPhase::Running, "Running")]
    #[case::dead(PluginPhase::Dead, "Dead")]
    #[case::unresponsive(PluginPhase::Unresponsive, "Unresponsive")]
    #[case::done(PluginPhase::Done, "Done")]
    fn render_row_shows_name_and_phase_label(#[case] phase: PluginPhase, #[case] label: &str) {
        // Given a plugin entry with this phase.
        let entry = PluginPickerEntry::new(
            "stall-watchdog".to_owned(),
            phase,
            crate::feat::theme::default_theme(),
        );

        // When rendering the row.
        let text = row_text(&entry);

        // Then the name and phase label both appear.
        assert!(text.contains("stall-watchdog"), "row shows name: {text}");
        assert!(text.contains(label), "row shows phase {label}: {text}");
    }

    #[rstest::rstest]
    fn phases_iterator_returns_name_sorted_entries() {
        // Given a cache populated out of name order.
        let mut cache = PluginContributions::default();
        cache.set_phase("zeta".to_owned(), PluginPhase::Running);
        cache.set_phase("alpha".to_owned(), PluginPhase::Dead);

        // When iterating phases.
        let names: Vec<&str> = cache.phases().map(|(name, _)| name).collect();

        // Then the names are in sorted order.
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[rstest::rstest]
    fn phases_iterator_empty_cache_yields_nothing() {
        // Given an empty contribution cache.
        let cache = PluginContributions::default();

        // When iterating phases.
        // Then nothing is yielded.
        assert_eq!(cache.phases().count(), 0);
    }
}
