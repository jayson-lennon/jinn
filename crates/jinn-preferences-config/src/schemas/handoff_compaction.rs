//! Handoff compaction configuration schema — the `jinn.toml`
//! `[context_curation.handoff_compaction]` section.
//!
//! Pure serde data. End-of-conversation compaction: at each
//! model-request boundary whose working context reaches the configured
//! trigger, the session model writes a handoff recap in-conversation and
//! the context folds down to the task turns, pins, and the recap. The
//! sibling [`crate::schemas::CompactionConfig`] section is unchanged;
//! this section exists alongside it.

use serde::{Deserialize, Serialize};

/// Handoff compaction configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each gate evaluation, so an edit applies at the next
/// model-request boundary.
///
/// The regime is configured in absolute tokens, mirroring how the
/// feature bounds context: `context_limit` sets the total budget and
/// `trigger_tokens` the fire line inside it. The gap between the two
/// reserves room for the recap turn — the fire line must sit strictly
/// below the limit, and the gate refuses to fire when it does not.
///
/// Serialized as `[context_curation.handoff_compaction]` in `jinn.toml`.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCompactionConfig {
    /// Master switch. `false` (default) means nothing anywhere observes
    /// this feature: no gate check, no prompt section, no fold.
    #[serde(default)]
    pub enabled: bool,
    /// Total context budget in tokens. Overrides the model's reported
    /// `context_length` when set.
    ///
    /// Default: the active model's `context_length` from the provider
    /// cache; when that is unavailable the gate skips (never guesses in
    /// blocking mode).
    #[serde(default)]
    pub context_limit: Option<usize>,
    /// The fire line in tokens: the handoff fires at the first
    /// model-request boundary whose estimated working context meets or
    /// exceeds this count.
    ///
    /// Required for a firing regime; must sit strictly below the
    /// effective context limit so the recap turn has room. An unset or
    /// out-of-range value is a pass — the gate never fires an unbounded
    /// or overfull handoff.
    #[serde(default)]
    pub trigger_tokens: Option<usize>,
}

impl HandoffCompactionConfig {
    /// The regime the gate may fire under, resolved against the model's
    /// reported limit: `(effective_limit, trigger)` or `None` when
    /// unconfigured or invalid (trigger missing, at, or beyond the
    /// limit — the gap must reserve the recap turn's room).
    ///
    /// An explicit `context_limit` wins over the model's reported
    /// length; both must exist for the regime to resolve.
    #[must_use]
    pub fn resolve_regime(&self, model_reported_limit: Option<u32>) -> Option<(u32, u32)> {
        if !self.enabled {
            return None;
        }
        let limit = match self.context_limit {
            Some(explicit) => u32::try_from(explicit).ok()?,
            None => model_reported_limit?,
        };
        let trigger = u32::try_from(self.trigger_tokens?).ok()?;
        if trigger >= limit {
            return None;
        }
        Some((limit, trigger))
    }
}

impl jinn_config::Configurable for HandoffCompactionConfig {
    const KEY: &'static str = "context_curation.handoff_compaction";
}

#[cfg(test)]
#[path = "handoff_compaction_tests.rs"]
mod tests;
