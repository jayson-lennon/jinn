//! `HandoffCompactionConfig` tests — defaults, regime resolution, and
//! TOML shape.

#![allow(clippy::expect_used, reason = "test code")]

use super::HandoffCompactionConfig;

#[rstest::rstest]
#[test]
fn default_disables_the_feature_and_keeps_inert_defaults() {
    // Given the default config.
    let config = HandoffCompactionConfig::default();

    // Then the feature is disabled with no firing regime.
    assert!(!config.enabled);
    assert_eq!(config.context_limit, None);
    assert_eq!(config.trigger_tokens, None);
    assert_eq!(config.resolve_regime(Some(100_000)), None);
}

#[rstest::rstest]
#[test]
fn regime_resolves_from_explicit_limit_and_trigger() {
    // Given a config with an explicit limit and an in-range trigger.
    let config = HandoffCompactionConfig {
        enabled: true,
        context_limit: Some(32_000),
        trigger_tokens: Some(24_000),
    };

    // When resolving the regime without a model-reported limit.
    let regime = config.resolve_regime(None);

    // Then the explicit values win.
    assert_eq!(regime, Some((32_000, 24_000)));
}

#[rstest::rstest]
#[test]
fn regime_falls_back_to_the_model_reported_limit() {
    // Given a config with a trigger but no explicit limit.
    let config = HandoffCompactionConfig {
        enabled: true,
        context_limit: None,
        trigger_tokens: Some(12_000),
    };

    // When resolving against a model-reported limit.
    let regime = config.resolve_regime(Some(16_000));

    // Then the model's reported length is the effective limit.
    assert_eq!(regime, Some((16_000, 12_000)));
}

#[rstest::rstest]
#[test]
fn regime_needs_a_limit_source() {
    // Given a config with a trigger but neither an explicit limit nor a
    // model-reported one.
    let config = HandoffCompactionConfig {
        enabled: true,
        context_limit: None,
        trigger_tokens: Some(12_000),
    };

    // When resolving the regime.
    let regime = config.resolve_regime(None);

    // Then nothing resolves — the gate never fires unbounded.
    assert_eq!(regime, None);
}

#[rstest::rstest]
#[test]
fn regime_rejects_a_trigger_at_or_beyond_the_limit() {
    // Given configs whose trigger equals or exceeds the limit.
    let at = HandoffCompactionConfig {
        enabled: true,
        context_limit: Some(16_000),
        trigger_tokens: Some(16_000),
    };
    let beyond = HandoffCompactionConfig {
        enabled: true,
        context_limit: Some(16_000),
        trigger_tokens: Some(20_000),
    };

    // When resolving both regimes.
    // Then neither resolves — the gap must reserve the recap's room.
    assert_eq!(at.resolve_regime(None), None);
    assert_eq!(beyond.resolve_regime(None), None);
}

#[rstest::rstest]
#[test]
fn regime_rejects_a_missing_trigger() {
    // Given an enabled config with a limit but no trigger.
    let config = HandoffCompactionConfig {
        enabled: true,
        context_limit: Some(16_000),
        trigger_tokens: None,
    };

    // When resolving the regime.
    // Then nothing resolves — an unconfigured fire line never fires.
    assert_eq!(config.resolve_regime(None), None);
}

#[rstest::rstest]
#[test]
fn section_roundtrips_through_its_documented_key() {
    // Given a TOML table under the section's documented key.
    let raw = r#"
        [context_curation.handoff_compaction]
        enabled = true
        context_limit = 32000
        trigger_tokens = 24000
    "#;

    // When reading it through the configuration layer.
    let config = jinn_config::testutil::config_layer(raw);
    let read: HandoffCompactionConfig = config.get().expect("section reads");

    // Then every field survives the read.
    assert!(read.enabled);
    assert_eq!(read.context_limit, Some(32_000));
    assert_eq!(read.trigger_tokens, Some(24_000));
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
