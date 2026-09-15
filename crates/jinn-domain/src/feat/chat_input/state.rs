//! State types for the chat input box.
//!
//! The concrete types live in `jinn-slices` (the chat-input slice's shared
//! vocabulary); this module re-exports them so kernel paths
//! (`crate::feat::chat_input::state::...`) keep resolving.

pub use jinn_slices::chat_input_state::autocomplete::AutocompleteState;
pub use jinn_slices::chat_input_state::autocomplete::AutocompleteTrigger;
pub use jinn_slices::chat_input_state::chat_input_box::ChatInputBoxState;
pub use jinn_slices::chat_input_state::chat_input_box::InputMode;
pub use jinn_slices::chat_input_state::wrap::WrappedLine;
pub use jinn_slices::chat_input_state::wrap_text;

/// Re-export shim: autocomplete vocabulary lives in `jinn-slices`.
pub mod autocomplete {
    pub use jinn_slices::chat_input_state::autocomplete::*;
}

/// Re-export shim: input-box state lives in `jinn-slices`.
pub mod chat_input_box {
    pub use jinn_slices::chat_input_state::chat_input_box::*;
}

/// Re-export shim: word-wrap vocabulary lives in `jinn-slices`.
pub mod wrap {
    pub use jinn_slices::chat_input_state::wrap::*;
}
