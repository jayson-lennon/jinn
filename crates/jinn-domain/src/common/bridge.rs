//! Kanal closure bridge — connects the sync TUI thread to the message fabric.
//!
//! The TUI's intent handler is synchronous and cannot `.await`. This bridge
//! accepts typed message closures through a kanal channel (sync send), then
//! an async drain task calls each closure with the fabric's publish sink —
//! the same `BusService` every actor publishes through. Each closure's
//! publication therefore rides the identical path (schema-id routing,
//! recording mode, delivery semantics) as a direct `publish`.

use jinn_slices::PublishSink;

use crate::common::services::bus_service::BusService;

/// A closure that publishes a typed message to the fabric's publish sink.
pub type BridgeClosure = Box<dyn FnOnce(&dyn PublishSink) + Send + 'static>;

/// Bridge between the sync TUI thread and the message fabric.
///
/// The TUI sends [`BridgeClosure`]s via the sync [`kanal::Sender`].
/// A background async task drains them and calls each closure with the
/// bus service.
#[derive(Debug, Clone)]
pub struct Bridge {
    sender: kanal::Sender<BridgeClosure>,
}

impl Bridge {
    /// Creates a new bridge that drains closures to the given bus service.
    ///
    /// Spawns a background tokio task that loops on `receiver.to_async().recv()`
    /// and calls each closure with the bus service.
    pub fn new(bus: BusService) -> Self {
        Self::with_handle(bus, &tokio::runtime::Handle::current())
    }

    /// Creates a new bridge using a specific runtime handle.
    ///
    /// Use this when constructing from outside a tokio async context
    /// (e.g., from sync test code using a shared test runtime).
    pub fn with_handle(bus: BusService, handle: &tokio::runtime::Handle) -> Self {
        let (sender, receiver) = kanal::unbounded::<BridgeClosure>();
        let async_rx = receiver.to_async();

        handle.spawn(async move {
            while let Ok(closure) = async_rx.recv().await {
                closure(&bus);
            }
        });

        Self { sender }
    }

    /// Creates a new bridge over a `BusService`'s fabric.
    ///
    /// The closure minted by
    /// [`Bridge::publish_closure`](Self::publish_closure) publishes through
    /// the service, so the delivery path (topic routing + the transitional
    /// kameo leg feeding un-ported actors) matches every other emitter.
    #[must_use]
    pub fn with_system(
        bus: &crate::common::services::bus_service::BusService,
        handle: &tokio::runtime::Handle,
    ) -> Self {
        Self::with_handle(bus.clone(), handle)
    }

    /// Creates a dummy bridge that discards all messages.
    ///
    /// Used in tests with a recording BusService where no real bus exists.
    #[must_use]
    pub fn new_dummy(handle: &tokio::runtime::Handle) -> Self {
        let (sender, receiver) = kanal::unbounded::<BridgeClosure>();
        let async_rx = receiver.to_async();
        // Spawn a drain task that silently discards all closures.
        // This keeps the channel open (sends succeed) but does nothing.
        handle.spawn(async move { while async_rx.recv().await.is_ok() {} });
        Self { sender }
    }

    /// Creates a minimal bridge for tests that don't need actual bus delivery.
    #[cfg(any(test, feature = "test-harness"))]
    #[must_use]
    pub fn new_for_test() -> Self {
        let (bus, _audit) = BusService::new_recording();
        Self::with_handle(
            bus,
            &crate::common::services::test_services::shared_test_handle(),
        )
    }

    /// Sends a closure through the bridge (synchronous, non-blocking).
    ///
    /// # Errors
    ///
    /// Returns the inner channel's [`SendError`] if the drain task has exited
    /// and the channel is closed. The closure will be called by the async drain task
    /// with the publish sink.
    pub fn send(&self, msg: BridgeClosure) -> Result<(), kanal::SendError> {
        self.sender.send(msg)
    }

    /// Wraps a typed message into a bridge closure that publishes it to the
    /// bus.
    ///
    /// The returned closure captures the message and publishes it through
    /// the drain's sink — the same `BusService::publish` shape every actor
    /// uses, fire-and-forget from the synchronous caller's perspective.
    pub fn publish_closure<M>(msg: M) -> BridgeClosure
    where
        M: jinn_slices::PublishableMessage,
    {
        Box::new(move |sink| {
            let payload = serde_json::to_value(&msg).unwrap_or(serde_json::Value::Null);
            sink.publish_schema(M::schema_id(), payload, std::any::type_name::<M>());
        })
    }
}

/// Implements the slice-facing publish surface over the kernel bus.
///
/// Publishing serializes nothing twice (the closure hands over the JSON
/// payload) and resolves the routed topic exactly like `BusService::publish`,
/// so every closure-driven publication is indistinguishable from a
/// direct actor publish.
impl PublishSink for BusService {
    fn publish_schema(
        &self,
        schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    ) {
        // The event rides the schema's routed topic. The kameo leg (when
        // present) receives the event too — un-ported bus actors keep
        // consuming while the port is in flight.
        let event = trouper::envelope::Event::new(schema_id, payload);
        tracing::debug!(message = name, "bridge publish");
        let bus = self.clone();
        tokio::spawn(async move {
            bus.publish_event(event).await;
        });
    }
}

/// A [`MessageSink`](crate::common::actor_deps::BusPublish) adapter that
/// publishes commands/events via the [`Bridge`].
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    use crate::common::bus::test_harness::TestHarness;
    use jinn_slices::BusMessage;

    /// A single message type for testing: small, schema'd, serde-roundtrippable.
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct TestMsg {
        value: u32,
    }

    impl BusMessage for TestMsg {}

    jinn_slices::crossing_schema!(TestMsg, "BridgeTestMsg",
        trouper::schema::SchemaKind::Event,
        description: "Bridge delivery test message.",
        fields: ["value" => trouper::schema::FieldTy::Int]);

    #[rstest::rstest]
    #[test]
    fn bridge_closure_publishes_to_bus_and_actor_receives() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async {
            // Given a trouper-backed harness and a recorder for the message.
            let harness = TestHarness::new().await;
            let recorder = harness.spawn_recorder::<TestMsg>().await;
            let bridge = Bridge::new(harness.bus());

            // When sending a closure through the bridge.
            let closure = Bridge::publish_closure(TestMsg { value: 99 });
            bridge.send(closure).expect("send");

            // Then the recorder eventually receives the message.
            let received = crate::common::bus::test_harness::await_recorded::<TestMsg>(
                &recorder,
                1,
                std::time::Duration::from_secs(2),
            )
            .await;
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].value, 99);
        });
    }
}
