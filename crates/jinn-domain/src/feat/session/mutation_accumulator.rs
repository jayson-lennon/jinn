//! Accumulates pruner context-override mutations until a token threshold is reached.
//!
//! The auto-prune workers emit [`HistoryMutation::SetContextOverride`] mutations that
//! change which entries are included in the assembled LLM prompt. Applying each one
//! individually invalidates the server-side KV cache and drives up cost. This struct
//! buffers those mutations in memory, **deduplicates by target [`ChatEntryId`]**, and
//! tracks the running **deduplicated token total** so the session actor can flush them
//! as a single batch only once the configured threshold is crossed.
//!
//! # Include dominance
//!
//! The accumulator mirrors the dominance rule enforced at apply time
//! (`ChatSessionState::apply_mutations`): a buffered [`ContextOverride::ForcedInclude`]
//! (emitted by a protection worker) is **sticky** and is never displaced by an
//! incoming [`ContextOverride::ForcedExclude`] from a pruning worker. This makes the
//! dedup **order-independent** — both `include-then-exclude` and `exclude-then-include`
//! for the same entry converge to the include.

use std::collections::HashMap;

use crate::protocol::HistoryMutation;
use crate::protocol::{ChangeSource, ChatEntryId, ContextOverride};

/// One buffered override mutation, keyed by its target entry id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AccEntry {
    value: ContextOverride,
    source: ChangeSource,
    token_cost: u32,
}

/// Per-session accumulator for `SetContextOverride` mutations.
///
/// Lives on [`crate::feat::session::chat_session::SessionCoreEphemeral`] so it is never
/// persisted — buffered mutations are discarded on session close or app quit.
#[derive(Debug, Clone, Default)]
pub struct MutationAccumulator {
    entries: HashMap<ChatEntryId, AccEntry>,
    total: u64,
}

impl MutationAccumulator {
    /// Returns `true` if no mutations are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of deduplicated buffered mutations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The deduplicated token total of all buffered mutations.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total
    }

    /// Buffers a context-override mutation with shield-aware dedup.
    ///
    /// If the entry is already buffered:
    /// - A buffered `ForcedInclude` is **sticky** and survives an incoming
    ///   `ForcedExclude` (shield protection).
    /// - Any other value overwrites the buffered entry, adjusting the running total.
    ///
    /// A fresh entry is inserted and its token cost added to the running total.
    pub fn push(
        &mut self,
        entry_id: ChatEntryId,
        value: ContextOverride,
        source: ChangeSource,
        token_cost: u32,
    ) {
        // Shield dominance: a buffered ForcedInclude is never displaced by a ForcedExclude.
        if let Some(existing) = self.entries.get(&entry_id)
            && existing.value == ContextOverride::ForcedInclude
            && value == ContextOverride::ForcedExclude
        {
            return;
        }

        match self.entries.insert(
            entry_id,
            AccEntry {
                value,
                source,
                token_cost,
            },
        ) {
            // Overwrote an existing entry: rebalance the running total.
            Some(prev) => {
                self.total = self
                    .total
                    .saturating_sub(u64::from(prev.token_cost))
                    .saturating_add(u64::from(token_cost));
            }
            // Fresh entry: add its cost.
            None => {
                self.total = self.total.saturating_add(u64::from(token_cost));
            }
        }
    }

    /// Drains all buffered mutations into `SetContextOverride` variants, resetting state.
    ///
    /// Returns one [`HistoryMutation::SetContextOverride`] per distinct buffered entry.
    /// The accumulator is empty after this call.
    pub fn drain(&mut self) -> Vec<HistoryMutation> {
        self.total = 0;
        self.entries
            .drain()
            .map(|(entry_id, acc)| HistoryMutation::SetContextOverride {
                entry_id,
                value: acc.value,
                source: acc.source,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use crate::protocol::{ChangeSource, ChatEntryId, ContextOverride};

    fn id(n: u8) -> ChatEntryId {
        // Deterministic, valid (8-4-4-4-12) UUID strings so dedup tests share stable ids.
        // n must stay 0..=9 so the final group stays exactly 12 hex chars.
        ChatEntryId::from(format!("00000000-0000-0000-0000-00000000000{n}"))
    }
    fn worker() -> ChangeSource {
        ChangeSource::Worker {
            name: "test".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn new_accumulator_is_empty_with_zero_total() {
        // Given a fresh accumulator.
        let acc = MutationAccumulator::default();

        // Then it is empty with a zero total.
        assert!(acc.is_empty());
        assert_eq!(acc.total_tokens(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn push_one_entry_reflects_its_token_cost() {
        // Given an empty accumulator.
        let mut acc = MutationAccumulator::default();

        // When pushing a 300-token exclude.
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // Then the total reflects that cost.
        assert!(!acc.is_empty());
        assert_eq!(acc.total_tokens(), 300);
    }

    #[rstest::rstest]
    #[test]
    fn re_pushing_same_entry_does_not_inflate_total() {
        // Given an accumulator with one 300-token exclude.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // When re-pushing the same entry id with a different cost.
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 500);

        // Then the total reflects only the latest cost, not both.
        assert_eq!(acc.total_tokens(), 500);
    }

    #[rstest::rstest]
    #[test]
    fn two_distinct_entries_sum_their_token_costs() {
        // Given an accumulator with one 300-token entry.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // When pushing a second distinct 700-token entry.
        acc.push(id(2), ContextOverride::ForcedExclude, worker(), 700);

        // Then the total is the sum.
        assert_eq!(acc.total_tokens(), 1000);
    }

    #[rstest::rstest]
    #[test]
    fn forced_include_displaces_buffered_forced_exclude() {
        // Given an accumulator with a ForcedExclude for entry 1.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // When pushing a ForcedInclude for the same entry.
        acc.push(id(1), ContextOverride::ForcedInclude, worker(), 300);

        // Then draining yields a ForcedInclude (include wins).
        let drained = acc.drain();
        assert_eq!(drained.len(), 1);
        assert!(matches!(
            drained.first(),
            Some(HistoryMutation::SetContextOverride {
                value: ContextOverride::ForcedInclude,
                ..
            })
        ));
    }

    #[rstest::rstest]
    #[test]
    fn forced_exclude_cannot_displace_buffered_forced_include() {
        // Given an accumulator with a ForcedInclude for entry 1.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedInclude, worker(), 300);

        // When pushing a ForcedExclude for the same entry.
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 999);

        // Then draining still yields the ForcedInclude (sticky), and the cost is unchanged.
        let drained = acc.drain();
        assert!(matches!(
            drained.first(),
            Some(HistoryMutation::SetContextOverride {
                value: ContextOverride::ForcedInclude,
                ..
            })
        ));
        assert_eq!(acc.total_tokens(), 0); // drained
    }

    #[rstest::rstest]
    #[test]
    fn shield_dominance_is_order_independent() {
        // Given include-then-exclude ordering.
        let mut a = MutationAccumulator::default();
        a.push(id(1), ContextOverride::ForcedInclude, worker(), 300);
        a.push(id(1), ContextOverride::ForcedExclude, worker(), 500);

        // And exclude-then-include ordering.
        let mut b = MutationAccumulator::default();
        b.push(id(1), ContextOverride::ForcedExclude, worker(), 500);
        b.push(id(1), ContextOverride::ForcedInclude, worker(), 300);

        // Then both converge to a ForcedInclude at cost 300.
        let da = a.drain();
        let db = b.drain();
        assert!(matches!(
            da.first(),
            Some(HistoryMutation::SetContextOverride {
                value: ContextOverride::ForcedInclude,
                ..
            })
        ));
        assert!(matches!(
            db.first(),
            Some(HistoryMutation::SetContextOverride {
                value: ContextOverride::ForcedInclude,
                ..
            })
        ));
    }

    #[rstest::rstest]
    #[test]
    fn equal_value_noop_repush_keeps_total_stable() {
        // Given an accumulator with a 300-token exclude.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // When re-pushing the same value and cost.
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);

        // Then the total is unchanged (overwrite with equal cost).
        assert_eq!(acc.total_tokens(), 300);
    }

    #[rstest::rstest]
    #[test]
    fn drain_returns_all_buffered_mutations_and_resets() {
        // Given an accumulator with two entries.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);
        acc.push(id(2), ContextOverride::ForcedExclude, worker(), 700);

        // When draining.
        let drained = acc.drain();

        // Then all mutations are returned.
        assert_eq!(drained.len(), 2);
        // And the accumulator is reset.
        assert!(acc.is_empty());
        assert_eq!(acc.total_tokens(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn drain_on_empty_returns_empty_vec() {
        // Given an empty accumulator.
        let mut acc = MutationAccumulator::default();

        // When draining.
        let drained = acc.drain();

        // Then nothing is returned.
        assert!(drained.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn is_empty_true_after_drain() {
        // Given a non-empty accumulator.
        let mut acc = MutationAccumulator::default();
        acc.push(id(1), ContextOverride::ForcedExclude, worker(), 300);
        assert!(!acc.is_empty());

        // When draining.
        acc.drain();

        // Then it is empty.
        assert!(acc.is_empty());
    }
}
