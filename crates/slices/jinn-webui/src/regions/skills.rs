//! The skills region — the hamburger menu's Skills item and its
//! modal.
//!
//! Parts declared: GET pull route + SSE event. No settle render (the
//! modal is pull-opened, never part of a settle pass) and no page
//! fill (the modal swaps into its own host span, like the persona and
//! MCP modals — `container_id` is `None` because the host is not one
//! of the page shell's inline regions).
//!
//! The rows are TUI-parity and unfiltered: one row per skill in the
//! viewed session's discovered set, each with the enablement mark
//! (●/○), name, description, and the project-scoped source badge (the
//! TUI badge rule: project skills badge, global skills do not). Rows
//! build from the msg-crate [`jinn_skills_msg::Skill`] plus the
//! session's skill accessors — never the TUI's `SkillEntry`, which
//! carries the theme and feeds ratatui line rendering. Loaded-ness
//! has no badge, by parity: the TUI row shows none either (the picker
//! figures loaded-ness in its excluded footer); a user discovers it
//! through the force-load notice.
//!
//! A toggle POST (in `delivery.rs`) applies `NameFilter::permit` /
//! `withhold` to the session's skill filter synchronously — the
//! mode-preserving operations, so an allow-mode filter survives — and
//! publishes nothing (the TUI confirm publishes nothing; the skill
//! gate reads the filter live). A force-load POST repeats the TUI
//! `<c-l>` load verbatim: pinned `skill` tool-call/result pair,
//! durable auto-enable when disabled, `MarkSessionInteracted`, and
//! the duplicate's "already loaded" transient.
//!
//! Async discovery rides the push seam: the scan actor emits
//! `SkillsLoaded` after writing the session's ephemeral set, the
//! fan-out subscribes that event and bumps the `skillsTick` signal;
//! an open modal's effect re-pulls its rows (the mcpTick pattern —
//! discovery lands async after the modal can be opened). The GET
//! pull route remains the renderer; the pushed frame is a signal
//! only.
//!
//! The menu item lives in the menu region (this feature's opening
//! gesture); this region owns the modal fragment and its pull.

use axum::extract::Path as AxPath;
use axum::extract::State as AxumState;
use jinn_skills_msg::Skill;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The skills region.
pub struct SkillsRegion;

impl WebRegion for SkillsRegion {
    fn id(&self) -> &'static str {
        "skills"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: false,
            pull_route: true,
            sse_event: true,
            page_fill: false,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::Skills]
    }

    fn pull(&self) -> Option<RegionPull> {
        Some(RegionPull {
            pattern: crate::contract::SKILL_PICKER_ROUTE,
            handler: {
                use axum::routing::get;
                get(pull_handler)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        // The modal swaps into its own host span in the page shell
        // (`SKILL_MODAL_HOST_ID`), not a page-shell inline region —
        // the host-not-container rule every modal follows, so opening
        // it can never destroy the menu item that opened it.
        None
    }

    fn css(&self) -> &'static str {
        SKILL_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        // Declared without the guard-render part: the modal reads the
        // session's discovered set and filter through `ServerShared`
        // (two separate short guards, dropped before rendering), not
        // one guard, so its fragment cannot come from a guard. The
        // trait method exists for registry completeness; it renders
        // nothing.
        String::new()
    }
}

/// `GET /web/skills/{id}` — the skills modal fragment. The client
/// pulls this on open (the menu item) and on every `skillsTick` bump;
/// the server renders the viewed session's discovered skills as they
/// stand, so the wire always carries the live picture.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
async fn pull_handler(
    AxumState(shared): AxumState<crate::server::ServerShared>,
    AxPath(session_id): AxPath<String>,
) -> Result<crate::server::Fragment, crate::server::HandlerError> {
    Ok(
        crate::server::Fragment::new(modal_fragment(&shared, &session_id)?)
            .with_swap(crate::contract::SKILL_MODAL_HOST_SELECTOR, "inner"),
    )
}

/// One row's read: the discovered skill and whether it passes the
/// session's current skill filter (the enablement mark).
#[derive(Debug, Clone)]
struct SkillRowRead {
    /// The discovered skill.
    skill: Skill,
    /// Whether the skill passes the session's current filter — the
    /// `is_skill_enabled` predicate, the same one the prompt
    /// assembler applies.
    enabled: bool,
}

/// The viewed session's discovered skills with their enablement,
/// read under two short-lived guards (dropped before rendering, per
/// the guard discipline). An unknown session renders no rows — the
/// pull for a session that died mid-view is an empty modal, not an
/// error.
#[must_use]
fn skill_rows(shared: &crate::server::ServerShared, session_id: &str) -> Vec<SkillRowRead> {
    let Some(id) = jinn_core_types::SessionId::try_from_string(session_id) else {
        return Vec::new();
    };
    let guard = shared.read_state();
    guard.session.get(&id).map_or_default(|session| {
        session
            .discovered_skills()
            .iter()
            .map(|skill| SkillRowRead {
                skill: skill.clone(),
                enabled: session.is_skill_enabled(&skill.name),
            })
            .collect::<Vec<_>>()
    })
}

/// Builds the web rows for one render: [`jinn_skills_msg::Skill`]
/// plus each row's enablement, sorted case-insensitively by name (the
/// shared builders' order the persona and MCP modals render in).
#[must_use]
fn sorted_rows(shared: &crate::server::ServerShared, session_id: &str) -> Vec<(Skill, bool)> {
    let mut rows: Vec<(Skill, bool)> = skill_rows(shared, session_id)
        .into_iter()
        .map(|read| (read.skill, read.enabled))
        .collect();
    rows.sort_by_key(|(skill, _)| skill.name.to_lowercase());
    rows
}

/// The modal fragment: the overlay with the unfiltered TUI-parity
/// rows — enablement mark, name, description, source badge, and the
/// per-row force-load button.
///
/// # Errors
///
/// Returns the render error; the caller maps it to a 500.
pub(crate) fn modal_fragment(
    shared: &crate::server::ServerShared,
    session_id: &str,
) -> Result<String, crate::server::HandlerError> {
    let rows: Vec<String> = sorted_rows(shared, session_id)
        .into_iter()
        .map(|(skill, enabled)| {
            crate::templates::render(&crate::templates::SkillRowTemplate::from_skill(
                skill, enabled,
            ))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "skill-row", "webui template render failed; \
                     skipping the row");
                String::new()
            })
        })
        .collect();
    crate::templates::render(&crate::templates::SkillModalTemplate::with_contract(rows))
        .map_err(|error| crate::server::map_render_error(&error))
}

/// The skills region's stylesheet: the modal overlay and the rows.
/// The modal geometry mirrors the MCP modal's (the same overlay
/// language, at the same stack level — registered after it, so ties
/// resolve in this region's favor); the rows add the description
/// fill, the source badge, and the force-load button.
pub const SKILL_CSS: &str = r#"
.skill-modal { position: fixed; inset: 0; background: var(--scrim);
               display: flex; align-items: flex-start;
               justify-content: center; padding-top: 10vh; z-index: 40; }
.skill-modal[hidden] { display: none; }
.skill-modal-inner { background: var(--panel); border: 1px solid var(--border);
                     border-radius: 8px; width: min(560px, 92vw);
                     max-height: 70vh; display: flex; flex-direction: column; }
.skill-modal-head { padding: 10px; border-bottom: 1px solid var(--border); }
#skills-rows { overflow-y: auto; padding: 4px 0; }
.skill-row { display: flex; gap: 8px; padding: 7px 12px; cursor: pointer;
             align-items: baseline; }
.skill-row:hover { background: var(--bg-hover); }
.skill-row .skill-row-marker { width: 1.2em; color: var(--muted); }
.skill-row .skill-row-description { color: var(--muted); flex: 1;
                                    overflow: hidden; text-overflow: ellipsis;
                                    white-space: nowrap; }
.skill-row .skill-row-source { font-size: .85em; padding: 1px 8px;
                               border-radius: 999px;
                               border: 1px solid var(--border); }
.skill-row .skill-row-source.skill-source-project { color: var(--muted); }
.skill-row .skill-row-load { margin-left: auto; border: 1px solid var(--border);
                             background: transparent; color: var(--fg);
                             border-radius: 4px; width: 1.6em; height: 1.6em;
                             line-height: 1; cursor: pointer; flex: none; }
.skill-row .skill-row-load:hover { border-color: var(--accent);
                                   color: var(--accent); }
.skill-row.skill-row-enabled .skill-row-name { font-weight: 600; }
.skill-row.skill-row-enabled .skill-row-marker { color: var(--accent); }
@media (max-width: 767px) {
  .skill-row { min-height: 40px; padding: 10px 12px; }
}
"#;

#[cfg(test)]
mod skills_region_tests;
