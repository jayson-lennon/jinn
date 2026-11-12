//! Handoff compaction configuration schema — the `jinn.toml`
//! `[context_curation.handoff_compaction]` section.
//!
//! Pure serde data. End-of-conversation compaction: at each
//! model-request boundary whose accumulated *work* reaches the
//! configured trigger, the session model writes a handoff recap
//! in-conversation and the context folds down to the task turns, pins,
//! and the recap. The sibling [`crate::schemas::CompactionConfig`]
//! section is unchanged; this section exists alongside it.

use serde::{Deserialize, Serialize};

/// Handoff compaction configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each gate evaluation, so an edit applies at the next
/// model-request boundary.
///
/// The trigger is **work-relative**: it counts the tokens of
/// accumulated work — the assembled prompt minus its constant floor
/// (system prompt, skills, shipped tool schemas), i.e. the conversation
/// content alone — rather than the whole wire size. The floor is
/// constant across requests, so subtracting it keeps a large fixed
/// floor from sitting at the fire line and folding in circles.
///
/// Serialized as `[context_curation.handoff_compaction]` in `jinn.toml`.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCompactionConfig {
    /// Master switch. `false` (default) means nothing anywhere observes
    /// this feature: no gate check, no prompt section, no fold.
    #[serde(default)]
    pub enabled: bool,
    /// The fire line in work tokens — assembled size minus the constant
    /// floor (system prompt, skills, shipped tool schemas). The handoff
    /// fires at the first model-request boundary whose estimated work
    /// meets or exceeds this count.
    ///
    /// Required for a firing regime. An unset value is a pass — the
    /// gate never fires an unbounded handoff.
    #[serde(default)]
    pub workset_trigger_tokens: Option<usize>,
}

impl HandoffCompactionConfig {
    /// The work-token fire line, or `None` when the handoff is
    /// disabled or unconfigured (no trigger set — a config from before
    /// this field resolves to nothing rather than guessing).
    #[must_use]
    pub fn resolve_trigger(&self) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        u32::try_from(self.workset_trigger_tokens?).ok()
    }
}

impl jinn_config::Configurable for HandoffCompactionConfig {
    const KEY: &'static str = "context_curation.handoff_compaction";
}

#[cfg(test)]
#[path = "handoff_compaction_tests.rs"]
mod tests;
