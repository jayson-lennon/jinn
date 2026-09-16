//! The chat-input slice's shared cell vocabulary.
//!
//! [`ChatInputBoxState`] is the user's in-progress message: the text buffer
//! with its grapheme-bounds and word-wrap caches, the cursor, the scroll
//! offset, the sticky `Queue`/`Steer` submission mode, and the active
//! `#`/`/`/`@` autocomplete session. It lives beside its helpers
//! ([`wrap`] and [`autocomplete`]) in `jinn-slices` so the kernel
//! (intents, `ChatSessionState`) and the TUI renderer reference the type
//! without depending on the slice crate — the same layering rule as
//! [`crate::ChatLogViewUi`].
//!
//! Writers are the exempt IntentHandler (editing/cursor/submit arms, via
//! `ChatSession`'s closure accessors), the renderer (per-frame wrap-width
//! and scroll upkeep), and the session actor (queued messages drain back
//! into the box on stream error/cancel). There is no actor and no route
//! row for this slice.

pub mod autocomplete;
pub mod chat_input_box;
pub mod wrap;

pub use autocomplete::AutocompleteState;
pub use autocomplete::AutocompleteTrigger;
pub use chat_input_box::ChatInputBoxState;
pub use chat_input_box::InputMode;
pub use wrap::WrappedLine;
pub use wrap::wrap_text;

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_slices::SlotKey;

/// A single match for the prompt template autocomplete popup.
#[derive(Debug, Clone)]
pub struct AutocompleteMatch {
    /// The template name (e.g. `"code-review"`).
    pub name: String,
    /// Short human-readable description for the popup.
    pub description: String,
}

/// The chat-input cell's payload: one input draft per session, keyed by
/// session id.
///
/// Readers must not grow the map (a session with no entry reads as its
/// default draft); only writers get-or-insert. Entries persist for the
/// life of the process — bounded by the number of sessions opened, and
/// cleaned up with the session family later.
pub type ChatInputs = HashMap<SessionId, ChatInputBoxState>;

/// The slot key the chat-input cell lives under.
#[must_use]
pub fn chat_inputs_slot() -> SlotKey {
    SlotKey::builtin("chat-input", "state")
}
