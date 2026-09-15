//! Picker identity — a composition-assigned, crate-unique picker name.

use std::fmt;

/// The stable identifier of a picker spec within the registry.
///
/// `'static` lifetime keeps lookup by value possible (the registry keys on
/// the inner string) while intents carry runtime `String`s — `get(&str)`
/// compares inner strings so a `PickerId` never has to outlive its spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PickerId(&'static str);

impl PickerId {
    /// Mints a picker id from its canonical name, e.g. `"skill"`.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The canonical name, e.g. `"skill"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for PickerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[test]
    fn id_preserves_and_displays_its_name() {
        // Given a picker id.
        let id = PickerId::new("skill");

        // When reading its name back.
        // Then the name round-trips and displays as itself.
        assert_eq!(id.as_str(), "skill");
        assert_eq!(id.to_string(), "skill");
    }
}
