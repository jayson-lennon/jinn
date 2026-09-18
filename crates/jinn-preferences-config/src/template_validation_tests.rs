//! Template validation tests for the shipped `default_jinn.toml`.
//!
//! The spec lives in [`jinn_common::template_check`]: the template must
//! parse (with every marked example region expanded), carry no keys the
//! schema dropped, and document every key the schema carries — without any
//! coupling to [`UserPreferences::default()`]'s values.

#![expect(
    clippy::expect_used,
    reason = "test code asserts with expect for clear failure messages"
)]

use crate::user_preferences::tests::explicit_user_preferences;
use crate::user_preferences::{DEFAULT_CONFIG, UserPreferences};

/// The template parses as-is (what jinn loads on first run).
#[rstest::rstest]
#[test]
fn jinn_template_parses_as_user_preferences() {
    // Given the shipped template.
    // When deserializing.
    let result = toml::from_str::<UserPreferences>(DEFAULT_CONFIG);

    // Then it succeeds.
    assert!(
        result.is_ok(),
        "template does not parse: {:?}",
        result.err()
    );
}

/// The template with all marked regions expanded parses too (what a user
/// gets by uncommenting every example).
#[rstest::rstest]
#[test]
fn jinn_template_with_all_examples_activated_parses() {
    // Given the template with every marked example expanded.
    let expanded = jinn_common::template_check::expand_marked_examples(DEFAULT_CONFIG);

    // When deserializing.
    let result = toml::from_str::<UserPreferences>(&expanded);

    // Then it succeeds.
    assert!(
        result.is_ok(),
        "expanded template does not parse: {:?}\n---\n{expanded}",
        result.err()
    );
}

/// Every schema key is documented (active or marked); no dead keys.
#[rstest::rstest]
#[test]
fn jinn_template_documents_every_config_key() {
    // Given the schema fixture (every field, None included) and the template.
    let schema = serde_json::to_value(explicit_user_preferences()).expect("fixture serializes");

    // When validating the template against the schema.
    let result = jinn_common::template_check::check_template_activates_and_documents::<
        UserPreferences,
    >(DEFAULT_CONFIG, &schema);

    // Then the template is complete and alive.
    assert!(
        result.is_ok(),
        "template validation failed: {:?}",
        result.map_err(|e| jinn_common::template_check::describe(&e))
    );
}
