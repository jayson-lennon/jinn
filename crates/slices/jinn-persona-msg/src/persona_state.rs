//! The persona slice's shared cell vocabulary.
//!
//! [`Persona`] lives in the slice-surface layer (not in the slice crate)
//! because the *readers* include kernel-resident code: the session actor
//! fills `context.personas` from the TUI-adjacent
//! `PersonasLoaded` event the slice publishes, and the persona picker
//! spec renders from the same type. The writer — the activation-time
//! markdown scan — lives in the slice crate. Both import this one type;
//! neither depends on the other.

use jinn_slices::SlotKey;

/// A parsed persona ready for use in the system prompt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Persona {
    /// Unique persona name (from frontmatter).
    pub name: String,
    /// Short description for the picker UI.
    pub description: String,
    /// The persona body - the actual system prompt text.
    pub body: String,
}

/// The persona slice's cell payload.
///
/// The name-sorted persona set the activation scan produced. The session
/// actor copies it into `context.personas` on `PersonasLoaded`; the cell
/// is the slice's own durable record of what was scanned.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Personas {
    /// The scanned personas, sorted by name.
    pub entries: Vec<Persona>,
    /// The active persona's NAME (not the payload — resolve via
    /// [`Personas::active`]). `None` until a selection is made; seeded
    /// to the default persona by the session actor on `PersonasLoaded`.
    #[serde(default)]
    pub active: Option<String>,
}

impl Personas {
    /// Looks up one persona by exact name.
    #[must_use]
    pub fn persona(&self, name: &str) -> Option<&Persona> {
        self.entries.iter().find(|p| p.name == name)
    }

    /// The active persona payload, if its name still resolves.
    ///
    /// Selections that outlive their persona (file deleted, scan shrank)
    /// resolve to `None` — the caller applies the default-persona
    /// fallback, mirroring the old context-state behavior.
    #[must_use]
    pub fn active(&self) -> Option<&Persona> {
        match self.active.as_deref() {
            Some(n) => self.persona(n),
            None => None,
        }
    }

    /// Replaces the catalog while preserving an existing selection when
    /// possible (the session actor's `PersonasLoaded` policy):
    ///
    /// 1. `seeded_persona_name` (if set and present in the new list) wins.
    /// 2. Otherwise the current `active` selection survives if still present.
    /// 3. Otherwise `default_name`.
    /// 4. Otherwise the first entry (alphabetically first, since the scan
    ///    is name-sorted).
    pub fn seeded_replace(
        &mut self,
        entries: Vec<Persona>,
        seeded_persona_name: Option<&str>,
        default_name: &str,
    ) {
        let present = |name: &str| entries.iter().any(|p: &Persona| p.name == name);
        let target = seeded_persona_name
            .filter(|n| present(n))
            .or_else(|| self.active.as_deref().filter(|n| present(n)))
            .unwrap_or(default_name);
        let target = if present(target) {
            Some(target.to_owned())
        } else {
            entries.first().map(|p| p.name.clone())
        };
        self.entries = entries;
        self.active = target;
    }

    /// Resolves the persona for a session's requested name with the
    /// same fallback order the old context state used: the session's
    /// persona name first, then the active selection, then the default
    /// persona name.
    #[must_use]
    pub fn resolve_for(&self, session_persona_name: &str, default_name: &str) -> Option<&Persona> {
        self.persona(session_persona_name)
            .or_else(|| self.active())
            .or_else(|| self.persona(default_name))
    }
}

/// The slot key the persona slice's cell lives under.
#[must_use]
pub fn personas_slot() -> SlotKey {
    SlotKey::builtin("persona", "entries")
}
