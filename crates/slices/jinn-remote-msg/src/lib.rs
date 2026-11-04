//! The `jinn remote` contract — the vocabulary crossing the remote slice's
//! boundary.
//!
//! Two families live here, and both are pure data plus pure functions:
//!
//! - [`wire`] — the HTTP payloads the control surface exchanges: the
//!   health probe, the session listing, the send request and its
//!   acknowledgement, and the error body. `curl` and the `jinn remote`
//!   CLI are both clients of exactly these shapes.
//! - [`registry`] — the instance-registry entry and its file functions:
//!   how a running jinn advertises itself and how a client finds one.
//!
//! Delivery publishes *existing* chat-input schemas
//! (`EnqueueUserMessage`, `SubmitSteeringMessage`), so no bus schema
//! lives here — this crate never depends on an implementation crate.

pub mod registry;
pub mod wire;

pub use registry::InstanceEntry;
pub use wire::{
    ErrorBody, HealthResponse, SendAck, SendMode, SendRequest, SendStatus, SessionInfo,
    SessionsResponse,
};
