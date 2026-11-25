//! The wire contract for the second web UI (`/web2`).
//!
//! Everything the browser and the server exchange is one of two tagged
//! enums: [`ClientMsg`] (browser → server, an imperative per variant) and
//! [`ServerMsg`] (server → browser, a noun or past-tense fact per
//! variant). Both ride a single WebSocket as JSON tagged by `type`.
//!
//! The contract crate holds data and pure functions only — no actor, no
//! behavior trait — so the entry and session vocabulary rides in from the
//! core types crate unchanged: a `ChatEntry` on the wire is the same shape
//! the bus already serializes.

pub mod client;
pub mod payload;
pub mod server;

pub use client::ClientMsg;
pub use payload::{
    DeltaField, EntryDeltaBody, EntryEnvelope, EntryUpsertBody, ErrorBody, ModelBody, ModelRowBody,
    ModelRowsBody, PhaseBody, PickerRowBody, PickerRowsBody, SessionRowBody, SessionsBody,
    SnapshotBody, TitleBody,
};
pub use server::ServerMsg;

#[cfg(test)]
mod wire_tests;

/// The protocol version the server stamps into every [`SnapshotBody`].
///
/// A page built against a different major version refuses to apply a
/// snapshot rather than half-render it; the browser checks it on connect.
pub const WEBUI2_PROTOCOL_VERSION: u16 = 1;
