//! The persona picker's spec — behavior authored once in the builder.
//!
//! Hooks fill in as the persona picker migrates off the legacy per-kind
//! handler arms; the id and widget kind here are the registry's source of
//! truth from the moment of registration.

use jinn_picker::PickerId;
use jinn_picker::PickerSpec;

/// The domain entry wrapped by this picker's items.
#[derive(Debug)]
pub struct PersonaEntry {
    /// Display name of the persona.
    pub name: String,
}

/// Builds the persona picker's spec.
#[must_use]
pub fn persona_spec() -> PickerSpec<PersonaEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::PERSONA_ID))
        .title(" Personas ")
}
