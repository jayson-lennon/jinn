pub mod cwd_input_state;
pub mod suggest;

pub use cwd_input_state::*;
pub use suggest::{SuggestCwdCompletion, resolve_parent_dir, split_at_cursor};

#[cfg(test)]
mod tests {
    use trouper::schema::{Schema, SchemaKind};

    use super::SuggestCwdCompletion;

    #[rstest::rstest]
    #[test]
    fn suggest_command_schema_contract_is_stable() {
        // Given the cwd popup's crossing contract.
        // When reading its schema definition.
        let schema = <SuggestCwdCompletion as Schema>::schema_def();

        // Then the name and Command kind are stable — the transport the
        // suggestion request rides depends on this kind.
        assert_eq!(schema.name, "SuggestCwdCompletion");
        assert!(matches!(schema.kind, SchemaKind::Command));
    }
}
