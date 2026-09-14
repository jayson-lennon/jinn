//! Composition-side picker registry — where specs are built and registered.
//!
//! This module is the adapter between the kernel's legacy `PickerKind`
//! world and the generic `jinn-picker` spec world: specs register here at
//! composition time, and [`spec_id_for_kind`] maps the static per-kind
//! scopes/intents onto the registry until every picker has migrated.
//!
//! When the last picker migrates, the adapter (and eventually `PickerKind`
//! itself) is deleted; `registered_ids` then *is* the picker vocabulary.

use jinn_picker::PickerRegistry;

/// The id of the persona picker's spec.
pub const PERSONA_ID: &str = "persona";
/// The id of the skill picker's spec.
pub const SKILL_ID: &str = "skill";

/// Maps a legacy `PickerKind` onto its spec id, `None` while the kind has
/// not migrated yet.
///
/// The pilot migrates persona + skill; every other kind falls through to
/// the legacy per-kind handlers.
#[must_use]
pub fn spec_id_for_kind(kind: &crate::feat::picker::PickerKind) -> Option<&'static str> {
    match kind {
        crate::feat::picker::PickerKind::Persona => Some(PERSONA_ID),
        crate::feat::picker::PickerKind::Skill => Some(SKILL_ID),
        _ => None,
    }
}

/// Builds the domain's picker registry: every migrated picker registers its
/// spec here once at composition.
#[must_use]
pub fn build_picker_registry() -> PickerRegistry {
    let mut registry = PickerRegistry::new();
    registry.register(super::persona_spec::persona_spec());
    registry.register(super::skill_spec::skill_spec());
    registry
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::PickerKind;

    #[rstest::rstest]
    #[test]
    fn migrated_kinds_map_onto_registered_specs_and_vice_versa() {
        // Given the domain's picker registry and the kind→id adapter.
        let registry = build_picker_registry();
        let migrated = [PickerKind::Persona, PickerKind::Skill];

        // When mapping each migrated kind and listing registered ids.
        let mapped_ids: Vec<&str> = migrated.iter().filter_map(spec_id_for_kind).collect();
        let mut registered_ids = registry.ids();
        registered_ids.sort_unstable();

        // Then the two sets are exactly equal — a kind with a spec id but
        // no registered spec (or the reverse) is a wiring bug.
        let mut expected = mapped_ids.clone();
        expected.sort_unstable();
        assert_eq!(registered_ids, expected);
        assert_eq!(mapped_ids.len(), migrated.len());
    }

    #[rstest::rstest]
    #[test]
    fn unmigrated_kinds_have_no_spec_id() {
        // Given every kind that has not migrated in the pilot.
        let unmigrated = [
            PickerKind::Provider,
            PickerKind::Session,
            PickerKind::Theme,
            PickerKind::SessionLifecycle,
            PickerKind::CompactionModel,
            PickerKind::ReasoningEffort,
            PickerKind::Tool,
            PickerKind::TaskList,
        ];

        // When mapping each kind.
        // Then none resolves to a spec id (legacy handlers stay in charge).
        for kind in &unmigrated {
            assert!(spec_id_for_kind(kind).is_none(), "{kind:?} unmapped");
        }
    }
}
