//! Handoff compaction configuration schema — the `jinn.toml`
//! `[context_curation.handoff_compaction]` section.
//!
//! Pure serde data. Implements the EoCC (End-of-Conversation Compaction)
//! regime from arXiv 2611.05306: at each model-request boundary the
//! session model authors a handoff recap in-conversation, the context
//! folds to task + recap, and work resumes. The sibling
//! [`CompactionConfig`] section is unchanged; this section exists
//! alongside it.

use serde::{Deserialize, Serialize};

/// Default fraction of the effective context limit at which the handoff
/// gate fires.
///
/// Deliberately below [`crate::schemas::compaction`]'s 0.7 auto-compaction
/// threshold so the handoff wins the race against background compaction.
const DEFAULT_HANDOFF_THRESHOLD: f64 = 0.6;

/// Minimum permitted `threshold` value.
///
/// The handoff gate fires at every model-request boundary; a threshold
/// near zero would put a handoff turn in front of every request.
const MIN_HANDOFF_THRESHOLD: f64 = 0.5;

/// Handoff compaction configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each gate evaluation, so an edit applies at the next
/// model-request boundary.
///
/// Serialized as `[context_curation.handoff_compaction]` in `jinn.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCompactionConfig {
    /// Master switch. `false` (default) means nothing anywhere observes
    /// this feature: no gate check, no prompt section, no fold.
    #[serde(default)]
    pub enabled: bool,
    /// Fraction of the effective context limit at which the handoff gate
    /// fires (0.5–1.0; validated floor 0.5). Default: 0.6.
    ///
    /// Keep this below the background compaction
    /// (`[context_curation.compaction]`) threshold so the handoff fires
    /// first, and below the auto-prune accumulation point so prune does
    /// not delete tool evidence the handoff recap needs to read.
    #[serde(default = "default_handoff_threshold")]
    pub threshold: f64,
    /// Explicit total context limit in tokens — the paper's W. Overrides
    /// the model's reported `context_length` when set.
    ///
    /// Default: the active model's `context_length` from the provider
    /// cache; when that is unavailable the gate skips (never guesses in
    /// blocking mode).
    #[serde(default)]
    pub context_limit: Option<usize>,
}

fn default_handoff_threshold() -> f64 {
    DEFAULT_HANDOFF_THRESHOLD
}

impl Default for HandoffCompactionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: DEFAULT_HANDOFF_THRESHOLD,
            context_limit: None,
        }
    }
}

impl HandoffCompactionConfig {
    /// The validated threshold: clamped up to the floor.
    ///
    /// A TOML edit below the floor is silently clamped at the point of
    /// use; the layer's `validate` cannot see through serde defaults, so
    /// the clamp is the enforcement point.
    #[must_use]
    pub fn effective_threshold(&self) -> f64 {
        if self.threshold < MIN_HANDOFF_THRESHOLD {
            MIN_HANDOFF_THRESHOLD
        } else {
            self.threshold
        }
    }

    /// Whether the gate may fire for the given threshold.
    #[must_use]
    pub fn threshold_floor() -> f64 {
        MIN_HANDOFF_THRESHOLD
    }
}

impl jinn_config::Configurable for HandoffCompactionConfig {
    const KEY: &'static str = "context_curation.handoff_compaction";
}

#[cfg(test)]
#[path = "handoff_compaction_tests.rs"]
mod tests;
