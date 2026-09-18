//! Smoke test: verify `[[auto_prune.regex.rules]]` round-trips through
//! `RegexAutoPruneConfig`.
//!
//! Pairs with the `toml_edit` round-trip smoke test that now lives in the
//! `jinn-provider-config` crate (the comment-preserving fixture moved there
//! when provider config was extracted).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    reason = "test code"
)]

use jinn_preferences_config::schemas::auto_prune::RegexAutoPruneConfig;
use serde::Deserialize;

#[rstest::rstest]
#[test]
fn auto_prune_regex_rules_round_trips_as_array_of_tables() {
    // Given a TOML snippet using the [[auto_prune.regex.rules]] form
    // (which is what the codebase documents).
    let toml_str = r#"
        [auto_prune.regex]
        enabled = true

        [[auto_prune.regex.rules]]
        pattern = "foo"
        tool_name = "bash"
        keep_last = 3

        [[auto_prune.regex.rules]]
        pattern = "bar"
    "#;
    // When deserializing through a wrapper that mirrors UserPreferences' shape.
    #[derive(Deserialize)]
    struct Wrapper {
        auto_prune: AutoPruneWrapper,
    }
    #[derive(Deserialize)]
    struct AutoPruneWrapper {
        regex: RegexAutoPruneConfig,
    }
    let parsed: Wrapper = toml::from_str(toml_str).expect("parse");
    let cfg = parsed.auto_prune.regex;

    // Then both rules are present and key fields preserved.
    assert_eq!(cfg.rules.len(), 2);
    assert_eq!(cfg.rules[0].pattern, "foo");
    assert_eq!(cfg.rules[0].keep_last, 3);
    assert_eq!(cfg.rules[1].pattern, "bar");
    assert_eq!(cfg.rules[1].tool_name, "bash"); // default applied

    // And re-serializing the bare struct round-trips through parse.
    let reserialized = toml::to_string(&cfg).expect("serialize");
    let reparsed: RegexAutoPruneConfig = toml::from_str(&reserialized).expect("reparse");
    assert_eq!(reparsed.rules.len(), 2);
}
