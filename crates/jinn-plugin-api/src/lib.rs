//! Public wire contract for jinn plugins — v1.
//!
//! This crate is the single source of truth for every byte that crosses the
//! plugin boundary. Plugins (out-of-process WASM guests run by jinn itself)
//! exchange newline-delimited JSON with the host over stdin/stdout; every line
//! is an [`Envelope`] wrapping one tagged message.
//!
//! The contract is deliberately separate from jinn's internal kameo messages:
//! internal messages are private and refactor freely, while these types are
//! public and frozen-once-shipped. Evolution is additive only — new optional
//! fields, new message variants — never renames or removals within a major
//! version.
//!
//! The JSON Schema (`plugin-api.schema.json`, colocated here) mirrors these
//! types by hand. A drift test fails when the two disagree, so third parties
//! can codegen bindings against the schema with confidence.
//!
//! # Versioning
//!
//! The envelope carries `v` (the wire major version, currently `1`). Within a
//! major version, unknown message tags deserialize to `Unknown` and are
//! ignored — an older host tolerates a newer plugin and vice versa.

mod envelope;
mod wire;

pub use envelope::{Envelope, PROTOCOL_VERSION, PluginToHostOrHostToPlugin};
pub use wire::{
    CancelStream, Hello, HostToPlugin, InsertSystemEntry, PluginCitation, PluginToHost,
    PushCitations, RestartStalledStream, SUBSCRIPTION_KINDS, StreamEndEvent, StreamEndReason,
    StreamEventPing, StreamStartEvent, TickEvent, ToolCallEvent, ToolResultEvent, TurnEndEvent,
    Welcome,
};
