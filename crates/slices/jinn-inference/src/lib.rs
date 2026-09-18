//! The inference slice — the actor that drives LLM provider streams.
//!
//! Hosts the trouper [`ServiceActor`] inference actor (converted from the
//! kernel kameo `LlmActor`). It consumes the slice-owned dispatch commands
//! ([`SendToLlmProvider`], [`CancelStream`]) and its own [`StreamCompleted`]
//! echo over the [`inference_topic`] trouper topic, builds/opens the provider
//! stream via the `LlmService` factory in `Services`, and republishes stream
//! facts (`StreamToken`, `StreamCompleted`, tool-stream events, error/cancel
//! entries) on the kameo bus — the single write point the session actor's
//! folds already consume.
//!
//! Streaming runs as plain tokio tasks *outside* the actor loop; the actor
//! loop only sees the three crossing messages (plus tombstone bookkeeping).
//!
//! Kernel dependency (see Cargo.toml): the actor publishes on the kameo bus
//! and resolves LLM factories through `Services`, granted at activation.

mod session;

pub mod bridge;
pub mod inference_actor;

use jinn_slices::SliceHost;

pub use jinn_inference_msg::CancelStream;
pub use jinn_inference_msg::SendToLlmProvider;
pub use jinn_inference_msg::StreamCompleted;
use trouper::schema::Schema;

pub use jinn_inference_msg::inference_topic;

/// Activates the slice: spawns the inference actor on trouper and subscribes
/// it to the [`inference_topic`] (the readiness point), then stages the
/// slice's three forward routes — the dispatch commands and the
/// actor's own `StreamCompleted` echo (the actor publishes completion on the
/// kameo bus and re-consumes it to finalize per-session tracking).
///
/// Composition drains the staged routes after activation (see
/// [`bridge::drain_routes`]).
///
/// # Panics
///
/// Panics if the just-spawned inference actor cannot be subscribed to the
/// inference topic — a broken subscription must abort launch rather than
/// leave dispatch commands silently unconsumed.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    services: jinn_domain::Services,
) {
    let path = inference_actor::InferenceActor::spawn(host.system(), services);
    host.subscribe_service(&path, &inference_topic())
        .expect("inference actor subscribes to the inference topic");

    host.forward::<SendToLlmProvider, _>(inference_topic(), || {
        <SendToLlmProvider as Schema>::schema_def()
    });
    host.forward::<CancelStream, _>(inference_topic(), || <CancelStream as Schema>::schema_def());
    host.forward::<StreamCompleted, _>(inference_topic(), || {
        <StreamCompleted as Schema>::schema_def()
    });
}
