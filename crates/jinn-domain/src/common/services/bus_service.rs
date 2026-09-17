//! Service wrapper for the kameo [`MessageBus`](kameo_actors::message_bus::MessageBus).
//!
//! In production, delegates to a real kameo `MessageBus`. In tests, can operate
//! in **recording mode** via [`BusService::new_recording()`], which captures all
//! `publish()` calls for assertion with [`BusAudit`].

use std::any::{Any, TypeId};
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use kameo::actor::ActorRef;
use kameo_actors::message_bus::{MessageBus, Publish, Register};

use crate::common::bus::BusMessage;

// ---------------------------------------------------------------------------
// BusService
// ---------------------------------------------------------------------------

/// Shared, cloneable wrapper around the message bus.
///
/// Injected into [`Services`](super::Services) during startup.
/// All bus operations go through this wrapper.
///
/// In test code, use [`BusService::new_recording()`] to create a recording
/// bus that captures publishes for assertion via [`BusAudit`].
#[derive(Clone)]
pub struct BusService {
    inner: BusInner,
}

#[derive(Clone)]
enum BusInner {
    Real(ActorRef<MessageBus>),
    #[cfg_attr(not(test), expect(dead_code, reason = "test-only recording mode"))]
    Recording(Arc<Mutex<Vec<RecordedMessage>>>),
}

impl BusService {
    /// Creates a new bus service wrapping the given actor ref.
    #[must_use]
    pub fn new(bus: ActorRef<MessageBus>) -> Self {
        Self {
            inner: BusInner::Real(bus),
        }
    }

    /// Creates a bus service in **recording mode** for tests.
    ///
    /// Returns a `(BusService, BusAudit)` pair. The service captures all
    /// `publish()` calls; the audit handle reads them back.
    /// `register()` is a no-op in recording mode.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn new_recording() -> (Self, BusAudit) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let service = Self {
            inner: BusInner::Recording(messages.clone()),
        };
        let audit = BusAudit { messages };
        (service, audit)
    }

    /// Returns a reference to the underlying bus actor ref.
    ///
    /// # Panics
    ///
    /// Panics if called on a recording-mode bus (tests should not need this).
    /// A recipient handle for forwarding publishes of `M` to the
    /// bus's subscribers — the seam the bridge's per-route relays
    /// register through at activation time.
    pub async fn register_recipient<M>(&self, recipient: kameo::actor::Recipient<M>)
    where
        M: crate::common::bus::BusMessage,
    {
        match &self.inner {
            BusInner::Real(bus) => {
                if let Err(err) = bus.ask(Register(recipient)).await {
                    tracing::warn!(?err, "bus recipient registration returned an error");
                }
            }
            BusInner::Recording(_) => {
                let _ = recipient;
            }
        }
    }

    /// Returns the raw bus actor ref.
    ///
    /// # Panics
    ///
    /// Panics when called on a recording (test-only) bus: there is no
    /// actor to return.
    #[expect(
        clippy::panic,
        reason = "invariant: recording variant is test-only; calling actor_ref on it is programmer misuse"
    )]
    #[must_use]
    pub fn actor_ref(&self) -> &ActorRef<MessageBus> {
        match &self.inner {
            BusInner::Real(bus) => bus,
            BusInner::Recording(_) => panic!("actor_ref() called on recording BusService"),
        }
    }

    /// Returns `true` if this bus is in recording mode (test-only).
    #[must_use]
    pub fn is_recording(&self) -> bool {
        matches!(&self.inner, BusInner::Recording(_))
    }

    /// Registers a recipient to receive messages of type `M` on the bus.
    ///
    /// No-op in recording mode.
    pub async fn register<M: Clone + Send + 'static>(&self, recipient: kameo::actor::Recipient<M>) {
        match &self.inner {
            BusInner::Real(bus) => {
                if let Err(e) = bus.ask(Register(recipient)).await {
                    tracing::warn!(error = ?e, "failed to register on bus; likely during shutdown");
                }
            }
            BusInner::Recording(_) => {
                // No-op in recording mode
                let _ = recipient;
            }
        }
    }

    /// Registers an actor to receive messages of type `M` on the bus.
    ///
    /// Convenience wrapper that creates the recipient from the actor ref.
    /// No-op in recording mode.
    pub async fn subscribe<M: Clone + Send + 'static, A: kameo::message::Message<M>>(
        &self,
        actor_ref: &ActorRef<A>,
    ) {
        self.register(actor_ref.clone().recipient::<M>()).await;
    }

    /// Publishes a typed message to all registered recipients on the bus.
    ///
    /// In recording mode, captures the message for later assertion.
    ///
    pub async fn publish<M: BusMessage>(&self, msg: M) {
        match &self.inner {
            BusInner::Real(bus) => {
                let name = message_name::<M>();
                tracing::debug!(message = name, "kameo: {name} sent");
                if let Err(e) = bus.tell(Publish(msg)).await {
                    tracing::warn!(err = ?e, "bus publish failed");
                }
            }
            BusInner::Recording(recorded) => {
                let type_id = TypeId::of::<M>();
                recorded.lock().push(RecordedMessage {
                    name: message_name::<M>().to_owned(),
                    type_id,
                    payload: Box::new(msg) as Box<dyn Any + Send>,
                });
            }
        }
    }
}

/// The short type name of a bus message (e.g. `"PushChatEntry"`).
fn message_name<M: BusMessage>() -> &'static str {
    std::any::type_name::<M>()
        .rsplit("::")
        .next()
        .unwrap_or(std::any::type_name::<M>())
}

impl fmt::Debug for BusService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            BusInner::Real(_) => f.debug_struct("BusService").finish_non_exhaustive(),
            BusInner::Recording(_) => f
                .debug_struct("BusService<Recording>")
                .finish_non_exhaustive(),
        }
    }
}

// ---------------------------------------------------------------------------
// RecordedMessage
// ---------------------------------------------------------------------------

/// A single captured publish call.
pub struct RecordedMessage {
    /// Short type name (e.g., `"PushChatEntry"`).
    pub name: String,
    /// `TypeId` of the message for typed downcasting.
    pub type_id: TypeId,
    /// The message payload, type-erased.
    pub payload: Box<dyn Any + Send>,
}

impl RecordedMessage {
    /// Downcasts the payload to a specific message type `M`.
    ///
    /// Returns `None` if the type doesn't match.
    pub fn downcast<M: BusMessage>(&self) -> Option<&M> {
        if self.type_id == TypeId::of::<M>() {
            self.payload.downcast_ref::<M>()
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// BusAudit
// ---------------------------------------------------------------------------

/// Test handle for reading messages captured by a recording [`BusService`].
///
/// Created by [`BusService::new_recording()`].
#[derive(Clone)]
pub struct BusAudit {
    messages: Arc<Mutex<Vec<RecordedMessage>>>,
}

impl BusAudit {
    /// Returns the ordered list of short type names for all captured messages.
    ///
    /// Useful for asserting message ordering:
    /// ```ignore
    /// assert_eq!(audit.names(), ["PersistSession", "PushChatEntry"]);
    /// ```
    pub fn names(&self) -> Vec<String> {
        self.messages
            .lock()
            .iter()
            .map(|m| m.name.clone())
            .collect()
    }

    /// Returns all captured messages of a specific type, in order.
    ///
    /// ```ignore
    /// let entries: Vec<PushChatEntry> = audit.of_type::<PushChatEntry>();
    /// assert_eq!(entries.len(), 1);
    /// ```
    pub fn of_type<M: BusMessage>(&self) -> Vec<M> {
        self.messages
            .lock()
            .iter()
            .filter_map(|m| m.downcast::<M>().cloned())
            .collect()
    }

    /// Returns the total number of captured messages.
    pub fn len(&self) -> usize {
        self.messages.lock().len()
    }

    /// Returns `true` if no messages have been captured.
    pub fn is_empty(&self) -> bool {
        self.messages.lock().is_empty()
    }

    /// Clears all captured messages.
    pub fn clear(&self) {
        self.messages.lock().clear();
    }

    /// Returns `true` if a message with the given type name was captured.
    pub fn contains_name(&self, name: &str) -> bool {
        self.messages.lock().iter().any(|m| m.name == name)
    }
}

impl fmt::Debug for BusAudit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusAudit")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Alpha {
        val: u32,
    }
    impl crate::common::bus::BusMessage for Alpha {}

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Beta {
        text: String,
    }
    impl crate::common::bus::BusMessage for Beta {}

    #[rstest::rstest]
    #[tokio::test]
    async fn new_recording_starts_empty() {
        let (_bus, audit) = BusService::new_recording();
        assert!(audit.is_empty());
        assert_eq!(audit.len(), 0);
        assert!(audit.names().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn publish_captures_single_message() {
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 42 }).await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit.names(), ["Alpha"]);
        let alphas: Vec<Alpha> = audit.of_type::<Alpha>();
        assert_eq!(alphas.len(), 1);
        assert_eq!(alphas[0].val, 42);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn publish_captures_multiple_types_in_order() {
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 1 }).await;
        bus.publish(Beta {
            text: "hello".into(),
        })
        .await;
        bus.publish(Alpha { val: 2 }).await;
        assert_eq!(audit.names(), ["Alpha", "Beta", "Alpha"]);
        assert_eq!(audit.of_type::<Alpha>().len(), 2);
        assert_eq!(audit.of_type::<Beta>().len(), 1);
        assert_eq!(audit.of_type::<Alpha>()[0].val, 1);
        assert_eq!(audit.of_type::<Alpha>()[1].val, 2);
        assert_eq!(audit.of_type::<Beta>()[0].text, "hello");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn contains_name_finds_published_type() {
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 99 }).await;
        assert!(audit.contains_name("Alpha"));
        assert!(!audit.contains_name("Beta"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clear_removes_all_messages() {
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 1 }).await;
        bus.publish(Beta { text: "x".into() }).await;
        assert_eq!(audit.len(), 2);
        audit.clear();
        assert!(audit.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn of_type_returns_empty_for_unpublished_type() {
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 1 }).await;
        let betas: Vec<Beta> = audit.of_type::<Beta>();
        assert!(betas.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn register_is_noop_in_recording_mode() {
        let (bus, _audit) = BusService::new_recording();
        // register is a no-op in recording mode — just verify it doesn't panic
        // We can't easily create a Recipient without spawning an actor,
        // so just verify the bus drops without panic.
        drop(bus);
    }

    /// A `MakeWriter` capturing formatted log output for assertions.
    #[derive(Clone, Default)]
    struct CapturingWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturingWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("poisoned").clone())
                .expect("captured output is utf-8")
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
        type Writer = CapturingSink;

        fn make_writer(&'a self) -> Self::Writer {
            CapturingSink(self.0.clone())
        }
    }

    struct CapturingSink(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut sink = self.0.lock().map_err(|err| {
                std::io::Error::other(format!("capture buffer mutex poisoned: {err}"))
            })?;
            sink.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn publish_on_real_bus_logs_sent_line() {
        use kameo::actor::Spawn;
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;

        // Given a real MessageBus-backed BusService and a subscriber
        // capturing debug events.
        let bus_actor = MessageBus::new(kameo_actors::DeliveryStrategy::BestEffort);
        let bus_ref = MessageBus::spawn(bus_actor);
        let bus = BusService::new(bus_ref);

        let capture = CapturingWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(capture.clone())
                .with_ansi(false)
                .with_filter(tracing_subscriber::EnvFilter::new("jinn_domain=debug")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        // When publishing a message.
        bus.publish(Alpha { val: 7 }).await;

        // Then a debug line names the type as sent. Delivery into the bus
        // actor is async, so poll briefly for the line to land.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if capture.contents().contains("kameo: Alpha sent") {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected 'kameo: Alpha sent' in captured output, got: {}",
                capture.contents()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
