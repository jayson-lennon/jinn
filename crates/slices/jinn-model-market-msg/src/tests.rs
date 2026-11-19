//! The contract crate's own tests: the wire invariants T2/T3 build on.

#![expect(
    clippy::expect_used,
    reason = "test module, a failed wire invariant is a test failure"
)]

use super::*;
use crate::scores::{EciScore, ValsScore};

// ── Crossing-schema contract ─────────────────────────────────────────

#[rstest::rstest]
#[test]
fn refresh_all_is_still_a_command() {
    // Given the canonical refresh contract.
    // When reading its schema definition.
    let def = <RefreshAll as trouper::schema::Schema>::schema_def();

    // Then the name and kind are stable: the name is the wire contract,
    // and the kind is load-bearing. `RefreshAll` is an imperative
    // instruction ("refresh now"), not a fact about the past, so it is a
    // Command. It has exactly one handler — the OpenRouter market actor
    // — so the route resolves to that actor and the delivery is
    // unambiguous. Re-declaring it as an Event to get broadcast
    // semantics would make the schema kind lie about the message.
    assert_eq!(def.name, "RefreshAll");
    assert!(matches!(def.kind, trouper::schema::SchemaKind::Command));
}

#[rstest::rstest]
#[test]
fn refresh_scores_force_is_a_command() {
    // Given the score actor's forced-refresh contract.
    // When reading its schema definition.
    let def = <RefreshScoresForce as trouper::schema::Schema>::schema_def();

    // Then the name and kind are stable: it is an imperative
    // instruction, and the score actor is its only declarant, so the
    // route resolves to exactly that actor.
    assert_eq!(def.name, "RefreshScoresForce");
    assert!(matches!(def.kind, trouper::schema::SchemaKind::Command));
}

#[rstest::rstest]
#[test]
fn vals_loaded_is_an_event() {
    // Given the vals half of the score actor's load announcement.
    // When reading its schema definition.
    let def = <ValsLoaded as trouper::schema::Schema>::schema_def();

    // Then the name and kind are stable: it is a past-tense fact ("the
    // vals index landed in the cell"), so it is an Event.
    assert_eq!(def.name, "ValsLoaded");
    assert!(matches!(def.kind, trouper::schema::SchemaKind::Event));
}

#[rstest::rstest]
#[test]
fn market_models_refreshed_is_an_event() {
    // Given the refresh chain's contract.
    // When reading its schema definition.
    let def = <MarketModelsRefreshed as trouper::schema::Schema>::schema_def();

    // Then the name and kind are stable: it is a past-tense fact
    // ("the models table was refreshed"), so it is an Event and
    // broadcasts to its single declarant — the score actor.
    assert_eq!(def.name, "MarketModelsRefreshed");
    assert!(matches!(def.kind, trouper::schema::SchemaKind::Event));
}

#[rstest::rstest]
#[test]
fn market_models_refreshed_serializes_as_null() {
    // Given the chain event — a unit struct.
    // When serializing it the way the erased publish path does.
    let payload = serde_json::to_value(MarketModelsRefreshed).expect("the event serializes");

    // Then the payload is `null`, not `{}` — a unit struct's serde
    // representation. The erased publish path carries that value
    // verbatim, so a hand-rolled publish of `{}` would not match it.
    assert!(payload.is_null());
}

// ── Cell payload behavior ────────────────────────────────────────────

#[rstest::rstest]
fn ui_state_reset_restores_the_opener_defaults() {
    // Given UI state moved away from every default, detail mode included.
    let mut state = MarketUiState {
        selection: 7,
        filter: "sonnet".to_owned(),
        filter_focused: true,
        sort_col: SortColumn::Eci,
        sort_desc: true,
        help_visible: true,
        results_viewport: 42,
        fetch_seq: 9,
        detail_model: Some("openrouter/a/model".to_owned()),
        detail_selection: 3,
        detail_sort_col: DetailSort::Tp,
        detail_sort_desc: true,
        mix_source: MixSource::Usage,
        active_mix: (1_000, 0.5, 2_000),
    };

    // When the opener resets it.
    state.reset();

    // Then every display field is back to the defaults the contract
    // names: table mode, table focused, filter empty, PROJ ascending.
    assert_eq!(state.selection, 0);
    assert_eq!(state.filter, "");
    assert!(!state.filter_focused);
    assert_eq!(state.sort_col, SortColumn::Proj);
    assert!(!state.sort_desc);
    assert!(!state.help_visible);
    // And the measurement fields reset too: the viewport falls back to
    // its constant until the first frame re-measures it, and the fetch
    // sequence restarts.
    assert_eq!(state.results_viewport, RESULTS_VIEWPORT_FALLBACK);
    assert_eq!(state.fetch_seq, 0);
    // And the detail-mode fields reset to table mode: no pin, the
    // detail sort on its PROJ-ascending default.
    assert_eq!(state.detail_model, None);
    assert_eq!(state.detail_selection, 0);
    assert_eq!(state.detail_sort_col, DetailSort::Proj);
    assert!(!state.detail_sort_desc);
    // And the mix returns to the fixed constant under the fixed source.
    assert_eq!(state.mix_source, MixSource::Fixed);
    assert_eq!(state.active_mix, PROJECTION_MIX);
}

#[rstest::rstest]
fn m_toggles_the_mix_source_and_survives_detail_round_trips() {
    // Given default UI state (fixed mix).
    let mut state = MarketUiState::default();
    assert_eq!(state.mix_source, MixSource::Fixed);

    // When `m` toggles and a detail round trip happens around it.
    state.toggle_mix_source();
    state.enter_detail("openrouter/a/model".to_owned());
    state.leave_detail();

    // Then the usage source survived the round trip, and toggling again
    // lands back on fixed.
    assert_eq!(state.mix_source, MixSource::Usage);
    state.toggle_mix_source();
    assert_eq!(state.mix_source, MixSource::Fixed);
}

#[rstest::rstest]
fn resolve_mix_uses_the_fixed_constant_for_the_fixed_source() {
    // Given usage facts that would produce a very different mix.
    let usage = Some((1_000_000, 500_000, 400_000, 800_000));

    // When resolving under the fixed source.
    let mix = resolve_mix(MixSource::Fixed, usage);

    // Then the shipped constant stands regardless of the usage.
    assert_eq!(mix, PROJECTION_MIX);
}

#[rstest::rstest]
fn resolve_mix_derives_from_measured_usage() {
    // Given a tree's measured numbers: 3.2M effective sent, 160k
    // received, 2.8M of 3.0M measured-sent tokens served from cache.
    let usage = Some((3_200_000, 160_000, 2_800_000, 3_000_000));

    // When resolving under the usage source.
    let mix = resolve_mix(MixSource::Usage, usage);

    // Then the mix is those numbers with the hit ratio computed.
    assert_eq!(mix.0, 3_200_000);
    assert_eq!(mix.2, 160_000);
    assert!((mix.1 - (2_800_000f64 / 3_000_000f64)).abs() < 1e-9);
}

#[rstest::rstest]
fn resolve_mix_degrades_to_fixed_without_measured_requests() {
    // Given usage with zero measured-sent tokens (a fresh session:
    // everything projects to $0.00, a useless ranking) and no usage.
    let fresh = Some((0, 0, 0, 0));

    // When resolving either way under the usage source.
    let fresh_mix = resolve_mix(MixSource::Usage, fresh);
    let none_mix = resolve_mix(MixSource::Usage, None);

    // Then both degrade to the fixed constant, never to a zero mix.
    assert_eq!(fresh_mix, PROJECTION_MIX);
    assert_eq!(none_mix, PROJECTION_MIX);
}

#[rstest::rstest]
fn enter_detail_pins_the_model_and_resets_the_detail_side_only() {
    // Given UI state with table-side state the round trip must preserve.
    let mut state = MarketUiState {
        filter: "son".to_owned(),
        filter_focused: true,
        sort_col: SortColumn::Eci,
        sort_desc: true,
        selection: 2,
        detail_selection: 5,
        detail_sort_col: DetailSort::Tp,
        detail_sort_desc: true,
        ..MarketUiState::default()
    };

    // When detail mode is entered for a model.
    state.enter_detail("openrouter/a/model".to_owned());

    // Then the mode is pinned, and the detail-side state is on its
    // defaults.
    assert_eq!(state.detail_model, Some("openrouter/a/model".to_owned()));
    assert_eq!(state.detail_selection, 0);
    assert_eq!(state.detail_sort_col, DetailSort::Proj);
    assert!(!state.detail_sort_desc);
    // And the table side is untouched.
    assert_eq!(state.filter, "son");
    assert!(state.filter_focused);
    assert_eq!(state.sort_col, SortColumn::Eci);
    assert!(state.sort_desc);
    assert_eq!(state.selection, 2);
}

#[rstest::rstest]
fn leave_detail_clears_the_pin_and_keeps_the_table_state() {
    // Given detail mode holding table-side state.
    let mut state = MarketUiState {
        detail_model: Some("openrouter/a/model".to_owned()),
        detail_selection: 4,
        detail_sort_desc: true,
        filter: "son".to_owned(),
        sort_col: SortColumn::In,
        selection: 1,
        ..MarketUiState::default()
    };

    // When detail mode is left.
    state.leave_detail();

    // Then the pin is gone: table mode again.
    assert_eq!(state.detail_model, None);
    // And the table side is untouched.
    assert_eq!(state.filter, "son");
    assert_eq!(state.sort_col, SortColumn::In);
    assert_eq!(state.selection, 1);
}

#[rstest::rstest]
fn projection_mix_is_the_fixed_contract_mix() {
    // Given the projection constant the slice's contract names.

    // When read.
    let (input, hit, output) = PROJECTION_MIX;

    // Then it is the locked decision: 10M input tokens, 98% cache-hit
    // fraction, 50k output tokens.
    assert_eq!(input, 10_000_000);
    assert!((hit - 0.98).abs() < f64::EPSILON);
    assert_eq!(output, 50_000);
}

#[rstest::rstest]
#[case(SortColumn::Proj, SortColumn::In)]
#[case(SortColumn::In, SortColumn::Out)]
#[case(SortColumn::Out, SortColumn::Cache)]
#[case(SortColumn::Cache, SortColumn::Eci)]
#[case(SortColumn::Eci, SortColumn::Vals)]
#[case(SortColumn::Vals, SortColumn::Model)]
#[case(SortColumn::Model, SortColumn::Proj)]
fn sort_cycle_advances_left_to_right_wrapping(#[case] from: SortColumn, #[case] to: SortColumn) {
    // Given a sort column and the column that follows it in the cycle.

    // When the cycle advances.
    let next = from.next();

    // Then the next column is the expected one.
    assert_eq!(next, to);
    // And the cycle is symmetric.
    assert_eq!(to.prev(), from);
}

#[rstest::rstest]
#[case(DetailSort::Endpoint, DetailSort::In)]
#[case(DetailSort::In, DetailSort::Out)]
#[case(DetailSort::Out, DetailSort::Cache)]
#[case(DetailSort::Cache, DetailSort::Proj)]
#[case(DetailSort::Proj, DetailSort::Lat)]
#[case(DetailSort::Lat, DetailSort::Tp)]
#[case(DetailSort::Tp, DetailSort::Up)]
#[case(DetailSort::Up, DetailSort::Endpoint)]
fn detail_sort_cycle_advances_in_header_order_wrapping(
    #[case] from: DetailSort,
    #[case] to: DetailSort,
) {
    // Given a detail sort and the sort that follows it in the cycle.

    // When the cycle advances.
    let next = from.next();

    // Then the next sort is the expected one.
    assert_eq!(next, to);
    // And the cycle is symmetric.
    assert_eq!(to.prev(), from);
}

#[rstest::rstest]
fn detail_sort_headers_name_the_eight_detail_columns() {
    // Given the eight detail sorts in cycle order.

    // When their headers are read.
    let headers: Vec<&str> = DETAIL_SORT_CYCLE.iter().map(|col| col.header()).collect();

    // Then each names its detail column, in header order.
    assert_eq!(
        headers,
        vec![
            "ENDPOINT", "IN /M", "OUT /M", "CACHE /M", "PROJ", "LAT p50", "TP p50", "UP 1d"
        ]
    );
}

#[rstest::rstest]
fn eci_index_lookups_key_on_the_display_name() {
    // Given an index with one model version.
    let mut index = EciIndex::default();
    index.scores.insert(
        "Claude Sonnet 4.5".to_owned(),
        EciScore {
            eci: 42.0,
            ci_low: 40.0,
            ci_high: 44.0,
        },
    );

    // When looking the name up.
    let score = index.get("Claude Sonnet 4.5");

    // Then the score and its interval come back.
    assert_eq!(
        score,
        Some(&EciScore {
            eci: 42.0,
            ci_low: 40.0,
            ci_high: 44.0,
        })
    );
    // And a missing name resolves to nothing (the `—` + `#` path).
    assert!(index.get("Unmatched Model").is_none());
    // And the census reflects the single entry.
    assert_eq!(index.len(), 1);
    assert!(!index.is_empty());
}

// ── MarketRow round-trip (the disk-cache payload) ────────────────────

#[rstest::rstest]
fn market_row_roundtrips_through_json() {
    // Given a row with every optional field set.
    let row = MarketRow {
        id: "openrouter/anthropic/claude-sonnet-4.5:thinking".to_owned(),
        author: "anthropic".to_owned(),
        slug: "claude-sonnet-4.5".to_owned(),
        variant: Some("thinking".to_owned()),
        prompt_price: 3e-6,
        completion_price: 1.5e-5,
        cache_read_price: Some(3e-7),
        has_overrides: true,
        context_length: Some(200_000),
        eci: Some(42.0),
        eci_ci: Some((40.0, 44.0)),
        vals: Some(58.4),
        is_free: false,
    };

    // When serializing and deserializing.
    let json = serde_json::to_string(&row).expect("row serializes");
    let back: MarketRow = serde_json::from_str(&json).expect("row deserializes");

    // Then the roundtrip is lossless.
    assert_eq!(back, row);
}

#[rstest::rstest]
fn market_data_state_defaults_to_a_clean_table() {
    // Given a fresh data cell payload.

    // When the default payload is read.
    let state = MarketDataState::default();

    // Then nothing has loaded: empty rows, no indexes, no stamps, no
    // in-flight flags, seed ECI source, unset vals source, no error.
    assert_eq!(state.rows, [] as [row::MarketRow; 0]);
    assert!(state.detail.is_none());
    assert!(state.eci.is_none());
    assert!(state.vals.is_none());
    assert!(state.models_fetched_at.is_none());
    assert!(state.eci_fetched_at.is_none());
    assert!(state.vals_fetched_at.is_none());
    assert!(state.endpoints_fetched_at.is_none());
    assert!(!state.models_loading);
    assert!(!state.endpoints_loading);
    assert!(!state.eci_loading);
    assert!(!state.vals_loading);
    assert_eq!(state.eci_source, EciSource::Seed);
    assert_eq!(state.vals_source, ValsSource::None);
    assert!(state.last_error.is_none());
}

#[rstest::rstest]
fn vals_index_lookups_key_on_the_provider_model_slug() {
    // Given an index with one model.
    let mut index = ValsIndex::default();
    index.scores.insert(
        "anthropic/claude-sonnet-5-5".to_owned(),
        ValsScore { accuracy: 71.309 },
    );

    // When looking the slug up.
    let score = index.get("anthropic/claude-sonnet-5-5");

    // Then the accuracy comes back.
    assert_eq!(score, Some(&ValsScore { accuracy: 71.309 }));
    // And a missing slug resolves to nothing (the `—` + `#` path).
    assert!(index.get("unmatched/model").is_none());
    // And the census reflects the single entry.
    assert_eq!(index.len(), 1);
    assert!(!index.is_empty());
}

#[rstest::rstest]
fn market_row_roundtrips_the_vals_field_and_defaults_it_for_old_caches() {
    // Given a row with the vals field set, serialized to JSON.
    let row = MarketRow {
        id: "openrouter/z-ai/glm-5.3".to_owned(),
        author: "z-ai".to_owned(),
        slug: "glm-5.3".to_owned(),
        variant: None,
        prompt_price: 1e-6,
        completion_price: 2e-6,
        cache_read_price: None,
        has_overrides: false,
        context_length: None,
        eci: None,
        eci_ci: None,
        vals: Some(58.4),
        is_free: false,
    };
    let json = serde_json::to_string(&row).expect("row serializes");

    // When deserializing it back, and also deserializing a cache payload
    // written before the field existed (no `vals` key).
    let back: MarketRow = serde_json::from_str(&json).expect("row deserializes");
    let legacy = "{\"id\":\"openrouter/a/b\",\"author\":\"a\",\"slug\":\"b\",\
        \"prompt_price\":1e-6,\"completion_price\":2e-6,\"cache_read_price\":null,\
        \"has_overrides\":false,\"context_length\":null,\"eci\":null,\"eci_ci\":null,\
        \"is_free\":false}";

    // Then the roundtrip carries the accuracy, and the legacy payload
    // defaults the field to None instead of failing to load.
    assert_eq!(back.vals, Some(58.4));
    let loaded: MarketRow = serde_json::from_str(legacy).expect("legacy cache loads");
    assert_eq!(loaded.vals, None);
}
