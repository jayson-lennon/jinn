//! The persona slice's shared cell vocabulary.
//!
//! [`Persona`] lives in the slice-surface layer (not in the slice crate)
//! because the *readers* include kernel-resident code: the session actor
//! fills `context.personas` from the [`crate::tui_signals`]-adjacent
//! `PersonasLoaded` event the slice publishes, and the persona picker
//! spec renders from the same type. The writer — the activation-time
//! markdown scan — lives in the slice crate. Both import this one type;
//! neither depends on the other.

use crate::SlotKey;

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
#[derive(Debug, Default, Clone)]
pub struct Personas {
    /// The scanned personas, sorted by name.
    pub entries: Vec<Persona>,
}

impl Personas {
    /// Looks up one persona by exact name.
    #[must_use]
    pub fn persona(&self, name: &str) -> Option<&Persona> {
        self.entries.iter().find(|p| p.name == name)
    }
}

/// The slot key the persona slice's cell lives under.
#[must_use]
pub fn personas_slot() -> SlotKey {
    SlotKey::builtin("persona", "entries")
}
