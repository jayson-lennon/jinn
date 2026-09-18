//! Keymap scopes for context-sensitive key handling.
//!
//! The scope determines which set of keybindings is active.
//! Each sidebar section has its own scope so section-specific keys
//! (like `r` for rename vs pin-relative) are unambiguous.

/// The current keymap context.
///
/// Controls which keybindings are active. Set via
/// [`ratatui_which_key::WhichKeyState::set_scope`].
///
/// Static variants are the composition-owned scopes. A slice's dynamic
/// scope ([`Scope::Dynamic`]) carries its identity as data, so slices
/// never edit this enum; their keymap bindings are generated from
/// registered route rows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Normal mode - navigation and commands.
    Normal,
    /// Sidebar - Persona section.

    /// Sidebar - Pins section.

    /// Sidebar - Sessions section.

    /// Sidebar - Task list section.

    /// Sidebar - MCP servers section.

    /// Picker - Provider/model selection.
    PickerProvider,
    /// Picker - Session browser.
    PickerSession,
    /// Picker - Persona selection.
    PickerPersona,
    /// Picker - Theme selection.
    PickerTheme,
    /// Picker - Session lifecycle recipe selection.
    PickerLifecycle,
    /// Picker - Reasoning effort selection.
    PickerReasoningEffort,
    /// Picker - OpenRouter routing endpoint selection.
    PickerEndpoint,
    /// Picker - Tool toggle selection.
    PickerTool,
    /// Picker - Skill toggle selection.
    PickerSkill,
    /// Picker - Read-only task list browser.
    PickerTaskList,
    /// Picker - Curated project directory selection.
    PickerProject,
    /// Picker - MCP server toggle selection.
    PickerMcpServer,
    /// Picker - Read-only plugin list.
    PickerPlugin,
    /// Input mode - typing into the input buffer.
    Input,
    /// Arg input mode - typing positional args for a lifecycle command.
    ArgInput,
    /// Token budget input mode - typing a numeric budget value.
    TokenBudgetInput,
    /// Rename session input mode - editing a session title.
    RenameSessionInput,
    /// CWD input mode - typing a directory path.

    /// A dynamically-registered slice's scope.
    ///
    /// Derives `Ord` on the inner string-based id (which-key stores
    /// catch-all handlers in a `BTreeMap<S, _>`), so the derived
    /// ordering is required, not hand-rolled.
    Dynamic(jinn_slices::SliceScopeId),
    /// Pruner accumulation threshold input mode - numeric input for the KV-cache gate.
    PrunerAccumulationInput,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::PickerProvider => write!(f, "Picker(provider)"),
            Self::PickerSession => write!(f, "Picker(session)"),
            Self::PickerPersona => write!(f, "Picker(persona)"),
            Self::PickerTheme => write!(f, "Picker(theme)"),
            Self::PickerLifecycle => write!(f, "Picker(lifecycle)"),

            Self::PickerReasoningEffort => write!(f, "Picker(reasoning-effort)"),
            Self::PickerEndpoint => write!(f, "Picker(endpoint)"),
            Self::PickerTool => write!(f, "Picker(tool)"),
            Self::PickerSkill => write!(f, "Picker(skill)"),
            Self::PickerTaskList => write!(f, "Picker(task-list)"),
            Self::PickerProject => write!(f, "Picker(project)"),
            Self::PickerMcpServer => write!(f, "Picker(mcp-server)"),
            Self::PickerPlugin => write!(f, "Picker(plugin)"),
            Self::Input => write!(f, "Input"),
            Self::Dynamic(id) => write!(f, "dynamic:{id}"),
            Self::ArgInput => write!(f, "ArgInput"),
            Self::TokenBudgetInput => write!(f, "TokenBudgetInput"),
            Self::RenameSessionInput => write!(f, "RenameSessionInput"),
            Self::PrunerAccumulationInput => write!(f, "PrunerAccumulationInput"),
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Dynamic scopes parse first: `dynamic:<slice>:<name>` is the
        // Display inverse and must round-trip.
        if let Some(rest) = s.strip_prefix("dynamic:") {
            let id = rest.parse::<jinn_slices::SliceScopeId>()?;
            return Ok(Self::Dynamic(id));
        }
        match s {
            "Normal" => Ok(Self::Normal),
            "Picker(provider)" => Ok(Self::PickerProvider),
            "Picker(session)" => Ok(Self::PickerSession),
            "Picker(persona)" => Ok(Self::PickerPersona),
            "Picker(theme)" => Ok(Self::PickerTheme),
            "Picker(lifecycle)" => Ok(Self::PickerLifecycle),

            "Picker(reasoning-effort)" => Ok(Self::PickerReasoningEffort),
            "Picker(endpoint)" => Ok(Self::PickerEndpoint),
            "Picker(tool)" => Ok(Self::PickerTool),
            "Picker(skill)" => Ok(Self::PickerSkill),
            "Picker(task-list)" => Ok(Self::PickerTaskList),
            "Picker(project)" => Ok(Self::PickerProject),
            "Picker(mcp-server)" => Ok(Self::PickerMcpServer),
            "Picker(plugin)" => Ok(Self::PickerPlugin),
            "Input" => Ok(Self::Input),
            "ArgInput" => Ok(Self::ArgInput),
            "TokenBudgetInput" => Ok(Self::TokenBudgetInput),
            "RenameSessionInput" => Ok(Self::RenameSessionInput),
            "PrunerAccumulationInput" => Ok(Self::PrunerAccumulationInput),

            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::Scope;
    use std::str::FromStr;

    #[rstest::rstest]
    fn picker_reasoning_effort_scope_round_trips() {
        // Given the PickerReasoningEffort scope variant.
        // When formatting then parsing back.
        // Then the round-trip preserves the variant.
        let s = Scope::PickerReasoningEffort.to_string();
        assert_eq!(s, "Picker(reasoning-effort)");
        assert_eq!(
            Scope::from_str(&s),
            Ok(Scope::PickerReasoningEffort),
            "Display/FromStr should round-trip"
        );
    }

    #[rstest::rstest]
    #[test]
    fn dynamic_scope_round_trips() {
        // Given a dynamic scope carrying a slice scope id.
        let scope = Scope::Dynamic(jinn_slices::SliceScopeId::new("quake-bar", "open"));

        // When formatting then parsing back.
        let s = scope.to_string();
        // Then the display form is the `dynamic:` prefixed key.
        assert_eq!(s, "dynamic:quake-bar:open");
        assert_eq!(
            Scope::from_str(&s),
            Ok(scope),
            "Display/FromStr should round-trip"
        );
    }

    #[rstest::rstest]
    #[test]
    fn plugin_picker_scope_round_trips() {
        // Given the PickerPlugin scope variant.
        // When formatting then parsing back.
        // Then the round-trip preserves the variant.
        let s = Scope::PickerPlugin.to_string();
        assert_eq!(s, "Picker(plugin)");
        assert_eq!(
            Scope::from_str(&s),
            Ok(Scope::PickerPlugin),
            "Display/FromStr should round-trip"
        );
    }
}
