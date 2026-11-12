//! Cross-section configuration checks — rules spanning several
//! `jinn.toml` sections at once.
//!
//! A [`jinn_config::Configurable`] section check validates one table in
//! isolation. Some configurations are only wrong as a *whole*: the
//! values agree section-by-section yet the relationship between them
//! silently disables a feature. Those rules live here, registered as
//! raw checks on the configuration layer ([`jinn_config::ConfigLayer::register_raw_check`])
//! so they run at launch validation and on every validated live reload,
//! in registration order, beside the per-section checks.
//!
//! Kernel-free, like the rest of this crate: a check reads only the
//! section schemas beside it.

use std::sync::Arc;

use jinn_config::{ConfigLayer, ConfigSectionError, Configurable as _, SectionCheck};

use crate::schemas::{AutoPruneConfig, HandoffCompactionConfig};

/// The umbrella table both checked sections live under.
const CONTEXT_CURATION_KEY: &str = "context_curation";

/// Fails when enabled handoff compaction fires at or below the pruner
/// accumulation threshold — the configuration that silently disables
/// every auto-pruner.
///
/// Handoff compaction folds the context at every model-request boundary
/// once its accumulated work reaches the trigger; the pruners' buffered
/// mutations flush only when the accumulation threshold is crossed.
/// A trigger at or below the threshold means the fold always wins the
/// race, so the pruners never accumulate enough to flush and their
/// prunes never reach the model.
///
/// Per the schema's own contract, the check applies **only when the
/// handoff is enabled and a trigger is set** —
/// [`HandoffCompactionConfig::resolve_trigger`] returning `Some` is the
/// whole precondition. A disabled or unconfigured handoff is inert: it
/// races nothing, so any threshold is fine.
///
/// # Errors
///
/// Returns [`ConfigSectionError::Invalid`] carrying the guidance
/// message when the trigger meets or passes the threshold.
pub fn check_pruner_headroom(
    handoff: &HandoffCompactionConfig,
    accumulation_threshold_tokens: u32,
) -> Result<(), ConfigSectionError> {
    let Some(trigger) = handoff.resolve_trigger() else {
        return Ok(());
    };
    if trigger <= accumulation_threshold_tokens {
        return Err(ConfigSectionError::Invalid {
            key: HandoffCompactionConfig::KEY,
            detail: format!(
                "context pruners are disabled if handoff compaction trigger is less than \
                 accumulation threshold (workset_trigger_tokens = {trigger}, \
                 accumulation_threshold_tokens = {accumulation_threshold_tokens})"
            ),
        });
    }
    Ok(())
}

/// The raw check registered on the layer: reads both sections out of the
/// `context_curation` umbrella table, absent children defaulting.
///
/// Registered after [`AutoPruneConfig`] in
/// [`crate::registration::register_all_sections`], so a malformed
/// `auto_prune` table is reported by its own per-section check before
/// this cross-check runs.
#[must_use]
pub fn pruner_headroom_check() -> SectionCheck {
    Arc::new(|table: &toml::Table| {
        let handoff = match child_section(table, HandoffCompactionConfig::KEY)? {
            Some(child) => HandoffCompactionConfig::from_table(&child)?,
            None => HandoffCompactionConfig::default(),
        };
        let auto_prune = match child_section(table, AutoPruneConfig::KEY)? {
            Some(child) => AutoPruneConfig::from_table(&child)?,
            None => AutoPruneConfig::default(),
        };
        check_pruner_headroom(&handoff, auto_prune.accumulation_threshold_tokens)
    })
}

/// Registers [`pruner_headroom_check`] on the layer under the
/// `context_curation` umbrella.
///
/// The one call a composition makes to opt into the cross-section rules
/// here; [`crate::registration::register_all_sections`] invokes it after
/// the per-section roster.
pub fn register_cross_checks(config: &ConfigLayer) {
    config.register_raw_check(CONTEXT_CURATION_KEY, pruner_headroom_check());
}

/// Resolves a child section's table out of its umbrella, `None` when
/// absent at any depth.
///
/// `key` is the child's full dotted key; only its leaf segment is walked
/// here, since the umbrella table is already in hand.
fn child_section(
    umbrella: &toml::Table,
    key: &'static str,
) -> Result<Option<toml::Table>, ConfigSectionError> {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let Some(item) = umbrella.get(leaf) else {
        return Ok(None);
    };
    let Some(table) = item.as_table() else {
        return Err(ConfigSectionError::NotATable {
            key,
            segment: leaf.to_owned(),
        });
    };
    Ok(Some(table.clone()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::*;

    fn handoff(enabled: bool, trigger: Option<u32>) -> HandoffCompactionConfig {
        HandoffCompactionConfig {
            enabled,
            workset_trigger_tokens: trigger
                .map(|tokens| usize::try_from(tokens).unwrap_or(usize::MAX)),
        }
    }

    #[rstest::rstest]
    #[test]
    fn check_passes_when_the_handoff_is_disabled() {
        // Given a disabled handoff with a trigger that would violate.
        let config = handoff(false, Some(100_000));

        // When checking headroom against the default threshold.
        let result = check_pruner_headroom(&config, 150_000);

        // Then the check passes — a disabled handoff is inert.
        assert!(result.is_ok(), "a disabled handoff races nothing");
    }

    #[rstest::rstest]
    #[test]
    fn check_passes_when_no_trigger_is_set() {
        // Given an enabled handoff with no trigger.
        let config = handoff(true, None);

        // When checking headroom against any threshold.
        let result = check_pruner_headroom(&config, 150_000);

        // Then the check passes — the gate never fires unbounded.
        assert!(result.is_ok(), "an unset trigger never fires");
    }

    #[rstest::rstest]
    #[test]
    fn check_rejects_a_trigger_below_the_threshold() {
        // Given an enabled handoff with a trigger under the threshold.
        let config = handoff(true, Some(100_000));
        let threshold = crate::schemas::auto_prune::DEFAULT_ACCUMULATION_THRESHOLD_TOKENS;

        // When checking headroom.
        let result = check_pruner_headroom(&config, threshold);

        // Then the check fails carrying the guidance message.
        let Err(error) = result else {
            panic!("expected the cross-section check to reject");
        };
        let ConfigSectionError::Invalid { detail, .. } = error else {
            panic!("expected Invalid, got {error:?}");
        };
        assert!(
            detail.contains("context pruners are disabled"),
            "the guidance message must name the consequence: {detail}"
        );
        assert!(
            detail.contains("100000") && detail.contains("150000"),
            "the message must name both values: {detail}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn check_rejects_a_trigger_equal_to_the_threshold() {
        // Given an enabled handoff whose trigger equals the threshold.
        let config = handoff(true, Some(150_000));

        // When checking headroom against that same threshold.
        let result = check_pruner_headroom(&config, 150_000);

        // Then the check fails — equal values still starve the pruners.
        assert!(result.is_err(), "an equal trigger is still a violation");
    }

    #[rstest::rstest]
    #[test]
    fn check_passes_when_the_trigger_exceeds_the_threshold() {
        // Given an enabled handoff with a trigger above the threshold.
        let config = handoff(true, Some(160_000));

        // When checking headroom against the default threshold.
        let result = check_pruner_headroom(&config, 150_000);

        // Then the check passes — the pruners get headroom to flush.
        assert!(result.is_ok(), "headroom above the threshold is healthy");
    }

    fn umbrella(body: &str) -> toml::Table {
        let doc: toml::Table = toml::from_str(body).expect("test TOML parses");
        doc.get("context_curation")
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default()
    }

    #[rstest::rstest]
    #[test]
    fn raw_check_rejects_an_enabled_handoff_below_an_absent_threshold() {
        // Given an umbrella carrying only the handoff child — the absent
        // auto_prune child reads its default threshold.
        let table = umbrella(
            "[context_curation.handoff_compaction]\nenabled = true\nworkset_trigger_tokens = 100000\n",
        );

        // When the raw check runs on it.
        let result = pruner_headroom_check()(&table);

        // Then the default 150 000 threshold still rejects the trigger.
        assert!(
            matches!(result, Err(ConfigSectionError::Invalid { .. })),
            "absent auto_prune must default its threshold into the check"
        );
    }

    #[rstest::rstest]
    #[test]
    fn raw_check_passes_an_empty_umbrella() {
        // Given an umbrella table with neither child present.
        let table = umbrella("[context_curation]\nother_key = 1\n");

        // When the raw check runs on it.
        let result = pruner_headroom_check()(&table);

        // Then both children default and the defaults agree.
        assert!(result.is_ok(), "defaults are consistent by construction");
    }

    #[rstest::rstest]
    #[test]
    fn raw_check_reads_a_written_threshold() {
        // Given an umbrella whose auto_prune child raises the threshold to
        // the handoff trigger — a violation the default threshold alone
        // would not produce.
        let table = umbrella(
            "[context_curation.handoff_compaction]\nenabled = true\nworkset_trigger_tokens = 100000\n\n[context_curation.auto_prune]\naccumulation_threshold_tokens = 100000\n",
        );

        // When the raw check runs on it.
        let result = pruner_headroom_check()(&table);

        // Then the written threshold is what the check compares.
        assert!(
            matches!(result, Err(ConfigSectionError::Invalid { .. })),
            "the written threshold must participate in the check"
        );
    }

    #[rstest::rstest]
    #[test]
    fn raw_check_passes_a_threshold_below_the_trigger() {
        // Given an umbrella whose threshold sits below the trigger.
        let table = umbrella(
            "[context_curation.handoff_compaction]\nenabled = true\nworkset_trigger_tokens = 100000\n\n[context_curation.auto_prune]\naccumulation_threshold_tokens = 90000\n",
        );

        // When the raw check runs on it.
        let result = pruner_headroom_check()(&table);

        // Then the configuration as a whole validates.
        assert!(result.is_ok(), "headroom below the trigger is healthy");
    }
}
