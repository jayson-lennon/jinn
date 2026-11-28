//! Pure helpers over the two market cells: the operations route actions
//! and the view need, kept out of the route closures so tests can drive
//! them directly.
//!
//! Everything here is sync-side: these functions read
//! [`MarketDataState`] and write [`MarketUiState`], never the reverse.
//! The actors' writes live in the actor modules (T2/T3).
//!
//! The list projection — the filter, the sort, the projected cost, and
//! the match ranges — lives in the contract crate so the web UI builds
//! its rows through the same functions. This module re-exports it and
//! keeps the terminal's own detail-mode projection beside it.

use jinn_model_market_msg::DetailSort;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;

pub use jinn_model_market_msg::compare_rows;
pub use jinn_model_market_msg::match_ranges as match_ranges_for;
pub use jinn_model_market_msg::projected_cents;
pub use jinn_model_market_msg::projected_cents_for;
pub use jinn_model_market_msg::resolve_mix;
pub use jinn_model_market_msg::row_name;
pub use jinn_model_market_msg::sort_key;

pub use jinn_model_market_msg::DetailRow;
pub use jinn_model_market_msg::endpoint_label;

/// The fuzzy match offsets of the active filter against one row's
/// display name, as the `[Range<usize>]` slices the selection widget's
/// highlighter takes — `None` with no filter.
#[must_use]
pub fn match_ranges(row: &MarketRow, ui: &MarketUiState) -> Option<Vec<std::ops::Range<usize>>> {
    match_ranges_for(row, &ui.filter)
}

/// The model's listing row, for the sentinel's prices.
fn model_row<'a>(data: &'a MarketDataState, ui: &MarketUiState) -> Option<&'a MarketRow> {
    let pinned = ui.detail_model.as_ref()?;
    data.rows.iter().find(|row| row.id == *pinned)
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
    let pinned = ui.detail_model.as_ref()?;
    jinn_model_market_msg::detail_rows(pinned, data.detail.as_ref())
}

/// The visible projection: the contract's fuzzy filter over
/// `author/slug[:variant]`, then its stable sort under the UI's column,
/// direction, and active mix.
#[must_use]
pub fn visible_rows(data: &MarketDataState, ui: &MarketUiState) -> Vec<MarketRow> {
    jinn_model_market_msg::filter_and_sort(
        &data.rows,
        &ui.filter,
        ui.sort_col,
        ui.sort_desc,
        ui.active_mix,
    )
}

/// The detail sort key for one row under the UI's detail sort column.
#[must_use]
pub fn detail_sort_key(
    row: &DetailRow,
    col: DetailSort,
    model: Option<&MarketRow>,
    mix: (u32, f64, u32),
) -> Option<f64> {
    jinn_model_market_msg::detail_sort_key(row, col, model, mix)
}

/// Compares two detail rows under the UI's detail sort column and
/// direction.
#[must_use]
pub fn detail_compare_rows(
    a: &DetailRow,
    b: &DetailRow,
    ui: &MarketUiState,
    model: Option<&MarketRow>,
    mix: (u32, f64, u32),
) -> std::cmp::Ordering {
    jinn_model_market_msg::detail_compare_rows(
        a,
        b,
        ui.detail_sort_col,
        ui.detail_sort_desc,
        model,
        mix,
    )
}

/// The detail projection, sorted for render: the Default sentinel stays
/// row 0 — the safe action sits at zero keystrokes in every sort — and
/// the endpoint rows sort by the detail sort.
#[must_use]
pub fn sorted_detail_rows(data: &MarketDataState, ui: &MarketUiState) -> Option<Vec<DetailRow>> {
    jinn_model_market_msg::sorted_detail_rows(
        ui.detail_model.as_deref()?,
        data.detail.as_ref(),
        model_row(data, ui),
        ui.detail_sort_col,
        ui.detail_sort_desc,
        ui.active_mix,
    )
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
mod detail_projection_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;
    use jinn_model_market_msg::MarketEndpointRow;
    use jinn_model_market_msg::PROJECTION_MIX;

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
            vals: None,
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
}
