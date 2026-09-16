//! Chat input box - message composition UI, validation, and intent handling.
//!
//! Co-locates everything about the chat input box:
//!
//! - **State** - `ChatInputBoxState` holds the input buffer, cursor, and autocomplete state.
//! - **Element** - renders the input prompt with cursor positioning and mode-aware styling.
//! - **Validator** - validates message submission, autocomplete confirmation, and normal escape.
//! - **Intent** - handles 16 intents: 13 chat-input intents (character insertion, deletion,
//!   submission, autocomplete, cursor movement), plus EnterInsertMode, EnterNormalMode,
//!   and NormalEscape.
//! - **Autocomplete render** - renders the prompt template autocomplete popup overlay.

pub mod autocomplete_render;

#[cfg(test)]
mod autocomplete_render_tests;
pub mod element;
#[cfg(test)]
mod element_tests;
pub mod intent;
#[cfg(test)]
mod intent_phase2_tests;
#[cfg(test)]
mod intent_tests;
pub mod protocol;
pub mod slash_command;
pub mod state;
pub mod validator;

// Re-export state types for convenience.
pub use jinn_chat_input_msg::AutocompleteMatch;
pub use jinn_chat_input_msg::AutocompleteTrigger;
pub use jinn_chat_input_msg::ChatInputBoxState;
pub use jinn_chat_input_msg::InputMode;

// Re-export element for registration.
pub use element::ChatInputBoxElement;

use crate::common::AppUiRegistry;

/// Register chat input box UI element.
pub fn register(registry: &mut AppUiRegistry) {
    registry.register(Box::new(ChatInputBoxElement));
}

#[cfg(test)]
mod register_tests {
    use super::*;
    use crate::common::AppUiRegistry;

    #[rstest::rstest]
    #[test]
    fn register_adds_element_to_registry() {
        // Given an empty registry.
        let mut registry = AppUiRegistry::new();

        // When registering chat input elements.
        register(&mut registry);

        // Then the registry is not empty.
        assert_eq!(registry.iter_mut().count(), 1);
    }
}

#[cfg(test)]
mod chat_input_tests;
