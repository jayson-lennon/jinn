//! The persona slice's shared cell vocabulary.
//!
//! [`Persona`] lives in the slice-surface layer (not in the slice crate)
//! because the *readers* include kernel-resident code: the session actor
//! fills `context.personas` from the TUI-adjacent
//! `PersonasLoaded` event the slice publishes, and the persona picker
//! spec renders from the same type. The writer — the activation-time
//! markdown scan — lives in the slice crate. Both import this one type;
//! neither depends on the other.

use jinn_slices::Persona;
use jinn_slices::SlotKey;

/// The persona slice's cell payload.
///
/// The name-sorted persona set the activation scan produced. The session
/// actor copies it into `context.personas` on `PersonasLoaded`; the cell
/// is the slice's own durable record of what was scanned.
///
/// There is no active selection here: the active session's profile
/// persona is the single persona authority, and this payload is a pure
/// catalog of what exists.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Personas {
    /// The scanned personas, sorted by name.
    pub entries: Vec<Persona>,
}

impl Personas {
    /// Builds a catalog from the scanned personas.
    #[must_use]
    pub fn new(entries: Vec<Persona>) -> Self {
        Self { entries }
    }

    /// Looks up one persona by exact name.
    #[must_use]
    pub fn persona(&self, name: &str) -> Option<&Persona> {
        self.entries.iter().find(|p| p.name == name)
    }

    /// Replaces the catalog with a freshly scanned persona set.
    pub fn replace_entries(&mut self, entries: Vec<Persona>) {
        self.entries = entries;
    }

    /// Resolves the persona for a session's requested name: the
    /// session's persona name first, then the default persona name.
    ///
    /// Names that stopped resolving (file deleted, scan shrank) are
    /// rewritten to the default by the session actor at ingestion, so
    /// this fallback is a display-time safety net, not a state machine.
    #[must_use]
    pub fn resolve_for(&self, session_persona_name: &str, default_name: &str) -> Option<&Persona> {
        self.persona(session_persona_name)
            .or_else(|| self.persona(default_name))
    }

    /// The catalog's name for `name`: itself when present, else
    /// `default_name` when present, else the first entry (alphabetically
    /// first, since the scan is name-sorted), else `name` unchanged.
    ///
    /// The pass-through on an empty catalog means a failed scan can
    /// never blank a session's persona.
    #[must_use]
    pub fn normalize_name(&self, name: &str, default_name: &str) -> String {
        if self.persona(name).is_some() {
            return name.to_owned();
        }
        if self.persona(default_name).is_some() {
            return default_name.to_owned();
        }
        match self.entries.first() {
            Some(first) => first.name.clone(),
            None => name.to_owned(),
        }
    }
}

/// The slot key the persona slice's cell lives under.
#[must_use]
pub fn personas_slot() -> SlotKey {
    SlotKey::builtin("persona", "entries")
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
    use super::Personas;
    use jinn_slices::Persona;

    const DEFAULT: &str = "coding-assistant";

    fn persona_named(name: &str) -> Persona {
        Persona {
            name: name.to_owned(),
            description: String::new(),
            body: String::new(),
        }
    }

    fn catalog(names: &[&str]) -> Personas {
        Personas::new(names.iter().map(|n| persona_named(n)).collect())
    }

    #[rstest::rstest]
    #[test]
    fn resolve_for_returns_the_sessions_own_persona() {
        // Given a catalog with two personas.
        let personas = catalog(&["coding-assistant", "general"]);

        // When resolving a session persona the catalog holds.
        let resolved = personas.resolve_for("general", DEFAULT);

        // Then it is the requested persona, not the default.
        assert_eq!(resolved.map(|p| p.name.as_str()), Some("general"));
    }

    #[rstest::rstest]
    #[test]
    fn resolve_for_falls_back_to_default() {
        // Given a catalog holding only the default persona.
        let personas = catalog(&[DEFAULT]);

        // When resolving a session persona the catalog does not hold.
        let resolved = personas.resolve_for("deleted-persona", DEFAULT);

        // Then the default persona's payload is the resolution.
        assert_eq!(resolved.map(|p| p.name.as_str()), Some(DEFAULT));
    }

    #[rstest::rstest]
    #[test]
    fn resolve_for_is_none_when_session_and_default_are_both_absent() {
        // Given a catalog holding neither the session's persona nor the default.
        let personas = catalog(&["only-one"]);

        // When resolving an unknown persona against an unknown default.
        let resolved = personas.resolve_for("deleted-persona", DEFAULT);

        // Then nothing resolves — no global selection can sneak in behind it.
        assert!(resolved.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn normalize_name_keeps_a_present_name() {
        // Given a catalog holding the name.
        let personas = catalog(&[DEFAULT, "general"]);

        // When normalizing it.
        let normalized = personas.normalize_name("general", DEFAULT);

        // Then it comes back unchanged.
        assert_eq!(normalized, "general");
    }

    #[rstest::rstest]
    #[test]
    fn normalize_name_rewrites_an_absent_name_to_the_default() {
        // Given a catalog holding the default but not the name.
        let personas = catalog(&[DEFAULT]);

        // When normalizing the absent name.
        let normalized = personas.normalize_name("deleted-persona", DEFAULT);

        // Then the default comes back.
        assert_eq!(normalized, DEFAULT);
    }

    #[rstest::rstest]
    #[test]
    fn normalize_name_falls_back_to_the_first_entry_when_the_default_is_absent() {
        // Given a catalog without the default persona.
        let personas = catalog(&["alpha", "beta"]);

        // When normalizing a name the catalog lacks.
        let normalized = personas.normalize_name("deleted-persona", DEFAULT);

        // Then the first (name-sorted) entry is the target.
        assert_eq!(normalized, "alpha");
    }

    #[rstest::rstest]
    #[test]
    fn normalize_name_passes_the_name_through_an_empty_catalog() {
        // Given an empty catalog — a scan that found nothing.
        let personas = catalog(&[]);

        // When normalizing any name.
        let normalized = personas.normalize_name("deleted-persona", DEFAULT);

        // Then the name survives: a failed scan must never blank a persona.
        assert_eq!(normalized, "deleted-persona");
    }
}
