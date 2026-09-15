//! User override for whether a chat entry participates in LLM context.

use serde::{Deserialize, Serialize};

/// User-controlled override for whether an entry is included in LLM context.
///
/// Tri-state that replaces the old `ignored: bool` field, supporting both
/// inclusion and exclusion overrides. The `x` key always flips the entry's
/// *effective* in-context state (landing on an explicit `Forced*` value) —
/// it never produces [`ContextOverride::Default`]. The `r` key resets an
/// entry back to [`ContextOverride::Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextOverride {
    /// Follow the entry kind's default inclusion rule.
    #[default]
    Default,
    /// User has explicitly forced this entry into the LLM context.
    ForcedInclude,
    /// User has explicitly forced this entry out of the LLM context
    /// (replaces old `ignored: true`).
    ForcedExclude,
}
