//! The model-picker region — the titlebar button and its modal.
//!
//! Parts declared: GET pull route + SSE event (`models`) + page fill.
//! No settle render (the `models` event is pushed by the fan-out's
//! subscription, not a settle pass) and no guard render of its own: the
//! picker reads the provider registry, the API keys, and the provider
//! cell — none of which live on the state guard — so its fragments
//! render through [`ServerShared`].
//!
//! The rows are TUI-parity: built by the shared provider-row builder
//! (`jinn_provider_selection_msg::provider_rows`) — availability from
//! api keys, aliases resolved, remote cache models merged, sorted
//! available-first with the active selection promoted to the top of an
//! unfiltered list. The filter is server-side: the pull route accepts
//! `?filter=…` (a case-insensitive substring over model id and provider
//! name) and each keystroke re-queries; the client discards stale
//! responses (latest-wins).
//!
//! The button fragment renders the viewed session's current model; the
//! modal fragment renders the row list, the filter input, and the
//! refresh button. A switch POST publishes the existing
//! `ProviderSwitch` (`Single` only — alloys are out of scope for the
//! web) and answers with the re-rendered button.
//!
//! Client half: `webui/client/src/regions/model_picker.ts` — the
//! button click opens the modal, filter input re-pulls, refresh posts
//! and awaits the `models` SSE push, row click switches, Esc or an
//! outside click closes.

use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use jinn_provider_selection_msg::provider_rows::{load_provider_entries, sorted_entries};
use jinn_webui_msg::SseKind;
use serde::Deserialize;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The model-picker region.
pub struct ModelPickerRegion;

impl WebRegion for ModelPickerRegion {
    fn id(&self) -> &'static str {
        "model-picker"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: true,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::Models]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::MODEL_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::MODEL_PICKER_ID)
    }

    fn css(&self) -> &'static str {
        MODEL_PICKER_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the picker reads the
        // registry/api-keys/cell through `ServerShared`, not session
        // state, so its fragment cannot come from a guard. The trait
        // method exists for registry completeness; it renders nothing.
        String::new()
    }
}

/// The pull's query: the optional `filter` parameter, a
/// case-insensitive substring over model id and provider name.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelFilterQuery {
    /// The filter text; absent or empty renders every row.
    #[serde(default)]
    pub filter: Option<String>,
}

/// `GET /web/model-picker/{id}?filter=…` — the model-picker fragment.
///
/// The fragment is the modal (the overlay with filter input, refresh
/// button, and filtered rows); the button fragment is a separate
/// render (`button_fragment`). The client pulls this on open and on
/// every filter keystroke; the server applies the filter before
/// rendering, so the wire carries only matching rows.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<ModelFilterQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = query.filter.unwrap_or_default();
    Ok(crate::server::Fragment::new(modal_fragment(
        &shared,
        &session_id,
        &filter,
    )?))
}

/// The session's current model label, read under one short-lived
/// guard (dropped before any await, per the guard discipline).
fn current_model_label(shared: &crate::server::ServerShared, session_id: &str) -> String {
    let Some(id) = jinn_core_types::SessionId::try_from_string(session_id) else {
        return String::new();
    };
    let guard = shared.read_state();
    guard.session.get(&id).map_or_default(|session| {
        crate::regions::transcript::model_label(session.model_selection())
    })
}

/// Builds the TUI-parity rows for one render: the shared builder's
/// load + sort, then the server-side substring filter over the same
/// search text the TUI fuzzy-matches (`"{model} {provider_name}"`).
/// Guards are taken under one block and dropped before rendering.
fn picker_rows(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Vec<jinn_provider_selection_msg::ProviderPickerEntry> {
    let needle = filter.to_lowercase();

    let (active, theme) = {
        let guard = shared.read_state();
        let active = jinn_core_types::SessionId::try_from_string(session_id)
            .and_then(|id| guard.session.get(&id))
            .map_or_default(|session| {
                crate::regions::transcript::model_label(session.model_selection())
            });
        (active, guard.frontend.theme.clone())
    };

    let all = {
        let registry = shared.provider_registry().read();
        let api_keys = shared.api_keys().read();
        let cache = shared.read_provider().model_cache.clone();
        load_provider_entries(&registry, &api_keys, cache.as_ref(), &theme)
    };
    let sorted = sorted_entries(&all, &needle, &active);
    if needle.is_empty() {
        return sorted;
    }
    sorted
        .into_iter()
        .filter(|entry| entry.search_text.to_lowercase().contains(&needle))
        .collect()
}

/// The titlebar button fragment: the current model as a control.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub fn button_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
) -> Result<String, crate::server::HandlerError> {
    let current = current_model_label(shared, session_id);
    crate::templates::render(&crate::templates::ModelButtonTemplate::with_contract(
        session_id.to_owned(),
        current,
    ))
    .map_err(|e| crate::server::map_render_error(&e))
}

/// The modal fragment: the overlay with filter, refresh button, and
/// the filtered TUI-parity rows.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub fn modal_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let active = current_model_label(shared, session_id);
    let rows: Vec<String> = picker_rows(shared, session_id, filter)
        .into_iter()
        .map(|entry| {
            let is_active = entry.provider_id == active;
            crate::templates::render(&crate::templates::ModelRowTemplate {
                entry,
                session_id: session_id.to_owned(),
                is_active,
                model_attr: crate::contract::MODEL_ID_DATA_ATTR,
                marker_attr: crate::contract::MODEL_MARKER_ATTR,
                unavailable_attr: crate::contract::MODEL_UNAVAILABLE_ATTR,
                action_attr: crate::contract::ACTION_DATA_ATTR,
                model_action: crate::contract::ACTION_MODEL,
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "model-row", "webui template render failed; \
                     skipping the row");
                String::new()
            })
        })
        .collect();
    crate::templates::render(&crate::templates::ModelModalTemplate::with_contract(
        session_id.to_owned(),
        rows,
        filter.to_owned(),
    ))
    .map_err(|e| crate::server::map_render_error(&e))
}

/// The model-picker's stylesheet: the titlebar button, the modal
/// overlay, the filter bar, the refresh control, and the rows.
pub const MODEL_PICKER_CSS: &str = r#"
.model-picker { margin-left: auto; }
.model-picker-button { background: var(--panel); color: var(--fg);
                       border: 1px solid var(--border); border-radius: 6px;
                       padding: 6px 10px; font: inherit; max-width: 260px;
                       cursor: pointer; }
.model-picker-button:hover { border-color: var(--accent); }
.model-picker-modal { position: fixed; inset: 0; background: rgba(0,0,0,.55);
                      display: flex; align-items: flex-start;
                      justify-content: center; padding-top: 10vh; z-index: 40; }
.model-picker-modal[hidden] { display: none; }
.model-picker-modal-inner { background: var(--panel); border: 1px solid var(--border);
                            border-radius: 8px; width: min(560px, 92vw);
                            max-height: 70vh; display: flex; flex-direction: column; }
.model-picker-modal-head { display: flex; gap: 8px; padding: 10px;
                           border-bottom: 1px solid var(--border); }
.model-picker-filter { flex: 1; background: var(--bg); color: var(--fg);
                       border: 1px solid var(--border); border-radius: 6px;
                       padding: 6px 8px; font: inherit; }
.model-picker-refresh { background: var(--panel); color: var(--fg);
                        border: 1px solid var(--border); border-radius: 6px;
                        padding: 6px 10px; font: inherit; cursor: pointer; }
.model-picker-refresh:hover { border-color: var(--accent); }
.model-picker-rows { overflow-y: auto; padding: 4px 0; }
.model-row { display: flex; gap: 8px; padding: 7px 12px; cursor: pointer; }
.model-row:hover { background: #23272f; }
.model-row .model-row-marker { width: 1.2em; color: var(--muted); }
.model-row .model-row-provider { color: var(--muted); }
.model-row.model-row-active { background: #20242c; }
.model-row.model-row-active .model-row-model { font-weight: 600; }
.model-row.model-row-unavailable { cursor: default; color: var(--muted); }
.model-row.model-row-unavailable:hover { background: transparent; }
"#;
