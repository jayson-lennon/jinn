//! `HandoffCompactionConfig` tests — defaults, floor, and TOML shape.

use super::HandoffCompactionConfig;

#[rstest::rstest]
#[test]
fn default_disables_the_feature_and_keeps_inert_defaults() {
    // Given the default config.
    let config = HandoffCompactionConfig::default();

    // Then the feature is disabled with the documented defaults.
    assert!(!config.enabled);
    assert_eq!(config.threshold, 0.6);
    assert_eq!(config.context_limit, None);
}

#[rstest::rstest]
#[test]
fn effective_threshold_clamps_below_the_floor() {
    // Given a config whose threshold was edited below the floor.
    let config = HandoffCompactionConfig {
        threshold: 0.0,
        ..HandoffCompactionConfig::default()
    };

    // When reading the effective threshold.
    let threshold = config.effective_threshold();

    // Then the floor applies.
    assert_eq!(threshold, HandoffCompactionConfig::threshold_floor());
}

#[rstest::rstest]
#[test]
fn effective_threshold_keeps_an_in_range_value() {
    // Given a config with an in-range threshold.
    let config = HandoffCompactionConfig {
        threshold: 0.75,
        ..HandoffCompactionConfig::default()
    };

    // When reading the effective threshold.
    let threshold = config.effective_threshold();

    // Then the value passes through unchanged.
    assert_eq!(threshold, 0.75);
}

#[rstest::rstest]
#[test]
fn section_roundtrips_through_its_documented_key() {
    // Given a TOML table under the section's documented key.
    let raw = r#"
        [context_curation.handoff_compaction]
        enabled = true
        threshold = 0.55
        context_limit = 32000
    "#;

    // When reading it through the configuration layer.
    let config = jinn_config::testutil::config_layer(raw);
    let read: HandoffCompactionConfig = config.get().expect("section reads");

    // Then every field survives the read.
    assert!(read.enabled);
    assert_eq!(read.threshold, 0.55);
    assert_eq!(read.context_limit, Some(32_000));
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
