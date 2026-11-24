//! Observable behavior tests for the compiled-in ECI alias table and the
//! display-name join.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::collections::HashSet;

use jinn_model_market_msg::EciIndex;
use jinn_model_market_msg::EciScore;

use crate::eci::seed_data;
use crate::eci_aliases::resolve_eci;

/// The seed index, parsed once per test from the shipped bytes.
fn seed_index() -> EciIndex {
    seed_data().expect("the shipped seed parses").index
}

/// Canonicalizes the way the join does, for asserting the strict pass.
fn canonical(name: &str) -> String {
    crate::eci::canonical_name(name)
}

#[rstest::rstest]
#[test]
fn every_alias_names_a_seed_row() {
    // Given the shipped seed's display names.
    let index = seed_index();
    let seed_names: HashSet<&str> = index.scores.keys().map(String::as_str).collect();

    // When each alias is checked against those names.
    for alias in crate::eci_aliases::ECI_ALIASES {
        // Then every alias's eci_name is a row in that seed.
        // This is the drift tripwire: a seed update that renames or drops a
        // version this table aliases must fail here, not silently strand an
        // alias.
        assert!(
            seed_names.contains(alias.eci_name),
            "alias table drift: {alias:?} names no row in the shipped seed CSV",
        );
    }
}

#[rstest::rstest]
#[test]
fn resolve_eci_strict_matches_a_canonicalized_seed_name() {
    // Given the seed index.
    let index = seed_index();

    // When resolving an OpenRouter id whose slug canonicalizes to a seed
    // name (`GPT-4o mini` → `gpt-4o-mini`).
    let resolved = resolve_eci("openai/gpt-4o-mini", &index);

    // Then the seed's own spelling comes back.
    assert_eq!(resolved, Some("GPT-4o mini"));
}

#[rstest::rstest]
#[test]
fn resolve_eci_alias_matches_a_dated_snapshot_slug() {
    // Given the seed index.
    let index = seed_index();

    // When resolving a dated snapshot slug that only the alias table can
    // name.
    let resolved = resolve_eci("openai/gpt-4o-2024-11-20", &index);

    // Then the aliased ECI display name comes back.
    assert_eq!(resolved, Some("GPT-4o (Nov 2024)"));
}

#[rstest::rstest]
#[test]
fn resolve_eci_strips_the_variant_suffix() {
    // Given the seed index and an alias for a base listing.
    let index = seed_index();

    // When resolving the `:free` variant of an aliased slug.
    let resolved = resolve_eci("openai/gpt-4o-2024-11-20:free", &index);

    // Then the base listing's ECI name serves the variant.
    assert_eq!(resolved, Some("GPT-4o (Nov 2024)"));
}

#[rstest::rstest]
#[test]
fn resolve_eci_returns_none_for_an_unknown_slug() {
    // Given the seed index.
    let index = seed_index();

    // When resolving a slug neither strict matching nor the table names.
    let resolved = resolve_eci("openai/no-such-model-slug", &index);

    // Then nothing resolves (the row shows the empty-cell placeholder
    // in its score columns, and no flag).
    assert_eq!(resolved, None);
}

#[rstest::rstest]
#[test]
fn strict_match_wins_over_the_alias_table() {
    // Given an index whose canonical key collides with an alias slug but
    // under a different name.
    let mut index = EciIndex::default();
    index.scores.insert(
        "gpt 4o 2024 11 20".to_owned(),
        EciScore {
            eci: 100.0,
            ci_low: 90.0,
            ci_high: 110.0,
        },
    );

    // When resolving the dated snapshot slug.
    let resolved = resolve_eci("openai/gpt-4o-2024-11-20", &index);

    // Then the index's own name wins (strict pass first) — a downloaded
    // seed that renames a version is authoritative over the compiled
    // table.
    assert_eq!(resolved, Some("gpt 4o 2024 11 20"));
}

#[rstest::rstest]
#[test]
fn canonical_name_folds_separators_and_case() {
    // Given spellings of one name across the two vocabularies.
    let a = canonical("GLM-5.3 Flash");
    let b = canonical("glm_5.3_flash");

    // When canonicalizing both.
    // Then they collide, so the strict pass sees one key.
    assert_eq!(a, b);
}

#[rstest::rstest]
#[test]
fn canonical_name_keeps_dated_snapshots_distinct() {
    // Given two dated snapshots of the same model.
    let a = canonical("Gemini 2.5 Pro (Mar 2025)");
    let b = canonical("Gemini 2.5 Pro (Jun 2025)");

    // When their canonical forms are compared.
    // Then they differ — parentheses survive.
    assert_ne!(a, b);
}
