//! Derivation of currently-applied auto-prune savings from entry state.
//!
//! [`prune_report`] scans the active session's history at render time and
//! totals the token cost of entries that are currently excluded by an
//! auto-prune worker — `ContextOverride::ForcedExclude` whose latest
//! `context_history` event names a worker. No state is written: the
//! quake bar recomputes this per frame, so the number always reflects
//! the live history (re-includes drop out automatically and pending,
//! un-flushed prunes never appear).

use jinn_core_types::{ChangeSource, ChatEntry, ContextOverride};

/// Result of scanning history for currently-applied auto-pruner exclusions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Sum of persisted token counts over counted entries (lower bound when
    /// counts are not yet computed).
    pub tokens: u64,
    /// Number of counted entries.
    pub entries: usize,
}

/// Totals the entries currently excluded by workers.
///
/// An entry is counted iff its override is still
/// [`ContextOverride::ForcedExclude`] and its most recent
/// `context_history` event came from a `ChangeSource::Worker`. Entries
/// whose token count is not yet computed contribute zero tokens but
/// still count, so `tokens` is a lower bound. Entries whose prune is
/// still buffered in the mutation accumulator are not applied yet and
/// are never counted.
pub fn prune_report(history: &[ChatEntry]) -> PruneReport {
    let counted = history.iter().filter(|entry| is_pruned_by_worker(entry));
    let tokens = counted
        .clone()
        .map(|entry| u64::from(entry.token_count.unwrap_or(0)))
        .sum();
    let entries = counted.count();

    PruneReport { tokens, entries }
}

/// Whether the entry is currently excluded by a worker.
///
/// Counts only entries whose override is still `ForcedExclude` (a later
/// user re-include drops them out) and whose latest recorded context
/// change names a worker — never the user, and never an unattributed
/// exclude with no recorded history.
fn is_pruned_by_worker(entry: &ChatEntry) -> bool {
    let Some(last) = entry.context_history.last() else {
        return false;
    };

    entry.context_override() == ContextOverride::ForcedExclude
        && matches!(&last.source, ChangeSource::Worker { .. })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_session_state::mutation_accumulator::MutationAccumulator;

    const PRUNER_NAME: &str = "edit_read";

    fn worker_prune(entry: &mut ChatEntry) {
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Worker {
                name: PRUNER_NAME.to_owned(),
            },
        );
    }

    #[rstest::rstest]
    #[test]
    fn counts_worker_excluded_entry_tokens() {
        // Given a worker-excluded entry with a persisted token count.
        let mut entry = ChatEntry::user("big tool output");
        worker_prune(&mut entry);
        entry.token_count = Some(300);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the entry's tokens are counted.
        assert_eq!(report.tokens, 300);
        // And one entry is counted.
        assert_eq!(report.entries, 1);
    }

    #[rstest::rstest]
    #[test]
    fn counts_persisted_compaction_named_excludes() {
        // Given an entry whose latest exclude names the retired compaction
        // worker — the shape persisted sessions still carry.
        let mut entry = ChatEntry::user("summarized away");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Worker {
                name: "compaction".to_owned(),
            },
        );
        entry.token_count = Some(300);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the exclude is counted like any worker exclude: the report
        // no longer special-cases the retired worker name.
        assert_eq!(report.tokens, 300);
        assert_eq!(report.entries, 1);
    }

    #[rstest::rstest]
    #[test]
    fn skips_user_sourced_excludes() {
        // Given a user-excluded entry.
        let mut entry = ChatEntry::user("user ignored this");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        entry.token_count = Some(300);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the user exclude is not counted.
        assert_eq!(report.tokens, 0);
        assert_eq!(report.entries, 0);
    }

    #[rstest::rstest]
    #[test]
    fn drops_reincluded_entries() {
        // Given a worker-excluded entry that the user later re-included.
        let mut entry = ChatEntry::user("brought back");
        worker_prune(&mut entry);
        entry.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
        entry.token_count = Some(300);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the re-included entry is not counted.
        assert_eq!(report.tokens, 0);
        assert_eq!(report.entries, 0);
    }

    #[rstest::rstest]
    #[test]
    fn skips_pending_only_prunes() {
        // Given an entry still at its default override whose prune is buffered
        // only in an accumulator.
        let entry = ChatEntry::user("not yet flushed");
        let mut accumulator = MutationAccumulator::default();
        accumulator.push(
            entry.id.clone(),
            ContextOverride::ForcedExclude,
            ChangeSource::Worker {
                name: PRUNER_NAME.to_owned(),
            },
            300,
        );

        // When computing the prune report from history.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the pending-only prune is not counted — the accumulator is
        // invisible to the derivation.
        assert_eq!(report.tokens, 0);
        assert_eq!(report.entries, 0);
    }

    #[rstest::rstest]
    #[test]
    fn missing_token_count_contributes_zero_but_entry_counts() {
        // Given a worker-excluded entry whose count is not yet computed.
        let mut entry = ChatEntry::user("uncounted");
        worker_prune(&mut entry);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the tokens are a lower bound (0).
        assert_eq!(report.tokens, 0);
        // And the entry still counts.
        assert_eq!(report.entries, 1);
    }

    #[rstest::rstest]
    #[test]
    fn empty_history_yields_zero_report() {
        // Given no history.
        // When computing the prune report.
        let report = prune_report(&[]);

        // Then both totals are zero.
        assert_eq!(report.tokens, 0);
        assert_eq!(report.entries, 0);
    }

    #[rstest::rstest]
    #[test]
    fn unattributed_exclude_not_counted() {
        // Given a ForcedExclude entry with an empty context-history audit trail.
        let mut entry = ChatEntry::user("restored without audit")
            .with_context_override(ContextOverride::ForcedExclude);
        entry.token_count = Some(300);

        // When computing the prune report.
        let report = prune_report(std::slice::from_ref(&entry));

        // Then the unattributed exclude is not counted.
        assert_eq!(report.tokens, 0);
        assert_eq!(report.entries, 0);
    }
}
