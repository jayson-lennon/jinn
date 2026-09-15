//! Picker kind - identifies which picker is currently active.
//!
//! A single set of `Picker*` commands, `Mode::Picker`, per-picker `Scope`,
//! and keymap bindings serve all pickers. [`PickerKind`] determines which
//! `SelectionState` (from `jinn-selection-widget`) the commands
//! operate on.

use serde::{Deserialize, Serialize};

/// Which picker is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickerKind {
    /// Provider/model picker.
    Provider,
    /// Session browser picker.
    Session,
    /// Persona picker.
    Persona,
    /// Theme picker.
    Theme,
    /// Session lifecycle picker - select a lifecycle recipe for new session creation.
    SessionLifecycle,
    /// Reasoning effort picker - select reasoning effort for reasoning-capable models.
    ReasoningEffort,
    /// Tool picker - toggle which tools are enabled for the session.
    Tool,
    /// Skill picker - toggle which skills are enabled for the session.
    Skill,
    /// Task list browser - read-only zoom view of the active session's task list.
    TaskList,
    /// Project picker - curated project directories; create a new session rooted
    /// at the highlighted dir with `<enter>` (or `<c-enter>` to also pick a lifecycle).
    Project,
    /// MCP server picker - toggle which MCP servers are enabled for the session.
    McpServer,
    /// Plugin picker - read-only list of loaded plugins with their lifecycle phase.
    Plugin,
    /// OpenRouter endpoint picker - pin a specific routing upstream for
    /// prefix-cache affinity on an OpenRouter-served Single model.
    Endpoint,
}

impl PickerKind {
    /// Every variant, in declaration order.
    ///
    /// Lets exhaustive drift tests (e.g. scope-binding coverage) iterate
    /// all kinds without a strum dependency.
    pub const ALL: [Self; 13] = [
        Self::Provider,
        Self::Session,
        Self::Persona,
        Self::Theme,
        Self::SessionLifecycle,
        Self::ReasoningEffort,
        Self::Tool,
        Self::Skill,
        Self::TaskList,
        Self::Project,
        Self::McpServer,
        Self::Plugin,
        // Update this count when adding a variant; `Endpoint` is last.
        Self::Endpoint,
    ];
}

impl std::fmt::Display for PickerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider => write!(f, "models"),
            Self::Session => write!(f, "sessions"),
            Self::Persona => write!(f, "personas"),
            Self::Theme => write!(f, "themes"),

            Self::SessionLifecycle => write!(f, "session-lifecycle"),

            Self::ReasoningEffort => write!(f, "reasoning effort"),

            Self::Tool => write!(f, "tools"),
            Self::Skill => write!(f, "skills"),
            Self::TaskList => write!(f, "task list"),
            Self::Project => write!(f, "projects"),
            Self::McpServer => write!(f, "mcp servers"),
            Self::Plugin => write!(f, "plugins"),

            Self::Endpoint => write!(f, "endpoints"),
        }
    }
}

impl PickerKind {
    /// Footer rows this picker kind draws at the bottom of its popup. This is
    /// the authoritative count consumed by both the render sites (which build
    /// the footer lines) and the geometry measurement (which must reserve the
    /// same number of rows). Keeping the two in sync here prevents the picker
    /// viewport from drifting from what is actually drawn.
    ///
    /// - `Provider`: two footer lines (refresh status + alloy mode).
    /// - All others: exactly one footer line.
    #[must_use]
    pub const fn footer_rows(self) -> u16 {
        match self {
            Self::Provider => 2,
            // Each single-footer kind is listed explicitly so that adding a
            // new variant forces a deliberate decision here rather than
            // silently defaulting to a wrong count.
            Self::Session
            | Self::Persona
            | Self::Theme
            | Self::SessionLifecycle
            | Self::ReasoningEffort
            | Self::Tool
            | Self::Skill
            | Self::TaskList
            | Self::Project
            | Self::McpServer
            | Self::Plugin
            | Self::Endpoint => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    #[test]
    fn reasoning_effort_displays_as_reasoning_effort() {
        // Given the ReasoningEffort picker kind.
        // When displaying.
        // Then it renders as 'reasoning effort'.
        assert_eq!(PickerKind::ReasoningEffort.to_string(), "reasoning effort");
    }

    #[rstest::rstest]
    #[test]
    fn endpoint_displays_as_endpoints() {
        // Given the Endpoint picker kind.
        // When displaying.
        // Then it renders as 'endpoints'.
        assert_eq!(PickerKind::Endpoint.to_string(), "endpoints");
    }

    #[rstest::rstest]
    #[test]
    fn plugin_displays_as_plugins() {
        // Given the Plugin picker kind.
        // When displaying.
        // Then it renders as 'plugins'.
        assert_eq!(PickerKind::Plugin.to_string(), "plugins");
    }
}
