//! The projection's tests: the filter, the sort, the formatting, and the
//! page URL — the behaviours both surfaces build on.

#![expect(
    clippy::expect_used,
    reason = "test module, a failed wire invariant is a test failure"
)]
#![allow(clippy::indexing_slicing, reason = "test code")]

use super::*;
use crate::data_state::MarketDetail;
use crate::row::MarketEndpointRow;
use crate::ui_state::{DetailSort, PROJECTION_MIX, SortColumn};

/// A `MarketRow` priced for the projection tests: everything else
/// defaults away (empty ids, no variant, no ECI).
fn priced_row(prompt: f64, completion: f64, cache: Option<f64>) -> MarketRow {
    MarketRow {
        id: "openrouter/a/model".to_owned(),
        author: "a".to_owned(),
        slug: "model".to_owned(),
        variant: None,
        prompt_price: prompt,
        completion_price: completion,
        cache_read_price: cache,
        has_overrides: false,
        context_length: None,
        eci: None,
        eci_ci: None,
        vals: None,
        is_free: false,
    }
}

/// A `MarketRow` named `author/slug` — only the name matters to the
/// filter.
fn named_row(author: &str, slug: &str) -> MarketRow {
    MarketRow {
        id: format!("openrouter/{author}/{slug}"),
        author: author.to_owned(),
        slug: slug.to_owned(),
        variant: None,
        prompt_price: 1e-6,
        completion_price: 2e-6,
        cache_read_price: None,
        has_overrides: false,
        context_length: None,
        eci: None,
        eci_ci: None,
        vals: None,
        is_free: false,
    }
}

/// Three named rows — the multi-term and single-term filter shapes.
fn fixture() -> Vec<MarketRow> {
    vec![
        named_row("anthropic", "claude-3-5-latest"),
        named_row("mistral", "small"),
        named_row("a", "tiny"),
    ]
}

fn names_matching(filter: &str) -> Vec<String> {
    let mut names: Vec<String> =
        filter_and_sort(&fixture(), filter, SortColumn::Proj, false, PROJECTION_MIX)
            .iter()
            .map(row_name)
            .collect();
    names.sort();
    names
}

// ── Projected cost ───────────────────────────────────────────────────

#[rstest::rstest]
fn projected_cents_prices_cache_reads_at_the_cache_price() {
    // Given a row publishing a cache-read price.
    let row = priced_row(1e-6, 2e-6, Some(1e-7));

    // When projecting under the first preset (80k in, 99% cache hit,
    // 1.5k out).
    let cents = projected_cents(&row, (80_000, 0.99, 1_500));

    // Then the projection is
    // 80k·(0.99·cache + 0.01·prompt) + 1.5k·completion, ×100 cents.
    let expected = (80_000.0 * (0.99 * 1e-7 + 0.01 * 1e-6) + 1_500.0 * 2e-6) * 100.0;
    assert!((cents - expected).abs() < 1e-9, "{cents} vs {expected}");
}

#[rstest::rstest]
fn projected_cents_falls_back_to_the_prompt_price_without_a_cache_price() {
    // Given a row with NO cache-read price (locked decision F).
    let row = priced_row(1e-6, 2e-6, None);

    // When projecting under the same mix.
    let cents = projected_cents(&row, (80_000, 0.99, 1_500));

    // Then the cache-hit share prices at the PROMPT price: the
    // projection is always fully costed.
    let expected = (80_000.0 * (0.99 * 1e-6 + 0.01 * 1e-6) + 1_500.0 * 2e-6) * 100.0;
    assert!((cents - expected).abs() < 1e-9, "{cents} vs {expected}");
}

#[rstest::rstest]
fn projected_cents_for_matches_projected_cents() {
    // Given one priced row and the contract mix.
    let row = priced_row(1e-6, 2e-6, Some(1e-7));

    // When projecting through the raw-price helper and the row wrapper.
    let raw = projected_cents_for(1e-6, 2e-6, Some(1e-7), PROJECTION_MIX);
    let wrapped = projected_cents(&row, PROJECTION_MIX);

    // Then the raw-price helper delegates exactly.
    assert!((raw - wrapped).abs() < 1e-12);
}

// ── Filter ───────────────────────────────────────────────────────────

#[rstest::rstest]
fn space_separated_terms_and_combine() {
    // Given the fixture and a two-term filter both terms must match
    // ("lat" ⊂ …latest, "anth" ⊂ anthropic).
    // When the projection resolves.
    let names = names_matching("lat anth");

    // Then only the name carrying BOTH terms survives.
    assert_eq!(names, vec!["anthropic/claude-3-5-latest".to_owned()]);
}

#[rstest::rstest]
fn term_order_does_not_matter() {
    // Given the two orderings of the same terms.
    let first = names_matching("anth lat");
    let second = names_matching("lat anth");

    // Then both keep exactly the same row.
    assert_eq!(first, vec!["anthropic/claude-3-5-latest".to_owned()]);
    assert_eq!(second, vec!["anthropic/claude-3-5-latest".to_owned()]);
}

#[rstest::rstest]
fn a_term_matching_no_row_drops_every_row() {
    // Given a filter whose second term matches nothing.
    // When the projection resolves.
    let names = names_matching("anth zzz");

    // Then the table is empty: AND means every term must hit.
    assert!(names.is_empty(), "expected no rows, got: {names:?}");
}

#[rstest::rstest]
fn whitespace_only_filter_keeps_every_row() {
    // Given a whitespace-only filter.
    // When the projection resolves.
    let names = names_matching("   ");

    // Then every row survives — it behaves as the empty filter.
    assert_eq!(names.len(), 3);
}

#[rstest::rstest]
fn single_term_filter_keeps_only_the_matching_row() {
    // Given a single-term filter.
    // When the projection resolves.
    let names = names_matching("small");

    // Then only the matching row survives.
    assert_eq!(names, vec!["mistral/small".to_owned()]);
}

#[rstest::rstest]
fn match_ranges_unions_the_terms_glyphs() {
    // Given a row both terms match: against `a/small`, `a` hits index 0
    // and `l` hits index 6 — different glyphs.
    let row = named_row("a", "small");

    // When the highlight ranges resolve.
    let starts: Vec<usize> = match_ranges(&row, "a l")
        .expect("an active filter yields ranges")
        .iter()
        .map(|r| r.start)
        .collect();

    // Then the union is ascending and carries BOTH terms' glyphs.
    assert_eq!(starts, vec![0, 6]);
}

#[rstest::rstest]
fn match_ranges_returns_none_for_a_whitespace_only_filter() {
    // Given a whitespace-only filter.
    let row = named_row("a", "small");

    // When the highlight ranges resolve.
    // Then there is nothing to highlight.
    assert_eq!(match_ranges(&row, " \t "), None);
}

#[rstest::rstest]
fn name_runs_split_the_matched_glyphs_from_the_plain_ones() {
    // Given the filter `a l` over `a/small`: `a` at index 0, `l` at 6.
    let row = named_row("a", "small");

    // When the runs resolve.
    let (runs, variant_runs) = name_runs(&row, "a l");

    // Then concatenating the runs rebuilds the name, exactly the two
    // matched glyphs are marked, and a row without a variant has no
    // suffix runs.
    assert!(
        variant_runs.is_empty(),
        "a row without a variant has no suffix runs"
    );
    let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
    let marked: String = runs
        .iter()
        .filter(|r| r.matched)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(joined, "a/small");
    assert_eq!(marked, "al");
}

#[rstest::rstest]
fn name_runs_are_one_plain_run_without_a_filter() {
    // Given no filter.
    let row = named_row("a", "small");

    // When the runs resolve.
    let (runs, _variant_runs) = name_runs(&row, "");

    // Then the whole name is one unmatched run.
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].matched);
    assert_eq!(runs[0].text, "a/small");
}

#[rstest::rstest]
fn name_runs_put_the_variant_in_its_own_segment() {
    // Given `a/small:free` and the filter `fr`, which lands in the suffix.
    let row = MarketRow {
        variant: Some("free".to_owned()),
        ..named_row("a", "small")
    };

    // When the runs resolve.
    let (base, suffix) = name_runs(&row, "fr");

    // Then the base is whole and plain, and the suffix carries the match.
    assert_eq!(base.len(), 1);
    assert_eq!(base[0].text, "a/small");
    assert!(!base[0].matched);
    let joined: String = suffix.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(joined, ":free");
    let marked: String = suffix
        .iter()
        .filter(|r| r.matched)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(marked, "fr");
}

// ── Detail sort ──────────────────────────────────────────────────────

fn endpoint_row(tag: &str, price: f64, latency: Option<f64>) -> MarketEndpointRow {
    MarketEndpointRow {
        tag: tag.to_owned(),
        provider_name: "Lambda".to_owned(),
        prompt_price: price,
        completion_price: price * 4.0,
        cache_read_price: None,
        latency_p50_ms: latency,
        throughput_p50: Some(40.0),
        uptime_1d: Some(0.99),
        quantization: None,
        overrides: Vec::new(),
    }
}

fn two_endpoints() -> MarketDetail {
    MarketDetail {
        model_id: "a/model".to_owned(),
        endpoints: vec![
            endpoint_row("slow", 1e-6, Some(900.0)),
            endpoint_row("fast", 3e-6, Some(100.0)),
        ],
    }
}

#[rstest::rstest]
fn detail_sort_puts_the_lowest_latency_first_ascending() {
    // Given two endpoints, the pricier one faster.
    let detail = two_endpoints();

    // When sorted by latency ascending.
    let rows = sorted_detail_rows(
        "a/model",
        Some(&detail),
        None,
        DetailSort::Lat,
        false,
        PROJECTION_MIX,
    )
    .expect("the detail belongs to the model");

    // Then the sentinel leads and the fast endpoint is next.
    assert!(matches!(rows[0], DetailRow::Default));
    assert_eq!(rows[1].display_name(), "Lambda (fast)");
    assert_eq!(rows[2].display_name(), "Lambda (slow)");
}

#[rstest::rstest]
fn detail_sort_reverses_on_a_descending_direction() {
    // Given the same two endpoints.
    let detail = two_endpoints();

    // When sorted by latency descending.
    let rows = sorted_detail_rows(
        "a/model",
        Some(&detail),
        None,
        DetailSort::Lat,
        true,
        PROJECTION_MIX,
    )
    .expect("the detail belongs to the model");

    // Then the slow endpoint leads the endpoints.
    assert!(matches!(rows[0], DetailRow::Default));
    assert_eq!(rows[1].display_name(), "Lambda (slow)");
    assert_eq!(rows[2].display_name(), "Lambda (fast)");
}

#[rstest::rstest]
fn detail_name_sort_reverses_with_the_direction() {
    // Given two endpoints whose names order one way ascending.
    let mut detail = two_endpoints();
    detail.endpoints[0].tag = "zeta".to_owned();
    detail.endpoints[1].tag = "alpha".to_owned();

    // When sorted by endpoint ascending.
    let asc = sorted_detail_rows(
        "a/model",
        Some(&detail),
        None,
        DetailSort::Endpoint,
        false,
        PROJECTION_MIX,
    )
    .expect("the detail belongs to the model");

    // Then alpha leads; descending, zeta leads — the name tie-break
    // follows the direction, as the table's Model column does.
    assert_eq!(asc[1].display_name(), "Lambda (alpha)");
    let desc = sorted_detail_rows(
        "a/model",
        Some(&detail),
        None,
        DetailSort::Endpoint,
        true,
        PROJECTION_MIX,
    )
    .expect("the detail belongs to the model");
    assert_eq!(desc[1].display_name(), "Lambda (zeta)");
}

#[rstest::rstest]
fn detail_sort_keeps_the_sentinel_first_in_every_sort() {
    // Given a detail whose sentinel sorts nowhere special.
    let detail = two_endpoints();

    // When sorted under every column in both directions.
    for col in [
        DetailSort::Endpoint,
        DetailSort::Proj,
        DetailSort::In,
        DetailSort::Out,
        DetailSort::Cache,
        DetailSort::Lat,
        DetailSort::Tp,
        DetailSort::Up,
    ] {
        for desc in [false, true] {
            // Then the sentinel is row 0.
            let rows =
                sorted_detail_rows("a/model", Some(&detail), None, col, desc, PROJECTION_MIX)
                    .expect("the detail belongs to the model");
            assert!(matches!(rows[0], DetailRow::Default), "{col:?} desc={desc}");
        }
    }
}

// ── Sort ─────────────────────────────────────────────────────────────

#[rstest::rstest]
fn sort_orders_by_the_projected_cost_ascending() {
    // Given a cheap row and an expensive one, in the opposite order.
    let rows = vec![priced_row(3e-6, 15e-6, None), priced_row(1e-6, 1e-6, None)];

    // When sorting by PROJ ascending.
    let sorted = filter_and_sort(&rows, "", SortColumn::Proj, false, PROJECTION_MIX);

    // Then the cheaper row comes first.
    assert_eq!(sorted[0].prompt_price, 1e-6);
    assert_eq!(sorted[1].prompt_price, 3e-6);
}

#[rstest::rstest]
fn sort_descending_reverses_the_valued_rows() {
    // Given the same two rows.
    let rows = vec![priced_row(3e-6, 15e-6, None), priced_row(1e-6, 1e-6, None)];

    // When sorting by PROJ descending.
    let sorted = filter_and_sort(&rows, "", SortColumn::Proj, true, PROJECTION_MIX);

    // Then the expensive row comes first.
    assert_eq!(sorted[0].prompt_price, 3e-6);
}

#[rstest::rstest]
#[case(SortColumn::Cache)]
#[case(SortColumn::Eci)]
#[case(SortColumn::Vals)]
fn none_keys_sort_last_in_either_direction(#[case] col: SortColumn) {
    // Given a valued row and an unvalued one, in that order.
    let mut valued = named_row("a", "valued");
    match col {
        SortColumn::Cache => valued.cache_read_price = Some(1e-7),
        SortColumn::Eci => valued.eci = Some(50.0),
        SortColumn::Vals => valued.vals = Some(60.0),
        _ => {}
    }
    let unvalued = named_row("b", "unvalued");
    let rows = vec![valued.clone(), unvalued.clone()];

    // When sorting ascending, then descending.
    let asc = filter_and_sort(&rows, "", col, false, PROJECTION_MIX);
    let desc = filter_and_sort(&rows, "", col, true, PROJECTION_MIX);

    // Then the valued row is first in both directions — a flip never
    // surfaces the unmatched rows first.
    assert_eq!(asc[0].slug, "valued");
    assert_eq!(desc[0].slug, "valued");
}

#[rstest::rstest]
fn the_name_sort_tiebreaks_by_display_name() {
    // Given two rows whose name-sort key is `None` for both.
    let rows = vec![named_row("zeta", "last"), named_row("alpha", "first")];

    // When sorting by MODEL ascending.
    let sorted = filter_and_sort(&rows, "", SortColumn::Model, false, PROJECTION_MIX);

    // Then the tiebreak is the display names, ascending.
    assert_eq!(row_name(&sorted[0]), "alpha/first");
}

#[rstest::rstest]
fn the_name_sort_reverses_with_the_direction() {
    // Given two rows in reverse-name order.
    let rows = vec![named_row("zeta", "last"), named_row("alpha", "first")];

    // When sorting by MODEL descending.
    let sorted = filter_and_sort(&rows, "", SortColumn::Model, true, PROJECTION_MIX);

    // Then the names reverse: zeta leads.
    assert_eq!(row_name(&sorted[0]), "zeta/last");
}

// ── Formatting ───────────────────────────────────────────────────────

#[rstest::rstest]
#[case(Some(3e-6), "$3.0000/M")]
#[case(Some(3.021e-7), "$0.3021/M")]
#[case(None, EMPTY_CELL)]
fn cache_column_formats_or_empties(#[case] price: Option<f64>, #[case] expected: &str) {
    // Given a published or unpublished cache price.
    // When formatting.
    // Then it renders four-digit per-million, or the empty placeholder.
    assert_eq!(cache_column(price), expected);
}

#[rstest::rstest]
fn dollars_formats_four_decimals() {
    // Given 51 cents.
    // When formatting.
    // Then it renders as dollars with four decimals.
    assert_eq!(dollars(51.0), "$0.5100");
}

#[rstest::rstest]
#[case(Some(58.26), "58.3")]
#[case(None, EMPTY_CELL)]
fn eci_column_formats_one_decimal(#[case] eci: Option<f64>, #[case] expected: &str) {
    assert_eq!(eci_column(eci), expected);
}

#[rstest::rstest]
#[case(Some(71.309), "71.31")]
#[case(None, EMPTY_CELL)]
fn vals_column_formats_two_decimals(#[case] vals: Option<f64>, #[case] expected: &str) {
    assert_eq!(vals_column(vals), expected);
}

#[rstest::rstest]
#[case(Some(1159.4), "1159ms")]
#[case(None, EMPTY_CELL)]
fn ms_column_formats_whole_milliseconds(#[case] ms: Option<f64>, #[case] expected: &str) {
    assert_eq!(ms_column(ms), expected);
}

#[rstest::rstest]
#[case(Some(38.2), "38 tok/s")]
#[case(None, EMPTY_CELL)]
fn throughput_column_formats_whole_tokens_per_second(
    #[case] tp: Option<f64>,
    #[case] expected: &str,
) {
    assert_eq!(throughput_column(tp), expected);
}

#[rstest::rstest]
#[case(Some(0.9923), "99.2%")]
#[case(None, EMPTY_CELL)]
fn percent_column_formats_one_decimal(#[case] frac: Option<f64>, #[case] expected: &str) {
    assert_eq!(percent_column(frac), expected);
}

// ── Page URL ─────────────────────────────────────────────────────────

/// The page URL derives from the listing id: prefix stripped, variant
/// suffix riding along, prefix-miss falling back to the raw id.
#[rstest::rstest]
#[case(
    "openrouter/deepseek/deepseek-chat:free",
    "https://openrouter.ai/deepseek/deepseek-chat:free"
)]
#[case(
    "openrouter/openai/gpt-oss-120b",
    "https://openrouter.ai/openai/gpt-oss-120b"
)]
#[case(
    "deepseek/deepseek-chat",
    "https://openrouter.ai/deepseek/deepseek-chat"
)]
fn model_page_url_strips_the_prefix_and_keeps_the_variant(
    #[case] model_id: &str,
    #[case] expected: &str,
) {
    assert_eq!(model_page_url(model_id), expected);
}
