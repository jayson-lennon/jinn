//! The `[[stream_rules.entry]]` list — regex rules tested against the live
//! assistant stream.
//!
//! Every other command- or output-shaping rule in `jinn.toml` runs *before*
//! a thing happens or *after* it finished. A stream rule runs in the middle:
//! its `conditions` are regexes over the accumulating assistant output, and
//! the first one to match interrupts the turn and resumes it with the
//! entry's `body` injected as guidance. That is the only mechanism jinn has
//! for catching a model doing something it was told not to while it is
//! still doing it.
//!
//! This module holds the section's declaration and its value shape only.
//! Compilation — turning `conditions` and `scopes` into a matcher, and
//! buffering the stream it matches against — lives in the
//! `jinn-stream-rules` slice, which reads this shape.

use serde::{Deserialize, Serialize};

/// The `jinn.toml` key the stream rules live at.
///
/// Dotted on purpose: the patcher resolves a list's parent table by the
/// head of the key and refuses to drop unmatched entries from a
/// single-segment one, so a bare `rules` would leave every deleted rule in
/// the file forever.
pub const STREAM_RULES_KEY: &str = "stream_rules.entry";

/// One rule, as written in `jinn.toml`.
///
/// An absent or empty `scopes` admits every stream — assistant prose,
/// reasoning, and serialized tool-call arguments alike. Naming scopes
/// narrows that to where the rule is meaningful: a rule about TypeScript
/// belongs on tool arguments, not on prose.
///
/// A rule missing a usable `body` is skipped with a warning rather than
/// interrupting a turn with nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRuleConfig {
    /// The rule's identity, unique per session, and the name the
    /// interruption reports.
    pub name: String,
    /// Prose description of what the rule forbids. Carried for the user's
    /// own benefit — jinn reads the `body`, not this.
    #[serde(default)]
    pub description: String,
    /// Regexes tested against the accumulated stream buffer.
    ///
    /// A pattern the regex engine rejects is dropped with a warning naming
    /// the rule, and the rule survives on its remaining patterns.
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Scope tokens naming where the rule applies.
    ///
    /// `text`, `thinking`, `tool`, and `tool:<name>(<glob>)` are the whole
    /// grammar; anything else is dropped with a warning. Absent or empty
    /// means every stream.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The markdown guidance injected when the rule fires.
    pub body: String,
}

impl jinn_config::ConfigList for StreamRuleConfig {
    const KEY: &'static str = STREAM_RULES_KEY;
    const ENTRY_KEY: &'static str = "name";
    const ENTRY_FIELDS: &'static [&'static str] =
        &["name", "description", "conditions", "scopes", "body"];
}
