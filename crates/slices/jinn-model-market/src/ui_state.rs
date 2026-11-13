//! Pure helpers over the two market cells: the operations route actions
//! and the view need, kept out of the route closures so tests can drive
//! them directly.
//!
//! Everything here is sync-side: these functions read
//! [`MarketDataState`] and write [`MarketUiState`], never the reverse.
//! The actors' writes live in the actor modules (T2/T3).

use fuzzy_matcher::FuzzyMatcher;
use jinn_model_market_msg::DetailSort;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketEndpointRow;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::MixSource;
use jinn_model_market_msg::PROJECTION_MIX;
use jinn_model_market_msg::SortColumn;
use std::ops::Range;

/// One detail-mode row: the auto-route sentinel or a provider endpoint.
///
/// A render-time projection, recomputed every frame like PROJ — the
/// sentinel is synthetic (its prices come from the model's `MarketRow`),
/// so it is not stored anywhere.
#[derive(Debug, Clone, PartialEq)]
pub enum DetailRow {
    /// The auto-route sentinel: prices come from the model's
    /// `MarketRow`; ENTER publishes the model-only switch (`tag: None`).
    Default,
    /// One provider endpoint; ENTER pins it (`tag: Some`).
    Endpoint(MarketEndpointRow),
}

impl DetailRow {
    /// The row's display name: `provider_name` plus ` (tag)` for a
    /// non-empty tag, `Default` for the sentinel — the `Endpoint`
    /// sort's tie-break key.
    #[must_use]
    pub fn display_name(&self) -> String {
        match self {
            Self::Default => "Default".to_owned(),
            Self::Endpoint(endpoint) => endpoint_label(endpoint),
        }
    }

    /// The row's prices: `(prompt, completion, cache)` as sort keys —
    /// the sentinel with no model row left yields `None`s, so a missing
    /// listing sorts last like any unpublished value.
    fn sort_prices(&self, model: Option<&MarketRow>) -> (Option<f64>, Option<f64>, Option<f64>) {
        match self {
            Self::Default => match model {
                Some(row) => (
                    row.prompt_price.into(),
                    row.completion_price.into(),
                    row.cache_read_price,
                ),
                None => (None, None, None),
            },
            Self::Endpoint(endpoint) => (
                endpoint.prompt_price.into(),
                endpoint.completion_price.into(),
                endpoint.cache_read_price,
            ),
        }
    }
}

/// The projected cost of one turn under the given mix, in cents.
///
/// `proj = I·(hit·cache_read_or_prompt + (1−hit)·prompt) + O·completion`
/// — locked decision F prices cache reads at the prompt price when the
/// listing publishes no cache price, so the projection is always fully
/// costed.
#[must_use]
pub fn projected_cents(row: &MarketRow, mix: (u32, f64, u32)) -> f64 {
    projected_cents_for(
        row.prompt_price,
        row.completion_price,
        row.cache_read_price,
        mix,
    )
}

/// [`projected_cents`] over raw prices — the shared body both the
/// listing rows and the detail rows (whose sentinel prices come from a
/// `MarketRow`) project through.
#[must_use]
pub fn projected_cents_for(
    prompt: f64,
    completion: f64,
    cache: Option<f64>,
    mix: (u32, f64, u32),
) -> f64 {
    let (input_tokens, cache_hit, output_tokens) = mix;
    let cache_read = cache.unwrap_or(prompt);
    let input = f64::from(input_tokens) * (cache_hit * cache_read + (1.0 - cache_hit) * prompt);
    let output = f64::from(output_tokens) * completion;
    (input + output) * 100.0
}

/// The mix the projection runs under: the shipped constant for
/// [`MixSource::Fixed`], the session tree's measured usage for
/// [`MixSource::Usage`].
///
/// The usage derivation mirrors the status bar's tree aggregate: input =
/// effective sent (provider-reported prompt tokens, local estimates as
/// fallback), output = received, cache hit = cached over measured. A
/// tree with no measured requests cannot derive a mix (every value
/// would be zero, projecting every model to $0.00), so it falls back to
/// the fixed constant — the toggle stays honest about ranking by
/// something.
#[must_use]
pub fn resolve_mix(source: MixSource, usage: Option<(u64, u64, u64, u64)>) -> (u32, f64, u32) {
    let Some((effective_sent, total_received, cached_total, measured_sent)) = usage else {
        return PROJECTION_MIX;
    };
    match source {
        MixSource::Fixed => PROJECTION_MIX,
        MixSource::Usage if measured_sent == 0 => PROJECTION_MIX,
        MixSource::Usage => {
            let cache_hit = cached_total as f64 / measured_sent.max(1) as f64;
            (
                u32::try_from(effective_sent).unwrap_or(u32::MAX),
                cache_hit.clamp(0.0, 1.0),
                u32::try_from(total_received).unwrap_or(u32::MAX),
            )
        }
    }
}

/// The sort key for one row.
///
/// `None` sorts after every value in either direction — an unmatched ECI
/// or an unpriced cache must not jump to the top when the user flips to
/// descending.
#[must_use]
pub fn sort_key(row: &MarketRow, col: SortColumn, mix: (u32, f64, u32)) -> Option<f64> {
    match col {
        SortColumn::Proj => Some(projected_cents(row, mix)),
        SortColumn::In => Some(row.prompt_price),
        SortColumn::Out => Some(row.completion_price),
        SortColumn::Cache => row.cache_read_price,
        SortColumn::Eci => row.eci,
        SortColumn::Model => None,
    }
}

/// The row's display name the filter matches against:
/// `author/slug[:variant]`.
#[must_use]
pub fn row_name(row: &MarketRow) -> String {
    match &row.variant {
        Some(variant) => format!("{}/{}:{variant}", row.author, row.slug),
        None => format!("{}/{}", row.author, row.slug),
    }
}

/// An endpoint's display label: `provider_name` plus ` (tag)` when the
/// tag is non-empty — the ENDPOINT cell's text and the name-sort's
/// tie-break key.
#[must_use]
pub fn endpoint_label(endpoint: &MarketEndpointRow) -> String {
    if endpoint.tag.is_empty() {
        endpoint.provider_name.clone()
    } else {
        format!("{} ({})", endpoint.provider_name, endpoint.tag)
    }
}

/// The detail mode's visible projection: the Default sentinel first,
/// then the endpoints — or `None` when nothing renders.
///
/// The pin is the stale guard: `data.detail` renders only when it
/// belongs to the pinned model, so detail fetched for a previously
/// drilled-in model cannot appear under the current one. Absent or
/// mismatched detail is the loading state, not an empty table.
#[must_use]
pub fn detail_rows(data: &MarketDataState, ui: &MarketUiState) -> Option<Vec<DetailRow>> {
    let detail = data.detail.as_ref()?;
    let pinned = ui.detail_model.as_ref()?;
    (detail.model_id == *pinned).then(|| {
        let mut rows: Vec<DetailRow> = detail
            .endpoints
            .iter()
            .map(|endpoint| DetailRow::Endpoint(endpoint.clone()))
            .collect();
        rows.insert(0, DetailRow::Default);
        rows
    })
}

/// The filter split into its AND terms: whitespace-separated, each of
/// which must fuzzy-match a row's name for the row to stay visible.
/// A whitespace-only filter yields no terms — unfiltered.
#[must_use]
fn filter_terms(filter: &str) -> Vec<&str> {
    filter.split_whitespace().collect()
}

/// Whether every filter term fuzzy-matches the row name.
///
/// Terms are order-insensitive and match anywhere — plain per-term
/// `SkimMatcherV2` subsequence semantics, no positional logic.
#[must_use]
fn all_terms_match(
    matcher: &fuzzy_matcher::skim::SkimMatcherV2,
    name: &str,
    terms: &[&str],
) -> bool {
    terms
        .iter()
        .all(|term| matcher.fuzzy_match(name, term).is_some())
}

/// The visible projection: rows filtered by the fuzzy filter, then
/// stably sorted by the selected column.
///
/// The filter runs over `author/slug[:variant]` with `SkimMatcherV2`:
/// whitespace-separated terms AND-combine, so a row stays only when
/// every term matches; a whitespace-only filter keeps everything. The
/// sort is stable so equal-cost rows keep their fetch order.
#[must_use]
pub fn visible_rows(data: &MarketDataState, ui: &MarketUiState) -> Vec<MarketRow> {
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
    let terms = filter_terms(&ui.filter);
    let mut rows: Vec<MarketRow> = data
        .rows
        .iter()
        .filter(|row| terms.is_empty() || all_terms_match(&matcher, &row_name(row), &terms))
        .cloned()
        .collect();
    rows.sort_by(|a, b| compare_rows(a, b, ui, ui.active_mix));
    rows
}

/// The fuzzy match offsets of the filter against one row's display name
/// — the byte indices the filter highlighted, as the `[Range<usize>]`
/// slices the selection widget's highlighter takes.
///
/// `None` means "no filter active" (nothing to highlight). An active
/// multi-term filter highlights the UNION of every term's matched
/// glyphs: the row reaching here matched every term, so each term
/// contributes its indices, merged and ascending. An active filter that
/// misses a row cannot reach here, because the projection has already
/// dropped the row.
#[must_use]
pub fn match_ranges(row: &MarketRow, ui: &MarketUiState) -> Option<Vec<Range<usize>>> {
    let terms = filter_terms(&ui.filter);
    if terms.is_empty() {
        return None;
    }
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
    let name = row_name(row);
    let mut indices: Vec<usize> = terms
        .iter()
        .filter_map(|term| matcher.fuzzy_indices(&name, term))
        .flat_map(|(_, matched)| matched)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    Some(indices.into_iter().map(|i| i..i + 1).collect())
}

/// Compares two rows under the UI's sort column and direction.
///
/// `None` keys sort after values regardless of direction, so a flip
/// never surfaces the unmatched rows first.
#[must_use]
pub fn compare_rows(
    a: &MarketRow,
    b: &MarketRow,
    ui: &MarketUiState,
    mix: (u32, f64, u32),
) -> std::cmp::Ordering {
    match (sort_key(a, ui.sort_col, mix), sort_key(b, ui.sort_col, mix)) {
        (Some(av), Some(bv)) => {
            let ord = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);
            if ui.sort_desc { ord.reverse() } else { ord }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => row_name(a).cmp(&row_name(b)),
    }
}

/// The model's listing row, for the sentinel's prices.
fn model_row<'a>(data: &'a MarketDataState, ui: &MarketUiState) -> Option<&'a MarketRow> {
    let pinned = ui.detail_model.as_ref()?;
    data.rows.iter().find(|row| row.id == *pinned)
}

/// The detail sort key for one row, mirroring [`sort_key`]'s
/// `None`-sorts-last rule.
///
/// `Endpoint` is the name-sort: `None` key, tie-broken by display name,
/// like the table's `Model`. The sentinel's price keys come from the
/// model's listing row; a missing row (a refresh emptied the table
/// mid-mode) sorts as a `None` key — a fallback, never a panic.
#[must_use]
pub fn detail_sort_key(
    row: &DetailRow,
    col: DetailSort,
    model: Option<&MarketRow>,
    mix: (u32, f64, u32),
) -> Option<f64> {
    match col {
        DetailSort::Endpoint => None,
        DetailSort::Proj => match row {
            DetailRow::Default => model.map(|m| projected_cents(m, mix)),
            DetailRow::Endpoint(endpoint) => Some(projected_cents_for(
                endpoint.prompt_price,
                endpoint.completion_price,
                endpoint.cache_read_price,
                mix,
            )),
        },
        DetailSort::In => row.sort_prices(model).0,
        DetailSort::Out => row.sort_prices(model).1,
        DetailSort::Cache => row.sort_prices(model).2,
        DetailSort::Lat => match row {
            DetailRow::Default => None,
            DetailRow::Endpoint(endpoint) => endpoint.latency_p50_ms,
        },
        DetailSort::Tp => match row {
            DetailRow::Default => None,
            DetailRow::Endpoint(endpoint) => endpoint.throughput_p50,
        },
        DetailSort::Up => match row {
            DetailRow::Default => None,
            DetailRow::Endpoint(endpoint) => endpoint.uptime_1d,
        },
    }
}

/// Compares two detail rows under the UI's detail sort column and
/// direction — the same `None`-last semantics as [`compare_rows`].
#[must_use]
pub fn detail_compare_rows(
    a: &DetailRow,
    b: &DetailRow,
    ui: &MarketUiState,
    model: Option<&MarketRow>,
    mix: (u32, f64, u32),
) -> std::cmp::Ordering {
    match (
        detail_sort_key(a, ui.detail_sort_col, model, mix),
        detail_sort_key(b, ui.detail_sort_col, model, mix),
    ) {
        (Some(av), Some(bv)) => {
            let ord = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);
            if ui.detail_sort_desc {
                ord.reverse()
            } else {
                ord
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.display_name().cmp(&b.display_name()),
    }
}

/// The detail projection, sorted for render: the Default sentinel stays
/// row 0 — the safe action sits at zero keystrokes in every sort, the
/// same position the endpoint picker prepends its sentinel to — and the
/// endpoint rows sort by the detail sort.
#[must_use]
pub fn sorted_detail_rows(data: &MarketDataState, ui: &MarketUiState) -> Option<Vec<DetailRow>> {
    let model = model_row(data, ui);
    let mut rows = detail_rows(data, ui)?;
    let sentinel = rows.remove(0);
    rows.sort_by(|a, b| detail_compare_rows(a, b, ui, model, ui.active_mix));
    rows.insert(0, sentinel);
    Some(rows)
}

/// The detail row at the mode's highlight, if any.
#[must_use]
pub fn highlighted_detail_row(data: &MarketDataState, ui: &MarketUiState) -> Option<DetailRow> {
    sorted_detail_rows(data, ui)?
        .get(ui.detail_selection)
        .cloned()
}

/// The highlighted row in the visible projection, if any.
#[must_use]
pub fn highlighted_row(data: &MarketDataState, ui: &MarketUiState) -> Option<MarketRow> {
    let rows = visible_rows(data, ui);
    rows.get(ui.selection).cloned()
}

/// Moves the highlight up one row, clamped at the top.
pub fn move_up(ui: &mut MarketUiState) {
    ui.selection = ui.selection.saturating_sub(1);
}

/// Moves the highlight down one row, clamped at the projection's end.
pub fn move_down(ui: &mut MarketUiState, visible_count: usize) {
    if visible_count == 0 {
        ui.selection = 0;
        return;
    }
    ui.selection = (ui.selection + 1).min(visible_count - 1);
}

/// Pages the highlight up by the measured viewport.
pub fn page_up(ui: &mut MarketUiState) {
    ui.selection = ui.selection.saturating_sub(ui.results_viewport);
}

/// Pages the highlight down by the measured viewport.
pub fn page_down(ui: &mut MarketUiState, visible_count: usize) {
    if visible_count == 0 {
        ui.selection = 0;
        return;
    }
    ui.selection = (ui.selection + ui.results_viewport).min(visible_count - 1);
}

#[cfg(test)]
mod projected_cost_tests {
    use super::*;

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
            is_free: false,
        }
    }

    #[rstest::rstest]
    fn projected_cents_prices_cache_reads_at_the_cache_price() {
        // Given a row publishing a cache-read price.
        let row = priced_row(1e-6, 2e-6, Some(1e-7));

        // When projecting under the first preset (80k in, 99% cache
        // hit, 1.5k out).
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
    fn projected_cents_scales_with_the_input_side_of_the_mix() {
        // Given one priced row.
        let row = priced_row(1e-6, 2e-6, Some(1e-7));

        // When projecting under 20k input versus 150k input (same hit
        // and output).
        let small = projected_cents(&row, (20_000, 0.99, 1_500));
        let large = projected_cents(&row, (150_000, 0.99, 1_500));

        // Then the larger input mix projects proportionally more.
        assert!(large > small);
    }
}

#[cfg(test)]
mod detail_projection_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;

    /// A `MarketRow` the detail fixtures drill into.
    fn priced_row(id: &str, prompt: f64, completion: f64, cache: Option<f64>) -> MarketRow {
        MarketRow {
            id: id.to_owned(),
            author: "a".to_owned(),
            slug: id
                .strip_prefix("openrouter/a/")
                .unwrap_or("model")
                .to_owned(),
            variant: None,
            prompt_price: prompt,
            completion_price: completion,
            cache_read_price: cache,
            has_overrides: false,
            context_length: None,
            eci: None,
            eci_ci: None,
            is_free: false,
        }
    }

    /// An endpoint with the given prices; the rest defaults away.
    fn endpoint(
        tag: &str,
        name: &str,
        prompt: f64,
        completion: f64,
        cache: Option<f64>,
    ) -> MarketEndpointRow {
        MarketEndpointRow {
            tag: tag.to_owned(),
            provider_name: name.to_owned(),
            prompt_price: prompt,
            completion_price: completion,
            cache_read_price: cache,
            latency_p50_ms: None,
            throughput_p50: None,
            uptime_1d: None,
            quantization: None,
            overrides: Vec::new(),
        }
    }

    /// Detail data + UI state pinned to a two-endpoint model: one
    /// endpoint publishes no cache price, the other carries a cheaper
    /// cache read.
    fn fixture() -> (MarketDataState, MarketUiState) {
        let data = MarketDataState {
            rows: vec![priced_row("openrouter/a/small", 1e-6, 2e-6, Some(1e-7))],
            detail: Some(jinn_model_market_msg::MarketDetail {
                model_id: "openrouter/a/small".to_owned(),
                endpoints: vec![
                    endpoint("azure/us-east", "Azure", 1.2e-6, 4.5e-6, Some(0.9e-7)),
                    endpoint("", "Lambda", 0.5e-6, 1e-6, None),
                ],
            }),
            ..MarketDataState::default()
        };
        let mut ui = MarketUiState::default();
        ui.enter_detail("openrouter/a/small".to_owned());
        (data, ui)
    }

    #[rstest::rstest]
    fn detail_rows_prepend_the_default_sentinel() {
        // Given a pinned model whose detail has landed.
        let (data, ui) = fixture();

        // When the projection resolves.
        let rows = detail_rows(&data, &ui).expect("pinned detail projects");

        // Then the sentinel is first and every endpoint follows in fetch
        // order.
        assert_eq!(rows.first(), Some(&DetailRow::Default));
        assert_eq!(rows.len(), 3);
    }

    #[rstest::rstest]
    fn detail_rows_are_none_when_the_pin_is_absent() {
        // Given table mode — no pin.
        let (data, mut ui) = fixture();
        ui.detail_model = None;

        // When the projection resolves.
        let rows = detail_rows(&data, &ui);

        // Then nothing renders: absent detail is the loading state.
        assert!(rows.is_none());
    }

    #[rstest::rstest]
    fn detail_rows_are_none_when_detail_belongs_to_another_model() {
        // Given the data cell still holding the previous model's detail.
        let (mut data, ui) = fixture();
        data.detail.as_mut().expect("fixture detail").model_id = "openrouter/b/big".to_owned();

        // When the projection resolves.
        let rows = detail_rows(&data, &ui);

        // Then the stale detail does not project: the pin is the guard.
        assert!(rows.is_none());
    }

    #[rstest::rstest]
    fn the_sentinel_prices_from_the_models_listing_row() {
        // Given a pinned model whose detail has landed.
        let (data, ui) = fixture();

        // When the sentinel's projection is keyed.
        let key = detail_sort_key(
            &DetailRow::Default,
            DetailSort::Proj,
            model_row(&data, &ui),
            PROJECTION_MIX,
        );

        // Then it matches the listing row's own PROJ — the sentinel is
        // that listing, auto-routed.
        let expected = projected_cents(&data.rows[0], PROJECTION_MIX);
        assert_eq!(key, Some(expected));
    }

    #[rstest::rstest]
    fn the_sentinel_sorts_last_when_the_listing_row_is_gone() {
        // Given detail mode over a table the refresh has emptied.
        let (mut data, ui) = fixture();
        data.rows.clear();

        // When the sentinel's price keys are taken.
        let in_key = detail_sort_key(
            &DetailRow::Default,
            DetailSort::In,
            model_row(&data, &ui),
            PROJECTION_MIX,
        );
        let proj_key = detail_sort_key(
            &DetailRow::Default,
            DetailSort::Proj,
            model_row(&data, &ui),
            PROJECTION_MIX,
        );

        // Then every key is None — the fallback, never a panic or a
        // made-up number.
        assert_eq!(in_key, None);
        assert_eq!(proj_key, None);
    }

    #[rstest::rstest]
    fn endpoint_sort_keys_come_from_the_endpoint() {
        // Given the fixture's cache-publishing endpoint.
        let (data, ui) = fixture();
        let azure = DetailRow::Endpoint(endpoint(
            "azure/us-east",
            "Azure",
            1.2e-6,
            4.5e-6,
            Some(0.9e-7),
        ));

        // When its keys are taken.
        let in_key = detail_sort_key(
            &azure,
            DetailSort::In,
            model_row(&data, &ui),
            PROJECTION_MIX,
        );
        let cache_key = detail_sort_key(
            &azure,
            DetailSort::Cache,
            model_row(&data, &ui),
            PROJECTION_MIX,
        );

        // Then they are the endpoint's own prices.
        assert_eq!(in_key, Some(1.2e-6));
        assert_eq!(cache_key, Some(0.9e-7));
    }

    #[rstest::rstest]
    fn unpublished_cache_sorts_last_in_either_direction() {
        // Given the fixture sorted by CACHE with the unpublishing
        // endpoint against the sentinel.
        let (data, mut ui) = fixture();
        ui.detail_sort_col = DetailSort::Cache;
        let sentinel = DetailRow::Default;
        let lambda = DetailRow::Endpoint(endpoint("", "Lambda", 0.5e-6, 1e-6, None));
        let model = model_row(&data, &ui);

        // When compared ascending, then descending.
        let asc = detail_compare_rows(&sentinel, &lambda, &ui, model, PROJECTION_MIX);
        ui.detail_sort_desc = true;
        let desc = detail_compare_rows(&lambda, &sentinel, &ui, model, PROJECTION_MIX);

        // Then the valued row sorts before the `None` key in both
        // directions — ascending, the valued row is Less; descending,
        // the valued row (now second) is Greater.
        assert_eq!(asc, std::cmp::Ordering::Less);
        assert_eq!(desc, std::cmp::Ordering::Greater);
    }

    #[rstest::rstest]
    fn the_name_sort_tiebreaks_by_display_name() {
        // Given two rows both keying None on the name sort.
        let (data, mut ui) = fixture();
        ui.detail_sort_col = DetailSort::Endpoint;
        let model = model_row(&data, &ui);
        let sentinel = DetailRow::Default;
        let azure = DetailRow::Endpoint(endpoint("azure/us-east", "Azure", 1e-6, 1e-6, None));

        // When they are compared.
        let ord = detail_compare_rows(&azure, &sentinel, &ui, model, PROJECTION_MIX);

        // Then the tiebreak is the display names, ascending:
        // "Azure (azure/us-east)" before "Default".
        assert_eq!(ord, std::cmp::Ordering::Less);
    }

    #[rstest::rstest]
    fn endpoint_labels_render_the_tag_only_when_present() {
        // Given one tagged and one untagged endpoint.
        let tagged = endpoint("azure/us-east", "Azure", 1e-6, 1e-6, None);
        let untagged = endpoint("", "Lambda", 1e-6, 1e-6, None);

        // When their labels are built.
        // Then only the tagged one carries the parenthesized tag.
        assert_eq!(endpoint_label(&tagged), "Azure (azure/us-east)");
        assert_eq!(endpoint_label(&untagged), "Lambda");
    }

    #[rstest::rstest]
    fn projected_cents_for_matches_projected_cents() {
        // Given one priced row and the contract mix.
        let row = priced_row("openrouter/a/small", 1e-6, 2e-6, Some(1e-7));

        // When projecting through the raw-price helper and the row
        // wrapper.
        let raw = projected_cents_for(1e-6, 2e-6, Some(1e-7), PROJECTION_MIX);
        let wrapped = projected_cents(&row, PROJECTION_MIX);

        // Then the refactor delegated exactly.
        assert!((raw - wrapped).abs() < 1e-12);
    }
}

#[cfg(test)]
mod filter_tests {
    #![allow(clippy::indexing_slicing, reason = "test code")]

    use super::*;

    /// A `MarketRow` named `author/slug` — everything else defaults
    /// away; only the name matters to the filter.
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
            is_free: false,
        }
    }

    /// A data cell over `anthropic/claude-3-5-latest`, `mistral/small`,
    /// and `a/tiny` — the multi-term and single-term shapes.
    fn fixture() -> MarketDataState {
        MarketDataState {
            rows: vec![
                named_row("anthropic", "claude-3-5-latest"),
                named_row("mistral", "small"),
                named_row("a", "tiny"),
            ],
            ..MarketDataState::default()
        }
    }

    fn rows_matching(ui: &MarketUiState) -> Vec<String> {
        let mut names: Vec<String> = visible_rows(&fixture(), ui).iter().map(row_name).collect();
        names.sort();
        names
    }

    /// UI state carrying only the filter — struct-update syntax, so a
    /// test names exactly what varies.
    fn filtered(filter: &str) -> MarketUiState {
        MarketUiState {
            filter: filter.to_owned(),
            ..MarketUiState::default()
        }
    }

    #[rstest::rstest]
    fn space_separated_terms_and_combine() {
        // Given the fixture and a two-term filter both terms must
        // match ("lat" ⊂ …latest, "anth" ⊂ anthropic).
        let ui = filtered("lat anth");

        // When the projection resolves.
        let names = rows_matching(&ui);

        // Then only the name carrying BOTH terms survives.
        assert_eq!(names, vec!["anthropic/claude-3-5-latest".to_owned()]);
    }

    #[rstest::rstest]
    fn term_order_does_not_matter() {
        // Given the two orderings of the same terms.
        let first = filtered("anth lat");
        let second = filtered("lat anth");

        // When both projections resolve.
        // Then both keep exactly the same row.
        assert_eq!(
            rows_matching(&first),
            vec!["anthropic/claude-3-5-latest".to_owned()]
        );
        assert_eq!(
            rows_matching(&second),
            vec!["anthropic/claude-3-5-latest".to_owned()]
        );
    }

    #[rstest::rstest]
    fn a_term_matching_no_row_drops_every_row() {
        // Given a filter whose second term matches nothing.
        let ui = filtered("anth zzz");

        // When the projection resolves.
        let names = rows_matching(&ui);

        // Then the table is empty: AND means every term must hit.
        assert!(names.is_empty(), "expected no rows, got: {names:?}");
    }

    #[rstest::rstest]
    fn whitespace_only_filter_keeps_every_row() {
        // Given a whitespace-only filter.
        let ui = filtered("   ");

        // When the projection resolves.
        let names = rows_matching(&ui);

        // Then every row survives — it behaves as the empty filter.
        assert_eq!(names.len(), 3);
    }

    #[rstest::rstest]
    fn single_term_filter_behavior_is_unchanged() {
        // Given a single-term filter.
        let ui = filtered("small");

        // When the projection resolves.
        let names = rows_matching(&ui);

        // Then only the matching row survives, exactly as before.
        assert_eq!(names, vec!["mistral/small".to_owned()]);
    }

    #[rstest::rstest]
    fn match_ranges_unions_the_terms_glyphs() {
        // Given a row both terms match: against `a/small`, `a` hits
        // index 0 and `l` hits index 6 — different glyphs.
        let row = named_row("a", "small");
        let ui = filtered("a l");

        // When the highlight ranges resolve.
        let starts: Vec<usize> = match_ranges(&row, &ui)
            .map_or_default(|ranges| ranges.iter().map(|r| r.start).collect());

        // Then the union is ascending and carries BOTH terms' glyphs —
        // more than either term alone highlights.
        assert_eq!(starts, vec![0, 6]);
    }

    #[rstest::rstest]
    fn match_ranges_returns_none_for_a_whitespace_only_filter() {
        // Given a whitespace-only filter.
        let row = named_row("a", "small");
        let ui = filtered(" \t ");

        // When the highlight ranges resolve.
        // Then there is nothing to highlight.
        assert_eq!(match_ranges(&row, &ui), None);
    }
}
