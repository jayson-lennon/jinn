//! The inference slice — the actor that drives LLM provider streams.
//!
//! Hosts the trouper [`ServiceActor`] inference actor (converted from the
//! kernel `LlmActor`). It consumes the slice-owned dispatch commands
//! ([`SendToLlmProvider`], [`CancelTurn`]) and its own [`StreamCompleted`]
//! echo, builds/opens the provider
//! stream via the `LlmService` factory in `Services`, and republishes stream
//! facts (`StreamToken`, `StreamCompleted`, tool-stream events, error/cancel
//! entries) on the fabric — the single write point the session actor's
//! folds already consume.
//!
//! Streaming runs as plain tokio tasks *outside* the actor loop; the actor
//! loop only sees the three crossing messages (plus tombstone bookkeeping).
//!
//! Kernel dependency (see Cargo.toml): the actor publishes on the fabric
//! and resolves LLM factories through `Services`, granted at activation.

mod session;

pub mod inference_actor;
pub mod repetition;
pub mod streaming_indicator;

use jinn_slices::SliceHost;

pub use jinn_inference_msg::CancelTurn;
pub use jinn_inference_msg::SendToLlmProvider;
pub use jinn_inference_msg::StreamCompleted;

/// Activates the slice: spawns the inference actor on trouper (its
/// `.subscribe` declarations are the readiness point). The dispatch
/// commands and the actor's own `StreamCompleted` echo arrive by
/// schema broadcast (the actor publishes completion on the fabric and
/// re-consumes it to finalize per-session tracking). The actor is
/// granted shared `State` alongside `Services`: it reads the session's
/// live working directory from it when minting stream-rule state.
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    state: jinn_kernel::common::state::State,
    services: jinn_kernel::Services,
) {
    // The streaming indicator's own screen region. The element holds
    // throbber animation state, so the draw function keeps exactly one
    // instance behind interior mutability.
    let element = parking_lot::Mutex::new(streaming_indicator::StreamingIndicatorElement::new());
    host.slices()
        .register_render_slot::<jinn_kernel::common::app_state::AppState>(
            jinn_slices::Region::StreamingIndicator,
            std::sync::Arc::new(
                move |frame: &mut ratatui::Frame<'_>,
                      target: jinn_slices::DrawTarget,
                      ctx: &dyn jinn_slices::DrawContext<jinn_kernel::common::app_state::AppState>,
                      _rects: &mut Vec<ratatui::layout::Rect>| {
                    let mut guard = element.lock();
                    streaming_indicator::paint(&mut guard, frame, target.area, ctx);
                },
            ),
        );

    let _path = inference_actor::InferenceActor::spawn(host.system(), state, services);
}

/// Register the streaming indicator element.
///
/// Called by composition in `jinn-tui`: the element is slice-owned, so the
/// kernel's element registry cannot reference it.
pub fn register(registry: &mut jinn_kernel::common::AppUiRegistry) {
    registry.register(Box::new(
        streaming_indicator::StreamingIndicatorElement::new(),
    ));
}
