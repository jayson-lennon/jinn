//! The model market's server side: the handlers behind its five client
//! messages, the row builders that project the data cell into wire rows,
//! and the pushes that answer an open market when its data lands.
//!
//! Parity source: the terminal's market popup (`jinn-model-market`). The
//! filter, the sort, the projection, and the formatters are the contract
//! crate's — the same functions the popup's table renders through — so
//! the page's rows are the popup's rows. This module adds no data path
//! of its own: it reads the market slice's data cell and publishes the
//! popup's own commands (`FetchModels`, `FetchEndpoints`, `RefreshAll`,
//! `ProviderSwitch`, `SetEndpointDefault`).
//!
//! The web never touches the popup's UI cell. `FetchEndpoints.seq` is
//! stamped `u64::MAX` so the actor's stale-sequence drop — which compares
//! against that cell's counter — can never cancel a web fetch.

use jinn_core_types::{ModelSelection, SessionId};
use jinn_kernel::common::actor_deps::BusPublish as _;
use jinn_model_market_msg::{
    FetchEndpoints, FetchModels, MarketDataState, MarketEndpointRow, MarketRow, MixSource,
    PROJECTION_MIX, RefreshAll, SortColumn, cache_column, dollars, eci_column, filter_and_sort,
    ms_column, name_runs, percent_column, price_per_million, projected_cents, projected_cents_for,
    resolve_mix, throughput_column, vals_column,
};
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_provider_selection_msg::{ProviderSwitch, SetEndpointDefault};
use jinn_session_state::aggregate_tree_stats;
use jinn_webui2_msg::{
    MarketDetailBody, MarketDirBody, MarketEndpointBody, MarketMixBody, MarketRowBody,
    MarketRowsBody, MarketSortBody, MarketStaleBody, MarketViewBody, ServerMsg, TextRunBody,
};

use crate::{
    actions::{Refusal, refused},
    outbound::OutboundHandle,
    protocol::Reply,
};

/// The sequence the web stamps on an endpoint fetch. The actor drops a
/// fetch whose sequence is below the terminal popup's counter after its
/// debounce; the maximum is never below it, so a popup opened at the same
/// moment can never cancel a web fetch.
const WEB_FETCH_SEQ: u64 = u64::MAX;

/// The mix the detail page's endpoint rows project under, once a view has
/// resolved it — the remembered view's, so the detail's PROJ agrees with
/// the list's.
type Mix = (u32, f64, u32);

/// Opens the market: answers the rows for `view` as the cell stands, and
/// on an empty catalog publishes the models fetch the terminal's opener
/// publishes — the actor serves a warm cache or the network, and the rows
/// land as a push when they do.
pub async fn open(out: &OutboundHandle, view: MarketViewBody) -> Reply {
    let session_id = remember_view(out, &view);
    let (body, empty) = rows_frame(out, &view, &session_id);
    if empty {
        out.publish(FetchModels).await;
    }
    Reply::one(ServerMsg::MarketRows(body))
}

/// Answers the rows for a new view — a filter, sort, or mix change. No
/// fetch: the open published it.
pub fn rows(out: &OutboundHandle, view: MarketViewBody) -> Reply {
    rows_consuming(out, view)
}

/// Asks for one model's endpoint detail. The cell answers at once when it
/// already holds this model's detail; otherwise the fetch is published and
/// the reply is the loading shape — the detail lands as a push.
pub async fn detail(out: &OutboundHandle, model_id: String) -> Reply {
    let Some(session_id) = active_session(out) else {
        return refused(&Refusal::NoActiveSession);
    };
    if !is_known_model(out, &model_id) {
        return refused(&Refusal::UnknownMarketModel(model_id.clone()));
    }
    out.remember_market_pinned(&model_id);
    let mix = remembered_mix(out);
    let (endpoints, loading) = {
        let cell = out.market_cell().read();
        match cell.detail.as_ref() {
            Some(detail) if detail.model_id == model_id => {
                (endpoint_bodies(&cell, &model_id, mix), false)
            }
            _ => (Vec::new(), true),
        }
    };
    if loading {
        out.publish(FetchEndpoints {
            model_id: model_id.clone(),
            seq: WEB_FETCH_SEQ,
        })
        .await;
    }
    Reply::one(ServerMsg::MarketDetail(detail_body(
        out,
        &session_id,
        &model_id,
        endpoints,
        loading,
    )))
}

/// Switches the active session's model to `model_id` and pins — or, with a
/// `None` tag, clears — its routing endpoint.
///
/// The terminal's detail-mode confirm, message for message: the switch,
/// the remembered last model, then the endpoint pin. The scope pop is
/// the page's own (it closes the market on the send).
pub async fn use_model(out: &OutboundHandle, model_id: String, tag: Option<String>) -> Reply {
    if !is_known_model(out, &model_id) {
        return refused(&Refusal::UnknownMarketModel(model_id.clone()));
    }
    let Some(session_id) = active_session(out) else {
        return refused(&Refusal::NoActiveSession);
    };
    let selection = ModelSelection::Single(model_id.clone());
    out.publish(ProviderSwitch {
        session_id,
        provider_id: selection.clone(),
    })
    .await;
    out.publish(UpdateAppState {
        updates: vec![AppStateUpdate::SetLastModel(Some(selection))],
    })
    .await;
    out.publish(SetEndpointDefault {
        model: model_id,
        tag,
    })
    .await;
    Reply::none()
}

/// Drops every market cache and refetches. The rows land as pushes.
pub async fn refresh(out: &OutboundHandle) -> Reply {
    out.publish(RefreshAll).await;
    Reply::none()
}

// ── Pushes (called by the outbound actor's handlers) ─────────────────

/// The catalog or a score index landed: an open market gets fresh rows
/// in the view it is holding. Nothing is pushed while none is open.
pub(crate) fn rows_changed(handle: &OutboundHandle) {
    let Some((view, session_id)) = handle.market_view() else {
        return;
    };
    let (body, _) = rows_frame(handle, &view, &session_id);
    handle.broadcast(&ServerMsg::MarketRows(body));
}

/// One model's endpoints landed: a detail page still showing that model
/// gets them. A page pinned to another model — the cell holds one
/// model's detail at a time — is left on its spinner.
pub(crate) fn detail_changed(handle: &OutboundHandle) {
    let Some(pinned) = handle.market_pinned() else {
        return;
    };
    let Some((view, session_id)) = handle.market_view() else {
        return;
    };
    let mix = resolve_mix(contract_mix(&view), Some(tree_usage(handle, &session_id)));
    let body = {
        let cell = handle.market_cell().read();
        match cell.detail.as_ref() {
            Some(detail) if detail.model_id == pinned => detail_body(
                handle,
                &session_id,
                &pinned,
                endpoint_bodies(&cell, &pinned, mix),
                false,
            ),
            _ => return,
        }
    };
    handle.broadcast(&ServerMsg::MarketDetail(body));
}

/// A refresh step failed: every page with a market open is told, and the
/// stale rows stand.
pub(crate) fn refresh_failed(handle: &OutboundHandle, message: &str) {
    if handle.market_view().is_none() {
        return;
    }
    handle.broadcast(&ServerMsg::MarketStale(MarketStaleBody {
        message: message.to_owned(),
    }));
}

// ── Row builders ─────────────────────────────────────────────────────

/// Records the view and the session it was asked from, and answers that
/// session's id — the one the frame echoes and the mix derives from.
fn remember_view(out: &OutboundHandle, view: &MarketViewBody) -> SessionId {
    let session_id = active_session(out).unwrap_or_default();
    out.remember_market_view(view.clone(), session_id.clone());
    session_id
}

/// `rows` consumes its view: the frame echoes a clone and the handle
/// remembers the original.
fn rows_consuming(out: &OutboundHandle, view: MarketViewBody) -> Reply {
    let session_id = active_session(out).unwrap_or_default();
    let (body, _) = rows_frame(out, &view, &session_id);
    out.remember_market_view(view, session_id.clone());
    Reply::one(ServerMsg::MarketRows(body))
}

fn active_session(out: &OutboundHandle) -> Option<SessionId> {
    let state = out.state();
    let guard = state.read();
    let id = guard.session.active_session_id().clone();
    guard.session.get(&id).map(|_| id)
}

fn is_known_model(out: &OutboundHandle, model_id: &str) -> bool {
    out.market_cell()
        .read()
        .rows
        .iter()
        .any(|row| row.id == model_id)
}

/// The mix the remembered view resolves to, for a detail asked outside a
/// rows frame — the constant when no market is open.
fn remembered_mix(out: &OutboundHandle) -> Mix {
    out.market_view()
        .map_or(PROJECTION_MIX, |(view, session_id)| {
            resolve_mix(contract_mix(&view), Some(tree_usage(out, &session_id)))
        })
}

/// The rows frame for one view, and whether the catalog is empty (the
/// open's cue to publish the fetch).
fn rows_frame(
    out: &OutboundHandle,
    view: &MarketViewBody,
    session_id: &SessionId,
) -> (MarketRowsBody, bool) {
    let mix = resolve_mix(contract_mix(view), Some(tree_usage(out, session_id)));
    let (rows, total, loading, updated, active_id) = {
        let cell = out.market_cell().read();
        let active_id = active_model_id(out);
        let rows: Vec<MarketRowBody> = filter_and_sort(
            &cell.rows,
            &view.filter,
            contract_sort(view),
            contract_desc(view),
            mix,
        )
        .iter()
        .map(|row| row_body(row, &view.filter, mix, active_id.as_deref()))
        .collect();
        let updated = cell
            .models_fetched_at
            .map_or_else(String::new, jinn_model_market_msg::age_stamp);
        (
            rows,
            cell.rows.len(),
            cell.models_loading,
            updated,
            active_id,
        )
    };
    let body = MarketRowsBody {
        session_id: session_id.clone(),
        view: view.clone(),
        total: u32::try_from(total).unwrap_or(u32::MAX),
        rows,
        loading,
        updated: (!updated.is_empty()).then_some(updated),
        mix_label: mix_label(mix),
        active_id,
    };
    (body, total == 0)
}

/// The session tree's measured usage for the mix: the four numbers the
/// terminal's render facts seed, from the same aggregate.
fn tree_usage(out: &OutboundHandle, session_id: &SessionId) -> (u64, u64, u64, u64) {
    let state = out.state();
    let guard = state.read();
    let tree = aggregate_tree_stats(
        guard.session.sessions(),
        guard.session.frozen_nodes(),
        session_id,
        &out.work_time_cell().read().snapshot(),
    );
    (
        tree.effective_sent,
        tree.total_received,
        tree.cached_total,
        tree.measured_sent,
    )
}

/// The active session's model id when it is a single selection — the row
/// the list marks. `None` for an alloy or no selection.
fn active_model_id(out: &OutboundHandle) -> Option<String> {
    let state = out.state();
    let guard = state.read();
    let session = guard.session.get(guard.session.active_session_id())?;
    match session.model_selection() {
        // The default selection is the no-provider sentinel, not a model:
        // it marks no row.
        ModelSelection::Single(id) if id != jinn_core_types::NO_PROVIDER_ID => Some(id.clone()),
        ModelSelection::Single(_) | ModelSelection::Alloy { .. } => None,
    }
}

/// The status line's mix label: the source word and the numbers, as the
/// terminal's status line renders them — `fixed` whenever the resolved
/// mix equals the constant, including the usage fallback on an
/// unmeasured tree.
fn mix_label(mix: Mix) -> String {
    let (input, hit, output) = mix;
    let source = if mix == PROJECTION_MIX {
        "fixed"
    } else {
        "tree"
    };
    format!(
        "{source} {}M·{}%·{}k",
        input / 1_000_000,
        (hit * 100.0) as u32,
        output / 1_000
    )
}

fn contract_sort(view: &MarketViewBody) -> SortColumn {
    match view.sort {
        MarketSortBody::Proj => SortColumn::Proj,
        MarketSortBody::In => SortColumn::In,
        MarketSortBody::Out => SortColumn::Out,
        MarketSortBody::Cache => SortColumn::Cache,
        MarketSortBody::Eci => SortColumn::Eci,
        MarketSortBody::Vals => SortColumn::Vals,
        MarketSortBody::Model => SortColumn::Model,
    }
}

fn contract_desc(view: &MarketViewBody) -> bool {
    matches!(view.dir, MarketDirBody::Desc)
}

fn contract_mix(view: &MarketViewBody) -> MixSource {
    match view.mix {
        MarketMixBody::Fixed => MixSource::Fixed,
        MarketMixBody::Usage => MixSource::Usage,
    }
}

/// One listing as the wire carries it.
fn row_body(row: &MarketRow, filter: &str, mix: Mix, active: Option<&str>) -> MarketRowBody {
    MarketRowBody {
        id: row.id.clone(),
        author: row.author.clone(),
        slug: row.slug.clone(),
        variant: row.variant.clone(),
        name_runs: name_runs(row, filter)
            .into_iter()
            .map(|run| TextRunBody {
                text: run.text,
                matched: run.matched,
            })
            .collect(),
        proj: dollars(projected_cents(row, mix)),
        in_m: price_per_million(row.prompt_price),
        out_m: price_per_million(row.completion_price),
        cache_m: cache_column(row.cache_read_price),
        eci: eci_column(row.eci),
        vals: vals_column(row.vals),
        is_active: active == Some(row.id.as_str()),
    }
}

/// The endpoint rows: the auto-route sentinel first, priced from the
/// model's listing, then every endpoint in fetch order.
fn endpoint_bodies(cell: &MarketDataState, model_id: &str, mix: Mix) -> Vec<MarketEndpointBody> {
    let mut rows: Vec<MarketEndpointBody> = cell
        .rows
        .iter()
        .filter(|row| row.id == model_id)
        .map(|listing| sentinel_body(listing, mix))
        .collect();
    let endpoints = cell
        .detail
        .as_ref()
        .filter(|detail| detail.model_id == model_id)
        .map_or(&[][..], |detail| detail.endpoints.as_slice());
    rows.extend(
        endpoints
            .iter()
            .map(|endpoint| endpoint_body(endpoint, mix)),
    );
    rows
}

/// The auto-route sentinel: the model's listing row, auto-routed. The
/// terminal's `DetailRow::Default`.
fn sentinel_body(listing: &MarketRow, mix: Mix) -> MarketEndpointBody {
    MarketEndpointBody {
        tag: None,
        name: "Default".to_owned(),
        proj: dollars(projected_cents(listing, mix)),
        in_m: price_per_million(listing.prompt_price),
        out_m: price_per_million(listing.completion_price),
        cache_m: cache_column(listing.cache_read_price),
        latency: ms_column(None),
        throughput: throughput_column(None),
        uptime: percent_column(None),
        is_default: true,
    }
}

/// One provider endpoint as the wire carries it.
fn endpoint_body(endpoint: &MarketEndpointRow, mix: Mix) -> MarketEndpointBody {
    MarketEndpointBody {
        tag: (!endpoint.tag.is_empty()).then(|| endpoint.tag.clone()),
        name: endpoint_name(endpoint),
        proj: dollars(projected_cents_for(
            endpoint.prompt_price,
            endpoint.completion_price,
            endpoint.cache_read_price,
            mix,
        )),
        in_m: price_per_million(endpoint.prompt_price),
        out_m: price_per_million(endpoint.completion_price),
        cache_m: cache_column(endpoint.cache_read_price),
        latency: ms_column(endpoint.latency_p50_ms),
        throughput: throughput_column(endpoint.throughput_p50),
        uptime: percent_column(endpoint.uptime_1d),
        is_default: false,
    }
}

/// An endpoint's display name: the provider's name, plus the tag when it
/// has one — the terminal's `endpoint_label`.
fn endpoint_name(endpoint: &MarketEndpointRow) -> String {
    if endpoint.tag.is_empty() {
        endpoint.provider_name.clone()
    } else {
        format!("{} ({})", endpoint.provider_name, endpoint.tag)
    }
}

/// The detail frame.
fn detail_body(
    out: &OutboundHandle,
    session_id: &SessionId,
    model_id: &str,
    endpoints: Vec<MarketEndpointBody>,
    loading: bool,
) -> MarketDetailBody {
    let (context_length, is_free, has_overrides) = out
        .market_cell()
        .read()
        .rows
        .iter()
        .find(|row| row.id == model_id)
        .map_or((None, false, false), |row| {
            (row.context_length, row.is_free, row.has_overrides)
        });
    let pinned_tag = out
        .services()
        .provider_registry
        .config_snapshot()
        .endpoint_defaults
        .iter()
        .find(|row| row.model == model_id)
        .map(|row| row.tag.clone());
    MarketDetailBody {
        session_id: session_id.clone(),
        model_id: model_id.to_owned(),
        context_length,
        is_free,
        has_overrides,
        endpoints,
        loading,
        pinned_tag,
    }
}
