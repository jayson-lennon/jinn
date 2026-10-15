//! State and identity for the session-lifecycle argument popup.
//!
//! The picker and the lifecycle slice both need the same cell and scope
//! identity, so this kernel-free vocabulary lives in the lifecycle message
//! crate. The parsed command template is snapshotted with the selected
//! lifecycle, giving rendering and validation a stable view for the popup's
//! lifetime.

use jinn_slices::LineInput;
use jinn_slices::SliceScopeId;
use jinn_slices::SlotKey;

use crate::CommandTemplate;

/// The lifecycle argument popup's dynamic input-capturing scope.
#[must_use]
pub fn arg_input_scope() -> SliceScopeId {
    SliceScopeId::new("session-lifecycle", "arg_input")
}

/// The slot containing the lifecycle argument popup state.
#[must_use]
pub fn arg_input_slot() -> SlotKey {
    SlotKey::builtin("session-lifecycle", "arg_input")
}

/// The selected lifecycle and the arguments currently being edited.
#[derive(Debug, Clone)]
pub struct ArgInputState {
    /// The lifecycle selected from the picker.
    pub lifecycle_name: String,
    /// The selected lifecycle's parsed setup command.
    pub template: CommandTemplate,
    /// The editable argument text and byte-offset cursor.
    pub text: LineInput,
}

impl ArgInputState {
    /// Creates popup state for `lifecycle_name` and its command template.
    #[must_use]
    pub fn new(lifecycle_name: String, template: CommandTemplate) -> Self {
        Self {
            lifecycle_name,
            template,
            text: LineInput::new(),
        }
    }

    /// Creates the inert state used before the picker selects a lifecycle.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(String::new(), CommandTemplate::parse(""))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test module, panics are acceptable")]

    use super::*;

    #[rstest::rstest]
    fn new_starts_with_empty_text_at_origin() {
        // Given a lifecycle name and parsed setup command.
        let name = "research".to_owned();
        let template = CommandTemplate::parse("cd /work/$1");

        // When creating popup state.
        let state = ArgInputState::new(name.clone(), template.clone());

        // Then the lifecycle and template are retained with empty input.
        assert_eq!(state.lifecycle_name, name);
        assert_eq!(state.template, template);
        assert_eq!(state.text.input, "");
        assert_eq!(state.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn popup_identity_is_stable() {
        // Given nothing but the popup's own identity accessors.
        // When reading the popup identities twice.
        let first_scope = arg_input_scope();
        let second_scope = arg_input_scope();
        let first_slot = arg_input_slot();
        let second_slot = arg_input_slot();

        // Then both reads mint the same scope identity.
        assert_eq!(first_scope, second_scope);
        // And both mint the same slot identity.
        assert_eq!(first_slot, second_slot);
        // And the scope captures input.
        assert!(first_scope.captures_input());
    }
}
