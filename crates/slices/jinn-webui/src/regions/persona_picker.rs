//! The persona region — the hamburger menu's Persona item and its
//! modal.
//!
//! Parts declared: GET pull route + page fill. No SSE event of its own
//! (persona switching is request/response — the switch POST's response
//! is the correction path, and the pull is the correctness dependency)
//! and no settle render (the modal is pull-opened, never part of a
//! settle pass).
//!
//! The rows are TUI-parity: built by the shared persona row builder
//! (`jinn_persona_msg::PersonaRow::rows` over the personas cell) — the
//! same builder the TUI picker consumes — and filtered server-side by
//! the shared fuzzy core (`filter_rows`, the TUI's match core over
//! persona name and description). The filter re-queries on each
//! keystroke through the pull route; matched spans render in `<mark>`
//! runs from the shared builder's segment split.
//!
//! The modal fragment renders the row list and the filter input for
//! the viewed session, active row marked. A switch POST publishes the
//! same message set the TUI confirm publishes
//! (`UpdateAppState(SetPersona)` + `MarkSessionInteracted`) and
//! answers with the re-rendered modal; when the chosen persona has no
//! `<compaction>` block, the response also carries the transcript so
//! the loud error entry is visible without a reload (the delivery
//! path's job, in `delivery.rs`).
//!
//! The menu item lives in the menu region (this feature's opening
//! gesture); this region owns the modal fragment and its pull.

use axum::extract::Path as AxPath;
use axum::extract::Query as AxQuery;
use axum::extract::State as AxumState;
use jinn_persona_msg::filter_rows;
use jinn_persona_msg::PersonaRow;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The persona region.
pub struct PersonaRegion;

impl WebRegion for PersonaRegion {
    fn id(&self) -> &'static str {
        "persona-picker"
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
            pattern: crate::contract::PERSONA_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::PERSONA_MODAL_HOST_ID)
    }

    fn css(&self) -> &'static str {
        PERSONA_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the modal reads the
        // personas cell through `ServerShared`, not session state, so
        // its fragment cannot come from a guard. The trait method
        // exists for registry completeness; it renders nothing.
        String::new()
    }
}

/// The pull's query: the optional `filter` parameter — the shared
/// fuzzy filter over persona name and description.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct PersonaFilterQuery {
    /// The filter text; absent or empty renders every row.
    #[serde(default)]
    pub filter: Option<String>,
}

/// `GET /web/persona-picker/{id}?filter=…` — the persona modal
/// fragment. The client pulls this on open (the menu item) and on
/// every filter keystroke; the server applies the filter before
/// rendering, so the wire carries only matching rows.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
    AxQuery(query): AxQuery<PersonaFilterQuery>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    let filter = query.filter.unwrap_or_default();
    Ok(
        crate::server::Fragment::new(modal_fragment(&shared, &session_id, &filter)?)
            .with_swap(crate::contract::PERSONA_MODAL_HOST_SELECTOR, "inner"),
    )
}

/// The session's current persona name, read under one short-lived
/// guard (dropped before any await, per the guard discipline).
#[must_use]
fn current_persona(shared: &crate::server::ServerShared, session_id: &str) -> Option<String> {
    let id = jinn_core_types::SessionId::try_from_string(session_id)?;
    let guard = shared.read_state();
    guard
        .session
        .get(&id)
        .map(|session| session.persona_name().to_owned())
}

/// Builds the TUI-parity rows for one render: the shared builder over
/// the personas cell, then the shared fuzzy filter — the TUI's own
/// match core (`SkimMatcherV2`, per-term AND, score-descending order)
/// over the same search text, via the msg crate's `filter_rows`.
/// Guards are taken under one block and dropped before rendering.
///
/// The active mark is computed once here, at row build, exactly as the
/// TUI picker computes it.
#[must_use]
fn persona_rows(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Vec<jinn_persona_msg::FilteredPersonaRow> {
    let active = current_persona(shared, session_id);
    let rows: Vec<PersonaRow> = {
        let personas = shared.personas_cell().read();
        PersonaRow::rows(&personas.entries, active.as_deref())
    };
    filter_rows(&rows, filter)
}

/// The modal fragment: the overlay with the filter input and the
/// filtered TUI-parity rows.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn modal_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
    filter: &str,
) -> Result<String, crate::server::HandlerError> {
    let rows: Vec<String> = persona_rows(shared, session_id, filter)
        .into_iter()
        .map(|filtered| {
            crate::templates::render(&crate::templates::PersonaRowTemplate::from_filtered(
                filtered,
            ))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "persona-row", "webui template render failed; \
                     skipping the row");
                String::new()
            })
        })
        .collect();
    crate::templates::render(&crate::templates::PersonaModalTemplate::with_contract(
        rows,
        filter.to_owned(),
    ))
    .map_err(|error| crate::server::map_render_error(&error))
}

/// The persona region's stylesheet: the modal overlay, the filter bar,
/// and the rows. The modal geometry mirrors the model-picker modal's
/// (the same overlay language, at the same stack level); the rows add
/// the active mark styling.
pub const PERSONA_CSS: &str = r#"
.persona-modal { position: fixed; inset: 0; background: rgba(0,0,0,.55);
                 display: flex; align-items: flex-start;
                 justify-content: center; padding-top: 10vh; z-index: 40; }
.persona-modal[hidden] { display: none; }
.persona-modal-inner { background: var(--panel); border: 1px solid var(--border);
                       border-radius: 8px; width: min(560px, 92vw);
                       max-height: 70vh; display: flex; flex-direction: column; }
.persona-modal-head { display: flex; gap: 8px; padding: 10px;
                      border-bottom: 1px solid var(--border); }
#persona-filter { flex: 1; background: var(--bg); color: var(--fg);
                  border: 1px solid var(--border); border-radius: 6px;
                  padding: 6px 8px; font: inherit; }
#persona-rows { overflow-y: auto; padding: 4px 0; }
.persona-row { display: flex; gap: 8px; padding: 7px 12px; cursor: pointer; }
.persona-row:hover { background: #23272f; }
.persona-row .persona-row-marker { width: 1.2em; color: var(--muted); }
.persona-row .persona-row-description { color: var(--muted); }
.persona-row mark { background: transparent; color: var(--accent);
                    font-weight: 600; }
.persona-row.persona-row-active { background: #20242c; }
.persona-row.persona-row-active .persona-row-name { font-weight: 600; }
.persona-row.persona-row-active .persona-row-marker { color: var(--accent); }
@media (max-width: 767px) {
  .persona-row { min-height: 40px; padding: 10px 12px; }
  #persona-filter { min-height: 40px; }
}
"#;

#[cfg(test)]
mod persona_region_tests;
