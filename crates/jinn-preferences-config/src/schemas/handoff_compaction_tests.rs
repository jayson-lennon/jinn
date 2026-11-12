//! `HandoffCompactionConfig` tests — defaults, trigger resolution, and
//! TOML shape.

#![allow(clippy::expect_used, reason = "test code")]

use super::HandoffCompactionConfig;

#[rstest::rstest]
#[test]
fn default_disables_the_feature_and_keeps_inert_defaults() {
    // Given the default config.
    let config = HandoffCompactionConfig::default();

    // Then the feature is disabled with no fire line.
    assert!(!config.enabled);
    assert_eq!(config.workset_trigger_tokens, None);
    assert_eq!(config.resolve_trigger(), None);
}

#[rstest::rstest]
#[test]
fn trigger_resolves_when_enabled_and_set() {
    // Given an enabled config with a work-token fire line.
    let config = HandoffCompactionConfig {
        enabled: true,
        workset_trigger_tokens: Some(50_000),
    };

    // When resolving the trigger.
    // Then the fire line resolves.
    assert_eq!(config.resolve_trigger(), Some(50_000));
}

#[rstest::rstest]
#[test]
fn trigger_needs_the_feature_enabled() {
    // Given a config with a trigger but the feature disabled.
    let config = HandoffCompactionConfig {
        enabled: false,
        workset_trigger_tokens: Some(50_000),
    };

    // When resolving the trigger.
    // Then nothing resolves.
    assert_eq!(config.resolve_trigger(), None);
}

#[rstest::rstest]
#[test]
fn trigger_needs_a_value() {
    // Given an enabled config with no fire line set.
    let config = HandoffCompactionConfig {
        enabled: true,
        workset_trigger_tokens: None,
    };

    // When resolving the trigger.
    // Then nothing resolves — an unconfigured fire line never fires.
    assert_eq!(config.resolve_trigger(), None);
}

#[rstest::rstest]
#[test]
fn legacy_keys_are_ignored() {
    // Given a config document carrying only the pre-rename keys.
    let raw = r#"
        [context_curation.handoff_compaction]
        enabled = true
        context_limit = 32000
        trigger_tokens = 24000
    "#;

    // When reading it through the configuration layer.
    let config = jinn_config::testutil::config_layer(raw);
    let read: HandoffCompactionConfig = config.get().expect("section reads");

    // Then the section reads enabled but fire-less: serde drops the
    // unknown legacy keys and no work trigger exists, so the gate
    // passes until the new key is set.
    assert!(read.enabled);
    assert_eq!(read.workset_trigger_tokens, None);
    assert_eq!(read.resolve_trigger(), None);
}

#[rstest::rstest]
#[test]
fn section_roundtrips_through_its_documented_key() {
    // Given a TOML table under the section's documented key.
    let raw = r#"
        [context_curation.handoff_compaction]
        enabled = true
        workset_trigger_tokens = 50000
    "#;

    // When reading it through the configuration layer.
    let config = jinn_config::testutil::config_layer(raw);
    let read: HandoffCompactionConfig = config.get().expect("section reads");

    // Then every field survives the read.
    assert!(read.enabled);
    assert_eq!(read.workset_trigger_tokens, Some(50_000));
}

#[rstest::rstest]
#[test]
fn absent_section_reads_as_disabled_defaults() {
    // Given a layer with no handoff section at all.
    let config = jinn_config::testutil::config_layer("");

    // When reading the section.
    let read: HandoffCompactionConfig = config.get().expect("absent section reads");

    // Then it reads as the inert default.
    assert_eq!(read, HandoffCompactionConfig::default());
}
