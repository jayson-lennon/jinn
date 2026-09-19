//! Fabric roundtrip tests — the delivery contract of the `BusService` swap.
//!
//! Every routed message must survive `publish` → trouper topic → schema
//! adapter decode → subscriber. These tests validate the fabric itself, not
//! any actor's behavior: a message goes in typed, comes out typed, intact.

use std::time::Duration;

use jinn_core_types::{ChatEntry, SessionId, ToolResult};
use crate::common::bus::test_harness::{TestHarness, await_recorded};
use crate::common::services::bus_service::{BusService, JINN_DOMAIN_TOPIC, RouteTestProbe};
use jinn_session_history_msg::PushChatEntry;
use crate::feat::chat_input::protocol::event::ChatEntrySubmitted;
use crate::feat::provider::protocol::event::ProviderSwitched;
use crate::feat::session::protocol::UserInteracted;
use jinn_tools_msg::ToolExecutionCompleted;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The typed wrapper letting one rstest parametrize over distinct message
/// types: a newtype fixture per case; the fabric only sees the inner value.
trait Sampled {
    /// The wrapped message type.
    type Message;

    /// Consumes the fixture into its message.
    fn into_message(self) -> Self::Message;
}

/// Fixture wrapping a [`ChatEntrySubmitted`].
struct ChatEntrySubmittedSample(ChatEntrySubmitted);

impl Sampled for ChatEntrySubmittedSample {
    type Message = ChatEntrySubmitted;

    fn into_message(self) -> Self::Message {
        self.0
    }
}

/// Fixture wrapping a [`UserInteracted`].
struct UserInteractedSample(UserInteracted);

impl Sampled for UserInteractedSample {
    type Message = UserInteracted;

    fn into_message(self) -> Self::Message {
        self.0
    }
}

/// Fixture wrapping a [`ProviderSwitched`].
struct ProviderSwitchedSample(ProviderSwitched);

impl Sampled for ProviderSwitchedSample {
    type Message = ProviderSwitched;

    fn into_message(self) -> Self::Message {
        self.0
    }
}

/// Fixture wrapping a [`ToolExecutionCompleted`].
struct ToolExecutionCompletedSample(ToolExecutionCompleted);

impl Sampled for ToolExecutionCompletedSample {
    type Message = ToolExecutionCompleted;

    fn into_message(self) -> Self::Message {
        self.0
    }
}

/// A fresh session id so no two tests share identity.
fn fresh_session() -> SessionId {
    SessionId::new()
}

fn sample_entry() -> ChatEntry {
    ChatEntry::user("fabric roundtrip probe")
}

fn sample_tool_result() -> ToolResult {
    ToolResult {
        tool_call_id: "tc_probe".to_owned(),
        name: "probe".to_owned(),
        content: "probe output".to_owned(),
        success: true,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

// ---------------------------------------------------------------------------
// Roundtrip: publish → topic → tap → decoded message
// ---------------------------------------------------------------------------

/// One publish of each routed message arrives at the subscriber decoded and
/// intact — the fabric carries the message without loss or mutation.
#[rstest::rstest]
#[case::chat_entry_submitted(ChatEntrySubmittedSample(ChatEntrySubmitted {
    session_id: SessionId::new(),
    entry: sample_entry(),
}))]
#[case::user_interacted(UserInteractedSample(UserInteracted { session_id: SessionId::new() }))]
#[case::provider_switched(ProviderSwitchedSample(ProviderSwitched {
    session_id: SessionId::new(),
    provider_name: "probe-provider".to_owned(),
}))]
#[case::tool_completed(ToolExecutionCompletedSample(ToolExecutionCompleted {
    session_id: SessionId::new(),
    result: sample_tool_result(),
}))]
#[tokio::test]
async fn publish_routed_message_roundtrips_to_subscriber<S: Sampled>(
    #[case] sample: S,
) where
    S::Message: jinn_slices::BusMessage
        + Clone
        + trouper::schema::Schema
        + serde::Serialize
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + 'static,
{
    // Given a harness with a recorder subscribed on the trouper fabric.
    let harness = TestHarness::new().await;
    let recorder = harness.spawn_recorder::<S::Message>().await;
    let expected = sample.into_message();

    // When publishing the sample message.
    harness.publish(expected.clone()).await;

    // Then the recorder receives exactly that message, decoded. Equality is
    // asserted through the serde representation — the fabric's wire format.
    let as_json = |m: &S::Message| serde_json::to_value(m).expect("message serializes");
    let messages = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
    assert_eq!(messages.len(), 1, "exactly one delivery expected");
    assert_eq!(
        as_json(&messages[0]),
        as_json(&expected),
        "delivered payload must equal the published message"
    );
}

/// A publish through a trouper-only `BusService` (kameo leg removed — the
/// post-demolition shape) still routes onto the fabric, proving the trouper
/// leg is the primary path, not a relay of the kameo bus.
#[tokio::test]
async fn publish_without_kameo_leg_still_routes_on_trouper() {
    // Given a trouper-only fabric and a recorder for the message.
    let system =
        trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let bus = BusService::new_trouper(system.clone(), None);
    let probe = RouteTestProbe::attach(&bus);
    let harness = TestHarness::from_parts(bus.clone(), system);
    let recorder = harness.spawn_recorder::<UserInteracted>().await;
    let session = fresh_session();

    // When publishing directly through the fabric.
    bus.publish(UserInteracted {
        session_id: session.clone(),
    })
    .await;

    // Then the recorder receives the message.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].session_id, session);
    // And the publish rode the shared domain topic.
    assert_eq!(
        probe.topic_for::<UserInteracted>(),
        Some(JINN_DOMAIN_TOPIC.to_owned())
    );
}

/// A route registered for a message's schema moves its publishes off the
/// default topic onto the routed one (slice-topic override) without
/// affecting other messages' routing.
#[tokio::test]
async fn registered_route_moves_publishes_to_override_topic() {
    // Given a trouper-only fabric with a route for UserInteracted onto a
    // slice topic, and a recorder.
    let system =
        trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let bus = BusService::new_trouper(system.clone(), None);
    let probe = RouteTestProbe::attach(&bus);
    bus.route_topic::<UserInteracted>(trouper::topics::Topic::new("session.slice"));
    let harness = TestHarness::from_parts(bus.clone(), system);
    let recorder = harness.spawn_recorder::<UserInteracted>().await;
    let session = fresh_session();

    // When publishing the routed message.
    bus.publish(UserInteracted {
        session_id: session.clone(),
    })
    .await;

    // Then delivery still completes.
    let messages = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
    assert_eq!(messages.len(), 1);
    // And the publish rode the override topic, not the default.
    assert_eq!(
        probe.topic_for::<UserInteracted>(),
        Some("session.slice".to_owned())
    );
    // And unrouted schemas kept the default topic.
    assert_eq!(
        probe.topic_for::<ProviderSwitched>(),
        Some(JINN_DOMAIN_TOPIC.to_owned())
    );
}

/// Recording mode keeps capturing publishes verbatim (test-mode parity with
/// the pre-swap kameo bus).
#[tokio::test]
async fn recording_mode_captures_published_messages() {
    // Given a recording bus and one fixed message.
    let (bus, audit) = BusService::new_recording();
    let session = fresh_session();
    let entry = sample_entry();
    let entry_id = entry.id.clone();
    let msg = PushChatEntry {
        session_id: session.clone(),
        entry,
    };

    // When publishing it.
    bus.publish(msg).await;

    // Then the audit captures exactly that message name.
    let names = audit.names();
    assert_eq!(names, vec!["PushChatEntry".to_owned()]);
    // And the capture typed-downcasts back with the session intact.
    let captured = audit.of_type::<PushChatEntry>().remove(0);
    assert_eq!(captured.session_id, session);
    assert_eq!(captured.entry.id, entry_id);
}
