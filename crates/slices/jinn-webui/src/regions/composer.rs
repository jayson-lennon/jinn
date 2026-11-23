//! The composer region — the send↔cancel controls above the input.
//!
//! Parts declared: guard render + SSE event + page fill. No GET pull
//! route, deliberately: the composer's fragments ride the POST
//! responses (`/web/send`, `/web/cancel` answer with the re-rendered
//! controls) and the SSE `controls` event — a pull would render a
//! form the client can only post through anyway. The phase decides
//! the button; the streaming spinner renders above the composer while
//! busy.
//!
//! Browser half: the controls' Datastar bindings (`controls.html`) — the
//! action-attribute delegation (send/cancel) and the
//! [`crate::contract::CONTROLS_ID`] swap. The textarea carries no
//! keydown binding on purpose: mobile soft keyboards use Enter for
//! newlines, so sending rides the button's click alone.

use jinn_kernel::common::state::StateReadGuard;
use jinn_webui_msg::SseKind;

use crate::region::RegionParts;
use crate::region::RegionPull;
use crate::region::WebRegion;

/// The composer region.
pub struct ComposerRegion;

impl WebRegion for ComposerRegion {
    fn id(&self) -> &'static str {
        "composer"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: true,
            pull_route: false,
            sse_event: true,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[SseKind::Controls]
    }

    fn settle_kinds(&self) -> &'static [SseKind] {
        // The idle controls are the settle's tail — the last push,
        // after the transcript's convergence render.
        &[SseKind::Controls]
    }

    fn pull(&self) -> Option<RegionPull> {
        None
    }

    fn container_id(&self) -> Option<&'static str> {
        Some(crate::contract::CONTROLS_ID)
    }

    fn css(&self) -> &'static str {
        COMPOSER_CSS
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &StateReadGuard<'_>,
    ) -> String {
        crate::templates::render(&crate::templates::ControlsTemplate::with_contract())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, what = "controls", "webui template render failed; \
                     serving a blank fragment");
                String::new()
            })
    }
}

/// The composer's stylesheet: the controls bar, textarea, buttons, and
/// the streaming spinner. The phone media query at the tail is the
/// touch-polish pass: the safe-area bottom inset, the ≥40px hit
/// heights, and the 16px input font (the iOS focus-zoom guard). The
/// buttons and the attach label stretch to the textarea's full height
/// at both breakpoints — the desktop rule states `align-self: stretch`
/// explicitly, because the mobile query's hit-height floor must not
/// become the only height story.
///
/// The chips strip is its own row: `#composer` wraps, and
/// `#image-chips` takes `flex-basis: 100%`, which pushes it onto a
/// full-width line ABOVE the textarea/buttons row — the user's
/// directive ("stack the uploaded images above the text/button row").
/// The strip scrolls vertically inside a bounded max-height, so many
/// attachments never push the composer over the viewport and the
/// textarea/buttons row never shifts or overflows at either breakpoint.
pub const COMPOSER_CSS: &str = r#"
.busy-spinner { width: 18px; height: 18px; margin: 0 auto 8px; border: 2px solid var(--border);
                border-top-color: var(--accent); border-radius: 50%;
                animation: webui-spin .8s linear infinite; }
@keyframes webui-spin { to { transform: rotate(360deg); } }
#composer { border-top: 1px solid var(--border); padding: 12px 18px; display: flex; flex-wrap: wrap; gap: 10px; }
#composer textarea { flex: 1; background: var(--panel); color: var(--fg);
                     border: 1px solid var(--border); border-radius: 6px;
                     padding: 8px 10px; font: inherit; resize: none; height: 64px; }
#composer button { background: var(--accent); color: var(--on-accent); border: 0; border-radius: 6px;
                   padding: 0 22px; font: inherit; cursor: pointer; align-self: stretch; }
#composer button.cancel { background: var(--err); color: var(--on-err); }
#image-chips { flex-basis: 100%; display: flex; flex-wrap: wrap; gap: 6px;
               overflow-y: auto; max-height: 132px; min-height: 0; }
.chip { flex: 0 0 auto; display: inline-flex; align-items: center; gap: 6px; background: var(--panel);
        border: 1px solid var(--border); border-radius: 6px; padding: 4px 6px; }
.chip img { width: 28px; height: 28px; object-fit: cover; border-radius: 4px; display: block; }
.chip-name { font-size: 12px; color: var(--muted); max-width: 140px; overflow: hidden;
             text-overflow: ellipsis; white-space: nowrap; }
.chip-x { background: transparent; color: var(--muted); border: 0; font: inherit;
          cursor: pointer; padding: 0 2px; line-height: 1; flex: 0 0 auto; }
.chip-x:hover { color: var(--err); }
#composer .attach { align-self: stretch; background: var(--panel); color: var(--fg);
                    border: 1px solid var(--border); border-radius: 6px;
                    font: inherit; font-size: 18px; line-height: 1; padding: 8px 12px;
                    cursor: pointer; user-select: none; display: inline-flex;
                    align-items: center; }
#composer .attach:hover { border-color: var(--accent); }
#composer .attach input[type="file"] { display: none; }
@media (max-width: 767px) {
  #composer { padding: 10px 12px; padding-bottom: calc(10px + env(safe-area-inset-bottom)); }
  #composer textarea { font-size: 16px; min-height: 40px; height: 64px; }
  #composer button { min-height: 40px; align-self: stretch; margin-bottom: 0; }
}
"#;

/// The session's busy flag from a state guard — the composer's
/// signal payload. The Controls push carries it (the fragment itself
/// is invariant, bound to the `busy` signal client-side).
pub(crate) fn busy_of(session_id: &jinn_core_types::SessionId, guard: &StateReadGuard<'_>) -> bool {
    guard
        .session
        .get(session_id)
        .is_some_and(|session| session.phase().is_working())
}
