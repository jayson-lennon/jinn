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
//! action-attribute delegation (send/cancel), the Enter-to-send
//! binding, and the [`crate::contract::CONTROLS_ID`] swap.

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
        session_id: &jinn_core_types::SessionId,
        guard: &StateReadGuard<'_>,
    ) -> String {
        controls_fragment(session_id, guard)
    }
}

/// The composer's stylesheet: the controls bar, textarea, buttons, and
/// the streaming spinner. The phone media query at the tail is the
/// touch-polish pass: the safe-area bottom inset, the ≥40px hit
/// heights, and the 16px input font (the iOS focus-zoom guard). The
/// desktop rules above it are unchanged.
pub const COMPOSER_CSS: &str = r#"
.busy-spinner { width: 18px; height: 18px; margin: 0 auto 8px; border: 2px solid var(--border);
                border-top-color: var(--accent); border-radius: 50%;
                animation: webui-spin .8s linear infinite; }
@keyframes webui-spin { to { transform: rotate(360deg); } }
#composer { border-top: 1px solid var(--border); padding: 12px 18px; display: flex; gap: 10px; }
#composer textarea { flex: 1; background: var(--panel); color: var(--fg);
                     border: 1px solid var(--border); border-radius: 6px;
                     padding: 8px 10px; font: inherit; resize: none; height: 64px; }
#composer button { background: var(--accent); color: var(--on-accent); border: 0; border-radius: 6px;
                   padding: 0 22px; font: inherit; cursor: pointer; }
#composer button.cancel { background: var(--err); color: var(--on-err); }
@media (max-width: 767px) {
  #composer { padding: 10px 12px; padding-bottom: calc(10px + env(safe-area-inset-bottom)); }
  #composer textarea { font-size: 16px; min-height: 40px; height: 64px; }
  #composer button { min-height: 40px; align-self: flex-end; margin-bottom: 0; }
}
"#;

/// The composer-controls fragment from a state guard — the shape the
/// fan-out calls with its own guard. The phase decides the button.
pub fn controls_fragment(
    session_id: &jinn_core_types::SessionId,
    guard: &StateReadGuard<'_>,
) -> String {
    let busy = guard
        .session
        .get(session_id)
        .is_some_and(|session| session.phase().is_working());
    crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
        session_id.to_string(),
        busy,
    ))
    .unwrap_or_else(|error| {
        tracing::warn!(%error, what = "controls", "webui template render failed; \
             serving a blank fragment");
        String::new()
    })
}
