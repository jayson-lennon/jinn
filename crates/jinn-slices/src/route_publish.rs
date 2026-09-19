//! The publish surface a [`PublishClosure`](crate::PublishClosure)
//! drains against.
//!
//! Implemented by the kernel's bus wrapper (`BusService`); slice crates
//! only ever see this trait, so the fabric's routing table and its
//! recording mode stay kernel-private while closures publish through
//! the exact same path as every other emitter.

/// The erased publish sink closures hand schema-tagged payloads to.
pub trait PublishSink {
    /// Publishes one schema-tagged event onto the fabric.
    ///
    /// `name` is the message's short type name (recording-mode parity);
    /// `payload` is the serde-serialized message body.
    fn publish_schema(
        &self,
        schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    );
}
