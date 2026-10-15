//! Service wrapper for the message fabric.
//!
//! Production publishes onto the **trouper actor system**: every message
//! is broadcast by its schema id to EVERY actor that declared
//! `.handles::<M>()` at spawn. Publishing is schema-broadcast — no route
//! table, no topic resolution, no way for one slice's registration to
//! divert another consumer's traffic.
//!
//! In tests, [`BusService`] can operate in **recording mode** via
//! [`BusService::new_recording()`], which captures all `publish()` calls
//! for assertion with [`BusAudit`].

use std::any::{Any, TypeId};
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::route::BusMessage;

// ---------------------------------------------------------------------------
// BusService
// ---------------------------------------------------------------------------

/// Shared, cloneable wrapper around the message fabric.
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
    /// Trouper-native fabric: publishes broadcast by schema to every
    /// declarant subscriber.
    Troupe {
        system: trouper::system::ActorSystem,
    },
    Recording(Arc<Mutex<Vec<RecordedMessage>>>),
}

impl BusService {
    /// Creates a bus service backed by the trouper fabric.
    #[must_use]
    pub fn new_trouper(system: trouper::system::ActorSystem) -> Self {
        Self {
            inner: BusInner::Troupe { system },
        }
    }

    /// The trouper system this service publishes onto, when the fabric is
    /// trouper-backed. The seam the bridge drains closures against.
    ///
    /// # Panics
    ///
    /// Panics when called on a recording-mode bus (test-only): there is no
    /// system to return.
    #[expect(
        clippy::panic,
        reason = "invariant: recording variant is test-only; calling system_ref on it is programmer misuse"
    )]
    #[must_use]
    pub fn system_ref(&self) -> &trouper::system::ActorSystem {
        match &self.inner {
            BusInner::Troupe { system } => system,
            BusInner::Recording(_) => {
                panic!("system_ref() called on a recording bus (test-only)")
            }
        }
    }

    /// Creates a bus service in **recording mode** for tests.
    ///
    /// Returns a `(BusService, BusAudit)` pair. The service captures all
    /// `publish()` calls; the audit handle reads them back.
    /// `register()` is a no-op in recording mode.
    pub fn new_recording() -> (Self, BusAudit) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let service = Self {
            inner: BusInner::Recording(messages.clone()),
        };
        let audit = BusAudit { messages };
        (service, audit)
    }

    /// Returns `true` if this bus is in recording mode (test-only).
    #[must_use]
    pub fn is_recording(&self) -> bool {
        matches!(&self.inner, BusInner::Recording(_))
    }

    /// Publishes a typed message onto the fabric.
    ///
    /// The message broadcasts by its schema id to every actor that
    /// declared `.handles::<M>()`; zero receivers is a silent no-op.
    /// In recording mode, captures the message for later assertion.
    pub async fn publish<M>(&self, msg: M)
    where
        M: BusMessage
            + trouper::schema::Schema
            + serde::Serialize
            + Clone
            + Send
            + Sync
            + trouper::envelope::PayloadValue,
    {
        match &self.inner {
            BusInner::Troupe { system } => {
                tracing::debug!(
                    message = message_name::<M>(),
                    "trouper: {} published",
                    message_name::<M>()
                );
                system.publish(msg).await;
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
            BusInner::Troupe { .. } => f.debug_struct("BusService<Troupe>").finish_non_exhaustive(),
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

impl crate::route_publish::PublishSink for BusService {
    fn publish_schema(
        &self,
        schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    ) {
        tracing::debug!(message = name, "bridge publish");
        let system = self.system_ref().clone();
        tokio::spawn(async move {
            system
                .deliver_schema_value(schema_id, trouper::json::Json::from(payload))
                .await;
        });
    }
}

/// Implements the slice-facing publish surface over the bus.
///
/// The closure hands over the JSON payload already serialized; the
/// broadcast fans out by schema id to every declarant subscriber —
/// identical delivery to a typed [`BusService::publish`], only the
/// serialization timing differs.
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[derive(
        Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, trouper::schema::Event,
    )]
    #[schema(description = "Bus test message alpha.")]
    struct Alpha {
        val: u32,
    }
    impl BusMessage for Alpha {}

    #[derive(
        Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, trouper::schema::Event,
    )]
    #[schema(description = "Bus test message beta.")]
    struct Beta {
        text: String,
    }
    impl BusMessage for Beta {}

    #[rstest::rstest]
    #[tokio::test]
    async fn new_recording_starts_empty() {
        // Given a recording bus.
        let (_bus, audit) = BusService::new_recording();

        // When inspecting the audit log before any publish.
        // Then it holds no messages.
        assert!(audit.is_empty());
        assert_eq!(audit.len(), 0);
        // And no message names are recorded.
        assert_eq!(audit.names(), [] as [std::string::String; 0]);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn publish_captures_single_message() {
        // Given a recording bus.
        let (bus, audit) = BusService::new_recording();

        // When publishing one Alpha.
        bus.publish(Alpha { val: 42 }).await;

        // Then the audit holds exactly one message named "Alpha".
        assert_eq!(audit.len(), 1);
        assert_eq!(audit.names(), ["Alpha"]);
        // And the recorded Alpha carries the published value.
        let alphas: Vec<Alpha> = audit.of_type::<Alpha>();
        assert_eq!(alphas.len(), 1);
        assert_eq!(alphas[0].val, 42);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn publish_captures_multiple_types_in_order() {
        // Given a recording bus.
        let (bus, audit) = BusService::new_recording();

        // When publishing Alpha, Beta, then Alpha again.
        bus.publish(Alpha { val: 1 }).await;
        bus.publish(Beta {
            text: "hello".into(),
        })
        .await;
        bus.publish(Alpha { val: 2 }).await;

        // Then the audit records the three names in publish order.
        assert_eq!(audit.names(), ["Alpha", "Beta", "Alpha"]);
        // And each type is recoverable with its published payload.
        assert_eq!(audit.of_type::<Alpha>().len(), 2);
        assert_eq!(audit.of_type::<Beta>().len(), 1);
        assert_eq!(audit.of_type::<Alpha>()[0].val, 1);
        assert_eq!(audit.of_type::<Alpha>()[1].val, 2);
        assert_eq!(audit.of_type::<Beta>()[0].text, "hello");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn contains_name_finds_published_type() {
        // Given a recording bus with one Alpha published.
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 99 }).await;

        // When looking up message names in the audit.
        // Then the published type is found and the unpublished one is not.
        assert!(audit.contains_name("Alpha"));
        assert!(!audit.contains_name("Beta"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clear_removes_all_messages() {
        // Given a recording bus holding two published messages.
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 1 }).await;
        bus.publish(Beta { text: "x".into() }).await;
        assert_eq!(audit.len(), 2);

        // When clearing the audit.
        audit.clear();

        // Then the audit is empty again.
        assert!(audit.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn of_type_returns_empty_for_unpublished_type() {
        // Given a recording bus with only an Alpha published.
        let (bus, audit) = BusService::new_recording();
        bus.publish(Alpha { val: 1 }).await;

        // When filtering the audit for Beta.
        let betas: Vec<Beta> = audit.of_type::<Beta>();

        // Then nothing comes back.
        assert_eq!(betas, [] as [Beta; 0]);
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
    async fn publish_on_trouper_bus_logs_published_line() {
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;

        // Given a trouper-backed BusService and a subscriber capturing
        // debug events.
        let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let bus = BusService::new_trouper(system);

        let capture = CapturingWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(capture.clone())
                .with_ansi(false)
                .with_filter(tracing_subscriber::EnvFilter::new("jinn_slices=debug")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        // When publishing a message.
        bus.publish(Alpha { val: 7 }).await;

        // Then a debug line names the type as published.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if capture.contents().contains("trouper: Alpha published") {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected 'trouper: Alpha published' in captured output, got: {}",
                capture.contents()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
