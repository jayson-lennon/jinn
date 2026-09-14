//! The Discord status actor — the connection authority, on trouper.
//!
//! A [`ServiceActor`] draining the kanal channel fed by the Discord
//! gateway task. Each [`DiscordStatusUpdate`] it reads is:
//!
//! 1. folded into the authoritative connection cell
//!    ([`discord_connection_slot`]),
//! 2. published on the trouper `jinn.discord` topic (the slice's
//!    EXPORT face — no forward bridge route exists for this type),
//! 3. translated into the dashboard's generic
//!    [`ServiceStatusUpdate`] vocabulary and published on the kameo
//!    bus, whose forward relay still feeds the fabric events topic.
//!
//! The gateway task is a plain tokio task, so the channel stays kanal;
//! the drain loop is spawned from the actor's construction and the
//! actor path doubles as the readiness point.

use jinn_discord_msg::DiscordStatusUpdate;
use jinn_discord_msg::discord_topic;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::TypedCell;
use trouper::actor::ServiceActor;
use trouper::envelope::Event;
use trouper::system::ActorSystem;

/// Discord's own connection fact, folded by [`DiscordStatusActor`].
///
/// The single source of truth for "is the bot connected": feature gates
/// (e.g. thread creation) read this cell instead of greping the
/// dashboard's actor table. One writer — the status actor's fold.
#[derive(Debug, Clone)]
pub struct ConnectionState {
    /// Whether the gateway considers the bot online.
    pub connected: bool,
    /// Optional detail (e.g. the error message while disconnected).
    pub detail: Option<String>,
}

/// Discord's connection cell slot in the slices registry.
///
/// Canonical key shared by wiring (which mints the cell), the status
/// actor (which folds it), and feature gates (which read it).
#[must_use]
pub fn discord_connection_slot() -> jinn_slices::SlotKey {
    jinn_slices::SlotKey::builtin("discord", "connection")
}

/// The dashboard-facing projection of a status update.
#[must_use]
pub fn to_service_update(update: &DiscordStatusUpdate) -> ServiceStatusUpdate {
    let (lifecycle, with_description) = match update {
        DiscordStatusUpdate::Connecting => (Some(jinn_core_types::ActorLifecycle::Starting), true),
        DiscordStatusUpdate::Connected => (Some(jinn_core_types::ActorLifecycle::Running), true),
        DiscordStatusUpdate::Error { .. } => (Some(jinn_core_types::ActorLifecycle::Dead), true),
        DiscordStatusUpdate::Disconnected => (None, false),
    };
    ServiceStatusUpdate {
        name: update.entry_name().to_owned(),
        description: with_description.then(|| update.entry_description().to_owned()),
        lifecycle,
        status_message: Some(update.full_message()),
    }
}

/// Applies an update to the connection cell state.
pub fn fold_connection(state: &mut ConnectionState, update: &DiscordStatusUpdate) {
    match update {
        DiscordStatusUpdate::Connecting => {
            state.connected = false;
            state.detail = Some("Connecting".to_owned());
        }
        DiscordStatusUpdate::Connected => {
            state.connected = true;
            state.detail = None;
        }
        DiscordStatusUpdate::Disconnected => {
            state.connected = false;
            state.detail = Some("Disconnected".to_owned());
        }
        DiscordStatusUpdate::Error { message } => {
            state.connected = false;
            state.detail = Some(message.clone());
        }
    }
}

/// The Discord status actor — the connection authority.
///
/// Spawns its drain loop from construction: read each gateway update,
/// fold it into the cell, publish the native event on the trouper
/// topic, and republish the generic translation on the kameo bus.
pub struct DiscordStatusActor {
    /// Handle for the spawned drain loop (abort on drop semantics are
    /// not needed — the loop lives as long as the channels).
    _keep: (),
}

/// Dependencies for [`DiscordStatusActor`].
#[derive(Clone)]
pub struct DiscordStatusActorDeps {
    /// Receiver half of the kanal channel fed by the Discord gateway.
    pub status_rx: kanal::AsyncReceiver<DiscordStatusUpdate>,
    /// The write handle for discord's connection cell — the drain loop
    /// is its single writer.
    pub cell: TypedCell<ConnectionState>,
    /// The kameo bus, for the dashboard's generic vocabulary.
    pub bus: jinn_domain::common::services::bus_service::BusService,
    /// The trouper system, for the native topic publish + schema
    /// registration.
    pub system: ActorSystem,
}

impl DiscordStatusActor {
    /// Spawns the actor at `discord-status` and starts its drain loop.
    ///
    /// The path registration is the readiness point: once it returns,
    /// the loop is folding updates.
    pub fn spawn(deps: DiscordStatusActorDeps) -> trouper::actor::ActorPath {
        let path = trouper::actor::ActorPath::new("discord-status");
        let DiscordStatusActorDeps {
            status_rx,
            cell,
            bus,
            system,
        } = deps;
        system.register_schema::<DiscordStatusUpdate>();
        tokio::spawn(drain_status_channel(status_rx, cell, bus, system.clone()));
        path
    }
}

impl ServiceActor for DiscordStatusActor {
    async fn start(
        _args: &serde_json::Value,
    ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
        // Never called: `spawn` constructs the actor directly (its
        // state is the drain task's captured handles, not message
        // state).
        Ok(Self { _keep: () })
    }
}

/// Background drain loop: reads discord status updates from the kanal
/// channel, folds the connection fact into the cell, publishes the
/// native event on the trouper topic, and republishes the generic
/// translation on the kameo bus.
async fn drain_status_channel(
    rx: kanal::AsyncReceiver<DiscordStatusUpdate>,
    cell: TypedCell<ConnectionState>,
    bus: jinn_domain::common::services::bus_service::BusService,
    system: ActorSystem,
) {
    while let Ok(update) = rx.recv().await {
        cell.update(|state| fold_connection(state, &update));
        // Native event on the fabric: the dashboard subscribes the
        // topic directly (this is why no forward route exists for the
        // type).
        let payload = serde_json::to_value(&update).unwrap_or(serde_json::Value::Null);
        let event = Event::new(
            <DiscordStatusUpdate as trouper::schema::Schema>::schema_id(),
            payload,
        );
        if let Err(_unroutable) = system
            .send(system.envelope_to_topic(event, discord_topic()))
            .await
        {
            tracing::warn!("discord status topic send was unroutable");
        }
        // The dashboard consumes only the generic projection; discord's
        // row identity travels inside it, so the dashboard stays
        // feature-agnostic. The forward relay (drained at composition)
        // carries it to the fabric events topic.
        let () = bus.publish(to_service_update(&update)).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::super::discord_connection_slot;
    use super::ConnectionState;
    use super::DiscordStatusActor;
    use super::DiscordStatusActorDeps;
    use super::fold_connection;
    use super::to_service_update;
    use jinn_core_types::ActorLifecycle;
    use jinn_discord_msg::DiscordStatusUpdate;
    use jinn_slices::Slices;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    #[rstest::rstest]
    #[test]
    fn fold_connected_sets_cell_connected() {
        // Given a default connection state.
        let mut state = ConnectionState {
            connected: false,
            detail: None,
        };

        // When folding a Connected update.
        fold_connection(&mut state, &DiscordStatusUpdate::Connected);

        // Then the cell reports connected with no detail.
        assert!(state.connected);
        assert_eq!(state.detail, None);
    }

    #[rstest::rstest]
    #[test]
    fn fold_error_keeps_disconnected_with_reason() {
        // Given a default connection state.
        let mut state = ConnectionState {
            connected: false,
            detail: None,
        };

        // When folding a fatal Error update.
        fold_connection(
            &mut state,
            &DiscordStatusUpdate::Error {
                message: "401: invalid bot token".to_owned(),
            },
        );

        // Then the cell stays disconnected and carries the reason.
        assert!(!state.connected);
        assert_eq!(state.detail.as_deref(), Some("401: invalid bot token"));
    }

    #[rstest::rstest]
    #[test]
    fn connected_maps_to_running_for_the_dashboard() {
        // Given a Connected update.
        let update = DiscordStatusUpdate::Connected;

        // When projecting into the dashboard vocabulary.
        let projection = to_service_update(&update);

        // Then the row is Running with the Connected message.
        assert_eq!(projection.lifecycle, Some(ActorLifecycle::Running));
        assert_eq!(projection.status_message.as_deref(), Some("Connected"));
        assert_eq!(projection.name, "discord");
    }

    /// A trouper probe recording the status updates its topic
    /// subscription delivers.
    struct TopicProbe {
        seen: Arc<parking_lot::Mutex<Vec<DiscordStatusUpdate>>>,
    }

    impl trouper::actor::ServiceActor for TopicProbe {
        async fn start(
            _args: &serde_json::Value,
        ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
            Err(
                error_stack::IntoReport::into_report(trouper::registry::RegistryError::InvalidSpec)
                    .attach("TopicProbe spawns via start_with"),
            )
        }
    }

    impl trouper::actor::MsgHandler<DiscordStatusUpdate> for TopicProbe {
        async fn handle(
            &mut self,
            msg: DiscordStatusUpdate,
            _ctx: &mut trouper::context::MsgCtx<'_>,
        ) {
            self.seen.lock().push(msg);
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn drained_update_folds_cell_and_publishes_topic_event() {
        // Given a slices registry with the connection cell, a spawned
        // status actor, and a topic probe subscribed to jinn.discord.
        let slices = Slices::new();
        let connection = slices
            .register(
                discord_connection_slot(),
                ConnectionState {
                    connected: false,
                    detail: None,
                },
            )
            .expect("fresh registry");
        let harness = jinn_domain::common::bus::test_harness::TestHarness::new().await;
        let services = harness.services().await;
        let (tx, rx) = kanal::bounded::<DiscordStatusUpdate>(8);
        let fabric = jinn_testutil::TestFabric::new();
        let seen: Arc<parking_lot::Mutex<Vec<DiscordStatusUpdate>>> = Arc::default();
        let probe_path = trouper::builder::spawn_service_builder::<TopicProbe>(fabric.system())
            .at(trouper::actor::ActorPath::new("discord-status-probe"))
            .start_with({
                let seen = seen.clone();
                move || {
                    Box::pin(async move { Ok(TopicProbe { seen }) })
                        as Pin<Box<dyn Future<Output = _> + Send>>
                }
            })
            .handles::<DiscordStatusUpdate>()
            .start();
        fabric
            .system()
            .subscribe(&probe_path, &jinn_discord_msg::discord_topic(), None)
            .expect("probe subscribes");
        let deps = DiscordStatusActorDeps {
            status_rx: rx.to_async(),
            cell: connection,
            bus: services.bus.clone(),
            system: fabric.system().clone(),
        };
        DiscordStatusActor::spawn(deps);

        // When the gateway reports Connected down the kanal channel.
        let _ = tx.send(DiscordStatusUpdate::Connected);

        // Then the cell folds to connected and the topic carried the
        // native event to the probe.
        for _ in 0..200 {
            if !seen.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let s = slices
            .reader::<ConnectionState>(&discord_connection_slot())
            .expect("cell registered");
        assert!(s.read().connected);
        let events = seen.lock().clone();
        assert!(
            matches!(events.last(), Some(DiscordStatusUpdate::Connected)),
            "jinn.discord topic must carry the native event; got {events:?}"
        );
    }
}
