//! Personas - customizable system prompt profiles for the LLM.
//!
//! Persona discovery flows through a user-installed plugin (see
//! `plugins/persona-loader`): the plugin scans the user personas
//! directory (`~/.config/jinn/personas/`), parses the `+++` TOML
//! frontmatter, and contributes the set over the plugin wire. The plugin
//! coordinator translates the contribution and publishes it as a
//! `PersonasLoaded` event; the session actor consumes that event to
//! populate the persona catalog and resolve the active persona. Each
//! persona defines the agent's identity, behavioral guidelines, and any
//! other system prompt content.

#[expect(
    clippy::module_inception,
    reason = "persona/mod.rs is the public API, persona/ is implementation"
)]
mod persona;
mod persona_entry;

pub use persona::Persona;
pub use persona_entry::PersonaEntry;
pub(crate) use persona_entry::render_persona_row;
