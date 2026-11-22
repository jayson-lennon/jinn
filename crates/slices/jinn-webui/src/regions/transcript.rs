//! The transcript region — the session's full-fidelity transcript.
//!
//! Parts declared: all five. The correctness dependency of the whole
//! surface lives here — the cursor-addressed pull (`?since=N`) is
//! what a reconnecting page catches up through before subscribing.
//! The guard render is the fan-out's convergence push
//! (`transcript-full`, cursor 0) that heals any drift after rewinds,
//! cancels, or partial-entry edges.
//!
//! Browser half: the `#transcript`-targeted Datastar patches — the
//! append/replace swaps, the cursor re-base, and the optimistic user
//! entry (region-owned: it exists solely for this region).

use jinn_kernel::common::state::StateReadGuard;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The transcript region.
pub struct TranscriptRegion;

impl WebRegion for TranscriptRegion {
    fn id(&self) -> &'static str {
        "transcript"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: true,
            pull_route: true,
            sse_event: true,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::Streaming, SseKind::TranscriptFull]
    }

    fn settle_kinds(&self) -> &'static [SseKind] {
        // The convergence push rides every settle — after the boundary
        // is cleared, before the composer's idle controls. It is the
        // push that empties the streaming container structurally: the
        // full fragment carries the (empty) `#streaming` div as its
        // last child.
        &[SseKind::TranscriptFull]
    }

    fn pull(&self) -> Option<RegionPull> {
        // The cursor pull is the correctness dependency (catch-up
        // before subscribe) and stays its own handler in `server.rs`
        // beside the SSE endpoint it pairs with — it reads the store
        // for archived sessions, which the guard render cannot.
        Some(RegionPull {
            pattern: crate::contract::TRANSCRIPT_ROUTE,
            handler: {
                use axum::routing::get;
                get(crate::server::transcript_pull)
            },
        })
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::TRANSCRIPT_ID)
    }

    fn css(&self) -> &'static str {
        TRANSCRIPT_CSS
    }

    fn render(
        &self,
        session_id: &jinn_core_types::SessionId,
        guard: &StateReadGuard<'_>,
    ) -> String {
        transcript_full_fragment(session_id, guard)
    }
}

/// The transcript's stylesheet: the scroll wrap, entries, markdown
/// blocks, disclosures, the streaming container, citation rows, and
/// the footnote elements markdown rendering emits.
pub const TRANSCRIPT_CSS: &str = r#"
#transcript-wrap { flex: 1; overflow-y: auto; padding: 8px 0; }
.entry { padding: 6px 18px; }
.entry.user { border-left: 3px solid var(--accent); background: var(--bg-user-entry);
              margin: 8px 12px; border-radius: 6px; padding: 10px 14px; }
.entry .who { font-size: 11px; text-transform: uppercase; letter-spacing: .06em;
              color: var(--muted); margin-bottom: 2px; }
.entry.error { color: var(--err); }
.entry.system, .entry.compaction, .entry.annotation,
.entry.rule_interrupt, .entry.intersession, .entry.transient { color: var(--muted); font-size: 13px; }
.entry pre { background: var(--bg-code); padding: 10px; border-radius: 6px;
             overflow-x: auto; white-space: pre-wrap; word-break: break-word; }
.entry code { background: var(--bg-code); padding: 1px 5px; border-radius: 4px; }
.entry table { border-collapse: collapse; }
.entry td, .entry th { border: 1px solid var(--border); padding: 4px 8px; }
details.disclosure { border: 1px solid var(--border); border-radius: 6px;
                     margin: 4px 0; background: var(--panel); }
details.disclosure > summary { cursor: pointer; padding: 6px 12px; color: var(--muted);
                               font-size: 13px; list-style: none; }
details.disclosure > summary::before { content: "▸ "; }
details.disclosure[open] > summary::before { content: "▾ "; }
details.disclosure .body { padding: 4px 12px 10px; white-space: pre-wrap;
                           word-break: break-word; font-size: 13px;
                           font-family: ui-monospace, monospace; }
.badge { font-size: 11px; padding: 1px 7px; border-radius: 9px; margin-left: 8px; }
.badge.success { background: var(--bg-badge-ok); color: var(--ok); }
.badge.failure { background: var(--bg-badge-err); color: var(--err); }
.badge.pending { background: var(--bg-badge-pending); color: var(--muted); }
#streaming .entry.thinking details.disclosure .body { color: var(--muted); font-style: italic; }
.citation { display: flex; gap: 8px; align-items: baseline; padding: 2px 0;
            font-family: inherit; white-space: normal; }
.citation .citation-title { color: var(--accent); text-decoration: none; }
.citation a.citation-title:hover { text-decoration: underline; }
.citation .citation-url { color: var(--muted); font-size: 12px;
                          word-break: break-all; font-family: ui-monospace, monospace; }
sup.footnote-reference { font-size: 11px; }
sup.footnote-reference a { color: var(--accent); text-decoration: none; }
sup.footnote-reference a:hover { text-decoration: underline; }
div.footnote-definition { margin: 6px 0; padding-left: 10px;
                          border-left: 2px solid var(--border); font-size: 13px; }
sup.footnote-definition-label { color: var(--muted); margin-right: 6px; }
.entry img.attachment { display: block; max-width: 320px; max-height: 240px;
                        border-radius: 6px; margin: 8px 0 2px;
                        border: 1px solid var(--border); }
"#;

/// The model selection rendered as the label a reader recognizes.
///
/// Shared by every caller that turns a session's selection into the
/// display string (page render, picker, catch-up, full push).
#[must_use]
pub fn model_label(selection: &jinn_core_types::ModelSelection) -> String {
    match selection {
        jinn_core_types::ModelSelection::Single(name) => name.clone(),
        jinn_core_types::ModelSelection::Alloy { models, .. } => models.join(", "),
    }
}

/// The full-transcript fragment from a state guard — the fan-out's
/// convergence push. Blank for an unknown session (it may still be
/// loading; the next push fills it).
pub fn transcript_full_fragment(
    session_id: &jinn_core_types::SessionId,
    guard: &StateReadGuard<'_>,
) -> String {
    let Some(session) = guard.session.get(session_id) else {
        return String::new();
    };
    let transcript = crate::transcript::TranscriptModel::from_live(
        session.history(),
        session.title(),
        &model_label(session.model_selection()),
        session.phase().as_str(),
        session.is_bot(),
        session_id,
    );
    crate::templates::render(&crate::templates::TranscriptTemplate::new(
        transcript.entries,
        0,
    ))
    .unwrap_or_else(|error| {
        tracing::warn!(%error, what = "transcript-full", "webui template render failed; \
                 serving a blank fragment");
        String::new()
    })
}
