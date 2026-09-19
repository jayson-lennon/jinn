//! Kanal closure bridge — connects the sync TUI thread to the message fabric.
//!
//! The TUI's intent handler is synchronous and cannot `.await`. This bridge
//! accepts typed message closures through a kanal channel (sync send), then
//! an async drain task calls each closure with a reference to the
//! [`MessageBus`](kameo_actors::message_bus::MessageBus) actor ref — the
//! transitional kameo leg of the fabric. Every closure this module mints
//! publishes the same typed message both fabrics understand.

use kameo::prelude::ActorRef;
use kameo_actors::message_bus::MessageBus;

/// A closure that publishes a typed message to the fabric's kameo leg.
pub type BridgeClosure = Box<dyn FnOnce(&ActorRef<MessageBus>) + Send + 'static>;

/// Bridge between the sync TUI thread and the message fabric.
///
/// The TUI sends [`BridgeClosure`]s via the sync [`kanal::Sender`].
/// A background async task drains them and calls each closure with the bus ref.
#[derive(Debug, Clone)]
pub struct Bridge {
    sender: kanal::Sender<BridgeClosure>,
}

impl Bridge {
    /// Creates a new bridge that drains closures to the given bus.
    ///
    /// Spawns a background tokio task that loops on `receiver.to_async().recv()`
    /// and calls each closure with the bus actor ref.
    pub fn new(bus: ActorRef<MessageBus>) -> Self {
        Self::with_handle(bus, &tokio::runtime::Handle::current())
    }

    /// Creates a new bridge using a specific runtime handle.
    ///
    /// Use this when constructing from outside a tokio async context
    /// (e.g., from sync test code using a shared test runtime).
    pub fn with_handle(bus: ActorRef<MessageBus>, handle: &tokio::runtime::Handle) -> Self {
        let (sender, receiver) = kanal::unbounded::<BridgeClosure>();
        let async_rx = receiver.to_async();

        handle.spawn(async move {
            while let Ok(closure) = async_rx.recv().await {
                closure(&bus);
            }
        });

        Self { sender }
    }

    /// Creates a new bridge over a `BusService`'s kameo leg.
    ///
    /// The fabric's primary leg is trouper; the closure minted by
    /// [`Bridge::publish_closure`](Self::publish_closure) publishes through
    /// the leg the service carries. A service without a kameo leg yields
    /// `None` — call sites are expected to have wired a leg in production
    /// until demolition.
    #[must_use]
    pub fn with_system(
        bus: &crate::common::services::bus_service::BusService,
        handle: &tokio::runtime::Handle,
    ) -> Self {
        Self::with_handle(
            bus.kameo_leg_ref()
                .expect("bridge requires a kameo leg (transitional)")
                .clone(),
            handle,
        )
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
        let bus_actor =
            kameo_actors::message_bus::MessageBus::new(kameo_actors::DeliveryStrategy::BestEffort);
        let bus_ref = kameo::prelude::Spawn::spawn(bus_actor);
        Self::with_handle(
            bus_ref,
            &crate::common::services::test_services::shared_test_handle(),
        )
    }

    /// Sends a closure through the bridge (synchronous, non-blocking).
    ///
    /// # Errors
    ///
    /// Returns the inner channel's [`SendError`] if the drain task has exited
    /// and the channel is closed. The closure will be called by the async drain task
    /// with a reference to the message bus.
    pub fn send(&self, msg: BridgeClosure) -> Result<(), kanal::SendError> {
        self.sender.send(msg)
    }

    /// Wraps a typed message into a bridge closure that publishes it to the bus.
    ///
    /// The returned closure captures the message and spawns a tokio task
    /// to call `bus.tell(Publish(msg)).await`.
    pub fn publish_closure<M>(msg: M) -> BridgeClosure
    where
        M: Clone + Send + 'static,
    {
        Box::new(move |bus| {
            let bus = bus.clone();
            tokio::spawn(async move {
                let _ = bus.tell(kameo_actors::message_bus::Publish(msg)).await;
            });
        })
    }
}

/// A [`MessageSink`] adapter that publishes commands/events via the [`Bridge`].
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
    use kameo::prelude::*;
    use kameo_actors::DeliveryStrategy;
    use kameo_actors::message_bus::{Publish, Register};
    use std::sync::Arc;

    use parking_lot::Mutex;

    #[derive(Actor)]
    struct RecorderActor<T: Send + 'static> {
        received: Arc<Mutex<Vec<T>>>,
    }

    impl<T: Send + 'static> RecorderActor<T> {
        fn new(buffer: Arc<Mutex<Vec<T>>>) -> Self {
            Self { received: buffer }
        }
    }

    impl<T: Clone + Send + 'static> Message<T> for RecorderActor<T> {
        type Reply = ();

        async fn handle(&mut self, msg: T, _ctx: &mut Context<Self, Self::Reply>) {
            self.received.lock().push(msg);
        }
    }

    /// A simple message type for testing.
    #[derive(Clone, Debug, PartialEq)]
    struct TestMsg {
        value: u32,
    }

    impl crate::common::bus::BusMessage for TestMsg {}

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn spawn_bus() -> ActorRef<MessageBus> {
        MessageBus::spawn(MessageBus::new(DeliveryStrategy::BestEffort))
    }

    fn spawn_recorder<T: Clone + Send + 'static>()
    -> (ActorRef<RecorderActor<T>>, Arc<Mutex<Vec<T>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let actor = RecorderActor::spawn(RecorderActor::new(buffer.clone()));
        (actor, buffer)
    }

    #[rstest::rstest]
    #[test]
    fn bus_delivers_published_message_to_registered_recipient() {
        let rt = test_runtime();
        rt.block_on(async {
            // Given a message bus and a registered actor.
            let bus = spawn_bus();
            let (actor, buffer) = spawn_recorder::<TestMsg>();
            bus.tell(Register(actor.recipient::<TestMsg>()))
                .await
                .unwrap();

            // When publishing a message.
            bus.tell(Publish(TestMsg { value: 42 })).await.unwrap();

            // Then the registered actor receives it.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let received = buffer.lock();
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].value, 42);
        });
    }

    #[rstest::rstest]
    #[test]
    fn bridge_closure_publishes_to_bus_and_actor_receives() {
        let rt = test_runtime();
        rt.block_on(async {
            // Given a bus with a registered actor and a bridge.
            let bus = spawn_bus();
            let (actor, buffer) = spawn_recorder::<TestMsg>();
            bus.tell(Register(actor.recipient::<TestMsg>()))
                .await
                .unwrap();

            let bridge = Bridge::new(bus.clone());

            // When sending a closure through the bridge.
            let closure = Bridge::publish_closure(TestMsg { value: 99 });
            bridge.send(closure).unwrap();

            // Then the actor eventually receives the message.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let received = buffer.lock();
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].value, 99);
        });
    }
}
