//! Row building: raw OpenRouter listings joined into the
//! [`MarketRow`]s the table renders.
//!
//! Three concerns, one pass:
//!
//! - **Decision F** — a listing that publishes no `input_cache_read`
//!   carries `cache_read_price: None`; the projection costs cache reads
//!   at the prompt price and the CACHE column shows `—`. The *column*
//!   rule lives in the view; the `None` is decided here.
//! - **Flags** — `f` (`:free`) is the id's variant suffix; `†`
//!   (overrides) is the presence of a nonempty override list; `#`
//!   (no ECI) falls out of the join.
//! - **The score joins** — each routed through its single canonicalizing
//!   resolver: ECI through [`crate::eci_aliases::resolve_eci`], vals
//!   through [`crate::vals_aliases::resolve_vals`]. Both fold case and
//!   separators on *both* sides, strict-match against the index's raw
//!   keys, then consult their compiled-in alias table. This module
//!   deliberately carries **no** competing normalization of its own —
//!   two canonical forms is how a join silently dies.
//!
//! The id's `openrouter/` prefix is prepended here, per the `-msg`
//! crate's `MarketRow.id` docs: ENTER publishes exactly that string as
//! the `ProviderSwitch` selection.

use jinn_model_market_msg::EciIndex;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::ValsIndex;

use crate::or_models::RawModel;

/// Builds the market rows from the parsed catalog, joined with the ECI
/// index and the Vals Index when either is present.
///
/// Text-output gating (`or_models::is_text_output`) has already run or
/// runs here — this function applies it, so a caller handing it the raw
/// catalog gets exactly the popup's row set, in catalog order (the
/// render pass sorts; the fetch order is the stable base).
#[must_use]
pub fn build_rows(
    models: &[RawModel],
    eci: Option<&EciIndex>,
    vals: Option<&ValsIndex>,
) -> Vec<MarketRow> {
    models
        .iter()
        .filter(|m| crate::or_models::is_text_output(m))
        .map(|model| build_row(model, eci, vals))
        .collect()
}

/// Builds one row.
fn build_row(model: &RawModel, eci: Option<&EciIndex>, vals: Option<&ValsIndex>) -> MarketRow {
    let pricing = model.pricing.as_ref();
    let prompt_price = crate::or_models::parse_price(pricing.and_then(|p| p.prompt.as_deref()))
        .unwrap_or_default();
    let completion_price =
        crate::or_models::parse_price(pricing.and_then(|p| p.completion.as_deref()))
            .unwrap_or_default();
    let cache_read_price =
        crate::or_models::parse_price(pricing.and_then(|p| p.input_cache_read.as_deref()));
    let overrides = pricing.map_or_else(Vec::new, |p| p.overrides.clone());

    let variant = listing_variant(&model.id);
    let is_free = variant.as_deref() == Some("free");
    let eci_score = eci.and_then(|index| {
        let name = crate::eci_aliases::resolve_eci(&model.id, index)?;
        index.get(name).cloned()
    });
    let vals_score = vals.and_then(|index| {
        let slug = crate::vals_aliases::resolve_vals(&model.id, index)?;
        index.get(slug).cloned()
    });

    MarketRow {
        id: full_row_id(&model.id),
        author: listing_author(&model.id),
        slug: listing_slug(&model.id),
        variant,
        prompt_price,
        completion_price,
        // Decision F: no published cache price stays None — the prompt
        // price stands in at projection time, never at row-build time.
        cache_read_price,
        has_overrides: !overrides.is_empty(),
        context_length: model.context_length,
        eci: eci_score.as_ref().map(|s| s.eci),
        eci_ci: eci_score.map(|s| (s.ci_low, s.ci_high)),
        vals: vals_score.map(|s| s.accuracy),
        is_free,
    }
}

/// The row's full selection id: `openrouter/{author}/{slug}[:variant]`.
fn full_row_id(listing_id: &str) -> String {
    format!("openrouter/{listing_id}")
}

/// The listing's author — the namespace before the first `/`.
fn listing_author(listing_id: &str) -> String {
    listing_id
        .split_once('/')
        .map_or(listing_id, |(author, _)| author)
        .to_owned()
}

/// The listing's slug — the segment after the first `/`, variant
/// suffix stripped (`claude-sonnet-4.5:free` → `claude-sonnet-4.5`).
fn listing_slug(listing_id: &str) -> String {
    let after_slash = listing_id
        .split_once('/')
        .map_or(listing_id, |(_, rest)| rest);
    match after_slash.split_once(':') {
        Some((slug, _)) => slug.to_owned(),
        None => after_slash.to_owned(),
    }
}

/// The listing's variant suffix, without the colon (`free`, `thinking`);
/// `None` for a plain listing.
fn listing_variant(listing_id: &str) -> Option<String> {
    listing_id
        .split_once(':')
        .map(|(_, variant)| variant.to_owned())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod rows_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_in_result,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use crate::eci::SEED_ECI_SCORES;
    use crate::eci::parse_eci_scores;

    /// Parses one listing fixture (single-entry `data` array).
    #[expect(
        clippy::needless_pass_by_value,
        reason = "test fixtures hand over an owned serde_json::Value"
    )]
    fn one_listing(json: serde_json::Value) -> RawModel {
        let data = serde_json::json!([json]);
        let models: Vec<RawModel> = serde_json::from_value(data).expect("fixture parses");
        models.into_iter().next().expect("one listing")
    }

    /// The REAL seed index, parsed by T2's own parser — the same bytes
    /// and the same key form the score actor puts in the data cell.
    fn seed_index() -> EciIndex {
        let (index, total) = parse_eci_scores(SEED_ECI_SCORES).expect("seed parses");
        assert!(total > 0, "the shipped seed carries rows");
        index
    }

    #[rstest::rstest]
    fn build_row_carries_the_openrouter_prefix_and_parts() {
        // Given a plain listing.
        let model = one_listing(serde_json::json!({
            "id": "anthropic/claude-sonnet-4.5",
            "pricing": {"prompt": "0.000003", "completion": "0.000015"}
        }));

        // When building its row with no score data.
        let row = build_row(&model, None, None);

        // Then the id is the publishable selection string and the parts
        // are split without the variant.
        assert_eq!(row.id, "openrouter/anthropic/claude-sonnet-4.5");
        assert_eq!(row.author, "anthropic");
        assert_eq!(row.slug, "claude-sonnet-4.5");
        assert_eq!(row.variant, None);
    }

    #[rstest::rstest]
    fn build_row_splits_the_variant_suffix() {
        // Given a free-variant listing.
        let model = one_listing(serde_json::json!({
            "id": "deepseek/deepseek-r1:free",
            "pricing": {"prompt": "0", "completion": "0"}
        }));

        // When building its row.
        let row = build_row(&model, None, None);

        // Then the variant is split off, the slug is bare, and the free
        // flag rides the suffix.
        assert_eq!(row.id, "openrouter/deepseek/deepseek-r1:free");
        assert_eq!(row.slug, "deepseek-r1");
        assert_eq!(row.variant.as_deref(), Some("free"));
        assert!(row.is_free);
    }

    #[rstest::rstest]
    fn build_row_keeps_cache_price_none_when_unpublished() {
        // Given a listing publishing no input_cache_read (decision F).
        let model = one_listing(serde_json::json!({
            "id": "a/m",
            "pricing": {"prompt": "0.000003", "completion": "0.000015"}
        }));

        // When building its row.
        let row = build_row(&model, None, None);

        // Then the cache price is None and no override flag is set.
        assert_eq!(row.cache_read_price, None);
        assert!(!row.has_overrides);
    }

    #[rstest::rstest]
    fn build_row_flags_overrides_when_present() {
        // Given a listing with a >200k pricing tier.
        let model = one_listing(serde_json::json!({
            "id": "anthropic/claude-sonnet-4.5",
            "pricing": {
                "prompt": "0.000003",
                "completion": "0.000015",
                "input_cache_read": "0.0000003",
                "overrides": [
                    {"min_prompt_tokens": 200_000, "prompt": "0.000006"}
                ]
            }
        }));

        // When building its row.
        let row = build_row(&model, None, None);

        // Then the override flag is set and the cache price parsed.
        assert!(row.has_overrides);
        assert_eq!(row.cache_read_price, Some(3e-7));
    }

    #[rstest::rstest]
    fn build_rows_filters_non_text_output() {
        // Given a catalog with a text listing and an audio (ASR) one.
        let models: Vec<RawModel> = {
            let wrapped = serde_json::json!({
                "data": [
                    {"id": "a/text", "architecture": {"output_modalities": ["text"]}},
                    {"id": "a/audio", "architecture": {"output_modalities": ["audio"]}}
                ]
            });
            serde_json::from_value::<Vec<RawModel>>(wrapped["data"].clone())
                .expect("fixture parses")
        };

        // When building rows.
        let rows = build_rows(&models, None, None);

        // Then only the text-output listing is present.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "openrouter/a/text");
    }

    #[rstest::rstest]
    fn build_row_joins_eci_against_the_real_seed() {
        // Given the REAL seed index — T2's parser over the shipped
        // eci_scores.csv bytes, keys in raw display-name form — and an
        // OR listing whose slug differs from the display name only by
        // case and the dot-vs-space separator.
        let index = seed_index();
        let model = one_listing(serde_json::json!({
            "id": "anthropic/claude-sonnet-4.5"
        }));

        // When building the row with the index.
        let row = build_row(&model, Some(&index), None);

        // Then the join resolves: the raw display-name key matches, and
        // the score rides with its interval.
        assert!(
            row.eci.is_some(),
            "claude-sonnet-4.5 must join the seed; key form is the contract"
        );
        assert!(row.eci_ci.is_some());
    }

    #[rstest::rstest]
    fn build_row_joins_a_dated_slug_through_the_alias_table() {
        // Given a dated OR slug whose Epoch spelling is parenthesized —
        // the alias-table case (e.g. gpt-4o-2024-11-20 → "GPT-4o (Nov
        // 2024)"), resolved only by T2's table, not strict matching.
        let index = seed_index();
        let model = one_listing(serde_json::json!({
            "id": "openai/gpt-4o-2024-11-20"
        }));

        // When building the row with the index.
        let row = build_row(&model, Some(&index), None);

        // Then the alias closes the join.
        assert!(
            row.eci.is_some(),
            "dated GPT snapshots must resolve through the compiled alias table"
        );
    }

    #[rstest::rstest]
    fn build_row_leaves_eci_none_when_unmatched() {
        // Given the seed index and a listing no key or alias can name.
        let index = seed_index();
        let model = one_listing(serde_json::json!({
            "id": "mystery/model-that-never-existed:thinking"
        }));

        // When building the row.
        let row = build_row(&model, Some(&index), None);

        // Then the row keeps None — the `#` flag and `—` column.
        assert_eq!(row.eci, None);
        assert_eq!(row.eci_ci, None);
    }

    #[rstest::rstest]
    fn build_row_joins_vals_against_a_synthetic_index() {
        // Given a synthetic vals index keyed in OpenRouter's own
        // namespace (no shipped roster copy — the alias tests carry the
        // alias path) and a matching listing.
        let mut vals = ValsIndex::default();
        vals.scores.insert(
            "z-ai/glm-5.3".to_owned(),
            jinn_model_market_msg::ValsScore { accuracy: 58.4 },
        );
        let model = one_listing(serde_json::json!({
            "id": "z-ai/glm-5.3"
        }));

        // When building the row with the vals index.
        let row = build_row(&model, None, Some(&vals));

        // Then the accuracy rides the row's vals field.
        assert_eq!(row.vals, Some(58.4));
    }

    #[rstest::rstest]
    fn build_row_leaves_vals_none_when_unmatched() {
        // Given an empty vals index.
        let vals = ValsIndex::default();
        let model = one_listing(serde_json::json!({
            "id": "z-ai/glm-5.3"
        }));

        // When building the row.
        let row = build_row(&model, None, Some(&vals));

        // Then the vals field stays None — blank cell, `#` flag.
        assert_eq!(row.vals, None);
    }

    #[rstest::rstest]
    fn build_rows_tolerates_listings_without_pricing() {
        // Given a listing with no pricing object at all.
        let model = one_listing(serde_json::json!({"id": "a/unpriced"}));

        // When building its row.
        let row = build_row(&model, None, None);

        // Then prices default to zero and the row still exists.
        assert_eq!(row.prompt_price, 0.0);
        assert_eq!(row.completion_price, 0.0);
        assert_eq!(row.cache_read_price, None);
    }

    #[rstest::rstest]
    fn build_row_sets_the_override_flag_from_a_nonempty_override_list() {
        // Given a listing with a >200k pricing tier.
        let model = one_listing(serde_json::json!({
            "id": "anthropic/claude-sonnet-4.5",
            "pricing": {
                "prompt": "0.000003",
                "completion": "0.000015",
                "overrides": [{"min_prompt_tokens": 200_000, "prompt": "0.000006"}]
            }
        }));

        // When building its row.
        let row = build_row(&model, None, None);

        // Then the `†` flag rides the override list's presence.
        assert!(row.has_overrides);
    }
}
