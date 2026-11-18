//! The model-picker region — the select in the titlebar.
//!
//! Parts declared: page fill only. No SSE event, deliberately: a
//! model switch is per-POST (the response re-renders the picker with
//! the new selection), and no convergence event exists to re-render
//! it — a second viewer's picker updates the next time that page
//! pulls. No guard render: the picker reads the provider cell, not
//! the session state, so its fragment needs `ServerShared` and cannot
//! ride the fan-out's guard-render shape.
//!
//! Client half: `webui/client/src/regions/model_picker.ts` — the
//! change delegation on the contract's select and the
//! [`crate::contract::MODEL_PICKER_ID`] swap.

use axum::extract::Path as AxPath;
use axum::extract::State as AxumState;
use jinn_webui_msg::SseKind;

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
            sse_event: false,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[]
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
        // provider cell through `ServerShared`, not session state, so
        // its fragment cannot come from a guard. The trait method
        // exists for registry completeness; it renders nothing.
        String::new()
    }
}

/// `GET /web/model-picker/{id}` — the model-picker fragment.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let current = {
        let id = crate::server::parse_session_id(&session_id)?;
        let guard = shared.read_state();
        match guard.session.get(&id) {
            Some(session) => crate::regions::transcript::model_label(session.model_selection()),
            None => String::new(),
        }
    };
    Ok(crate::server::Fragment::new(model_picker_html(
        &shared,
        &session_id,
        &current,
    )?))
}

/// Renders the model-picker fragment: the cache's ids, sorted, with
/// the active selection marked.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub fn model_picker_html(
    shared: &crate::server::ServerShared,
    session_id: &str,
    current: &str,
) -> Result<String, crate::server::HandlerError> {
    let mut models: Vec<String> = {
        let guard = shared.read_provider();
        match guard.model_cache.as_ref() {
            Some(cache) => cache
                .entries
                .values()
                .flatten()
                .map(|info| info.id.clone())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    models.sort();
    models.dedup();
    crate::templates::render(&crate::templates::ModelTemplate::with_contract(
        session_id.to_owned(),
        current.to_owned(),
        models,
    ))
    .map_err(|e| crate::server::map_render_error(&e))
}

/// The model-picker's stylesheet: the titlebar select.
pub const MODEL_PICKER_CSS: &str = r#"
.model-picker { margin-left: auto; }
.model-picker select { background: var(--panel); color: var(--fg);
                       border: 1px solid var(--border); border-radius: 6px;
                       padding: 6px 8px; font: inherit; max-width: 260px; }
"#;
