//! Slash command definitions and registry.
//!
//! Defines the set of slash commands available in the chat input box.
//! Each command has a name (e.g. `"new"`) and a short description for the
//! autocomplete popup. Commands are matched on submit - the buffer text is
//! checked against the registered names.

/// A slash command the user can invoke from the chat input box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    /// Create a new chat session.
    New,
    /// Write the session to a single self-contained file.
    Export,
}

/// A single entry for the slash command autocomplete popup.
#[derive(Debug, Clone)]
pub struct SlashCommandEntry {
    /// The command name without the leading `/` (e.g. `"new"`).
    pub name: String,
    /// Short human-readable description for the popup.
    pub description: String,
}

impl SlashCommand {
    /// Returns all registered slash commands as popup entries.
    pub fn all_entries() -> Vec<SlashCommandEntry> {
        vec![
            SlashCommandEntry {
                name: "export".to_owned(),
                description: "Write the session to a file".to_owned(),
            },
            SlashCommandEntry {
                name: "new".to_owned(),
                description: "Create a new session".to_owned(),
            },
        ]
    }

    /// Looks up a slash command by name (without the leading `/`).
    pub fn lookup(name: &str) -> Option<Self> {
        match name {
            "export" => Some(Self::Export),
            "new" => Some(Self::New),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn lookup_new_returns_new() {
        // Given the command name "new".
        // When looking it up.
        // Then it resolves to the New command.
        assert_eq!(SlashCommand::lookup("new"), Some(SlashCommand::New));
    }

    #[rstest::rstest]
    fn lookup_removed_compact_names_resolve_nothing() {
        // Given the retired command names.
        // When looking each up.
        // Then nothing resolves — the compaction surface is gone.
        assert_eq!(SlashCommand::lookup("compact"), None);
        assert_eq!(SlashCommand::lookup("compact-all"), None);
    }

    #[rstest::rstest]
    fn lookup_unknown_returns_none() {
        // Given a name that matches no command.
        // When looking it up.
        // Then nothing is resolved.
        assert_eq!(SlashCommand::lookup("nonexistent"), None);
    }

    #[rstest::rstest]
    fn every_advertised_name_is_executable() {
        // Given the full command list.
        let entries = SlashCommand::all_entries();

        // When looking each advertised name up.
        // Then every one resolves, so the popup and the executor agree.
        for entry in &entries {
            assert!(
                SlashCommand::lookup(&entry.name).is_some(),
                "advertised command `{}` does not resolve",
                entry.name
            );
        }
    }

    #[rstest::rstest]
    fn all_entries_contains_all_commands() {
        // Given the full command list.
        let entries = SlashCommand::all_entries();

        // When collecting the advertised names.
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Then every command is advertised.
        assert!(names.contains(&"export"));
        assert!(names.contains(&"new"));
    }
}
