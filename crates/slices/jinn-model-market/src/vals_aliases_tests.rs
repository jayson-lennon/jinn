//! Alias-table tests against synthetic fixtures only.
//!
//! There is no vals counterpart to the ECI tripwire (which proves each
//! alias's target exists in a shipped seed): no vals snapshot ships in
//! the repo, so every test here builds a small `ValsIndex` by hand.

#![allow(clippy::expect_used, reason = "test code")]

use jinn_model_market_msg::ValsIndex;
use jinn_model_market_msg::ValsScore;

use crate::vals_aliases::resolve_vals;

/// A two-entry index carrying the exact slug forms two aliases point
/// at, so the alias branch can resolve end to end without any shipped
/// roster copy.
fn alias_fixture() -> ValsIndex {
    let mut index = ValsIndex::default();
    for slug in [
        "zai/glm-5.3",
        "zai/glm-5.3-flash",
        "baseten/nemotron-3.5-super-vl-120b-a12b",
    ] {
        index
            .scores
            .insert(slug.to_owned(), ValsScore { accuracy: 50.0 });
    }
    index
}

/// An index whose keys are already OpenRouter-shaped (`z-ai/glm-5.3`),
/// so the strict canonical path resolves without any alias.
fn strict_fixture() -> ValsIndex {
    let mut index = ValsIndex::default();
    for slug in ["z-ai/glm-5.3", "deepseek/deepseek-v4.1-flash-1017"] {
        index
            .scores
            .insert(slug.to_owned(), ValsScore { accuracy: 71.3 });
    }
    index
}

#[rstest::rstest]
fn strict_match_resolves_the_full_listing_id() {
    // Given an index keyed in OpenRouter's own namespace.
    let index = strict_fixture();

    // When resolving the matching listing id.
    let slug = resolve_vals("openrouter/z-ai/glm-5.3", &index);

    // Then the index key comes back verbatim.
    assert_eq!(slug, Some("z-ai/glm-5.3"));
}

#[rstest::rstest]
fn strict_match_ignores_the_variant_suffix() {
    // Given the same index.
    let index = strict_fixture();

    // When resolving a `:batch` twin of the base listing.
    let slug = resolve_vals("openrouter/deepseek/deepseek-v4.1-flash-1017:batch", &index);

    // Then the base listing's key serves the variant.
    assert_eq!(slug, Some("deepseek/deepseek-v4.1-flash-1017"));
}

#[rstest::rstest]
fn canonicalization_folds_case_and_separators() {
    // Given an index whose key differs only in case and separators.
    let index = strict_fixture();

    // When resolving a listing spelled `Z-AI/GLM-5.3`-differently.
    let slug = resolve_vals("openrouter/z-ai/glm_5.3", &index);

    // Then the canonical forms collide and the key resolves.
    assert_eq!(slug, Some("z-ai/glm-5.3"));
}

#[rstest::rstest]
fn the_alias_table_closes_a_provider_namespace_gap() {
    // Given an index keyed in Vals' namespace, with the alias target
    // present.
    let index = alias_fixture();

    // When resolving OpenRouter's own-spelling listing id.
    let slug = resolve_vals("openrouter/z-ai/glm-5.3", &index);

    // Then the alias table bridges to the Vals slug.
    assert_eq!(slug, Some("zai/glm-5.3"));
}

#[rstest::rstest]
fn an_alias_whose_target_is_absent_resolves_to_nothing() {
    // Given an index that does NOT carry the alias's Vals slug.
    let mut index = ValsIndex::default();
    index
        .scores
        .insert("zai/other-model".to_owned(), ValsScore { accuracy: 10.0 });

    // When resolving the aliased listing id.
    let slug = resolve_vals("openrouter/z-ai/glm-5.3", &index);

    // Then nothing resolves: an alias pointing at a missing index key
    // must not invent a score for the row.
    assert_eq!(slug, None);
}

#[rstest::rstest]
fn an_unmatched_listing_resolves_to_nothing() {
    // Given an index carrying none of the queried model.
    let index = alias_fixture();

    // When resolving a listing the join cannot name.
    let slug = resolve_vals("openrouter/inception/mercury-3", &index);

    // Then the row stays blank — no fuzzy fallback, ever.
    assert_eq!(slug, None);
}

#[rstest::rstest]
fn a_differing_bare_slug_does_not_match_a_differing_model() {
    // Given an index holding a *different* model that shares no slug.
    let mut index = ValsIndex::default();
    index
        .scores
        .insert("meta/muse-wonder".to_owned(), ValsScore { accuracy: 40.0 });

    // When resolving a listing whose bare slug is not `muse-wonder`.
    let slug = resolve_vals("openrouter/meta/muse-ember", &index);

    // Then nothing resolves — near-miss names never join.
    assert_eq!(slug, None);
}

#[rstest::rstest]
fn an_empty_index_resolves_to_nothing() {
    // Given an empty index (no cache, no download yet).
    let index = ValsIndex::default();

    // When resolving any listing.
    let slug = resolve_vals("openrouter/z-ai/glm-5.3", &index);

    // Then nothing resolves and nothing panics.
    assert_eq!(slug, None);
}

#[rstest::rstest]
fn every_alias_or_id_is_its_own_vals_slug_canonicalized() {
    // Given the alias table.
    // When each row is checked.
    // Then no alias maps a listing to itself through canonicalization —
    // such a row would be dead weight and hide a real spelling gap.
    for alias in crate::vals_aliases::VALS_ALIASES {
        let a = crate::eci::canonical_name(alias.or_id);
        let b = crate::eci::canonical_name(alias.vals_slug);
        assert_ne!(
            a, b,
            "alias {} → {} is a canonical no-op",
            alias.or_id, alias.vals_slug
        );
    }
}
