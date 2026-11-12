//! Canonical contracts and shared vocabulary for the persona slice.

mod compaction_block;
mod event;
pub mod persona_entry;
mod persona_picker_scope;
mod persona_picker_state;
pub mod persona_state;

pub use compaction_block::missing_compaction_block_message;
pub use event::PersonasLoaded;
pub use persona_entry::{PersonaEntry, persona_row};
pub use persona_picker_scope::persona_picker_scope;
pub use persona_picker_state::{
    PersonaPickerState, RESULTS_VIEWPORT_FALLBACK, persona_picker_slot,
};
pub use persona_state::{Personas, personas_slot};

#[cfg(test)]
mod tests {
    use super::*;
    use trouper::schema::{Schema, SchemaKind};

    #[rstest::rstest]
    #[test]
    fn crossing_schema_contract_is_stable() {
        // Given the canonical persona crossing contracts.
        // When reading their schema definitions.
        let event = <PersonasLoaded as Schema>::schema_def();

        // Then the event name and kind are stable.
        assert_eq!(event.name, "PersonasLoaded");
        assert!(matches!(event.kind, SchemaKind::Event));
    }
}
