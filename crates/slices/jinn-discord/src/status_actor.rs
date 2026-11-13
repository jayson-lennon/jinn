//! The Discord status actor — the connection authority, on trouper.
//!
//! A [`ServiceActor`] draining the kanal channel fed by the Discord
//! gateway task. Each [`DiscordStatusUpdate`] it reads is:
//!
//! 1. folded into the authoritative connection cell
//!    ([`discord_connection_slot`]),
//! 2. broadcast on the fabric by schema (the slice's EXPORT face),
//! 3. translated into the dashboard's generic
//!    [`ServiceStatusUpdate`] vocabulary and published on the
//!    bus.
//!
//! The gateway task is a plain tokio task, so the channel stays kanal;
//! the drain loop is spawned from the actor's construction and the
//! actor path doubles as the readiness point.

use jinn_discord_msg::DiscordStatusUpdate;
use jinn_slices::NoteTone;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::TypedCell;
use trouper::actor::ServiceActor;
use trouper::system::ActorSystem;

// The connection cell's payload and slot key live in `jinn-discord-msg`
// so the shared cell catalog can register the slot without depending on
// this crate, which depends on the kernel. Re-exported here so the
// slice's own modules keep reading them from one path.
pub use jinn_discord_msg::{ConnectionState, discord_connection_slot};

/// The dashboard-facing projection of a status update.
///
/// Carries no lifecycle: whether the gateway actor is alive is the
/// runtime's to announce, and an auth failure is not a dead actor. A
/// failure is expressed as an error-toned note on a row that still reads
/// whatever the runtime last reported.
#[must_use]
pub fn to_service_update(update: &DiscordStatusUpdate) -> ServiceStatusUpdate {
    let (note_tone, with_description) = match update {
        DiscordStatusUpdate::Connecting | DiscordStatusUpdate::Connected => (None, true),
        DiscordStatusUpdate::Error { .. } => (Some(NoteTone::Error), true),
        DiscordStatusUpdate::Disconnected => (None, false),
    };
    ServiceStatusUpdate {
        name: update.entry_name().to_owned(),
        description: with_description.then(|| update.entry_description().to_owned()),
        status_message: Some(update.full_message()),
        note_tone,
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
/// topic, and republish the generic translation on the fabric.
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
    /// The message bus, for the dashboard's generic vocabulary.
    pub bus: jinn_kernel::common::services::bus_service::BusService,
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
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::init is async by trait contract; this impl never awaits"
    )]
    async fn init(
        _args: &trouper::json::Json,
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
/// translation on the fabric.
async fn drain_status_channel(
    rx: kanal::AsyncReceiver<DiscordStatusUpdate>,
    cell: TypedCell<ConnectionState>,
    bus: jinn_kernel::common::services::bus_service::BusService,
    system: ActorSystem,
) {
    while let Ok(update) = rx.recv().await {
        cell.update(|state| fold_connection(state, &update));
        // Native event on the fabric: every declarant subscriber
        // receives it.
        system.publish(update.clone()).await;
        // The dashboard consumes only the generic projection; discord's
        // row identity travels inside it, so the dashboard stays
        // feature-agnostic.
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
    use jinn_discord_msg::DiscordStatusUpdate;
    use jinn_slices::NoteTone;
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

    /// Discord does not get to say whether its actor is alive. An auth
    /// failure is a failed handshake, not a dead actor, so the projection
    /// carries no lifecycle and says so through the note instead.
    #[rstest::rstest]
    #[test]
    fn an_error_projects_as_an_error_tone_not_a_dead_actor() {
        // Given a fatal auth Error update.
        let update = DiscordStatusUpdate::Error {
            message: "401: invalid bot token".to_owned(),
        };

        // When projecting into the dashboard vocabulary.
        let projection = to_service_update(&update);

        // Then the note is toned as an error and no lifecycle is claimed.
        assert_eq!(projection.note_tone, Some(NoteTone::Error));
        assert!(
            projection
                .status_message
                .as_deref()
                .is_some_and(|m| m.contains("401: invalid bot token")),
            "the message carries the reason: {:?}",
            projection.status_message
        );
    }

    /// An ordinary connection is not a warning, so it publishes no tone
    /// at all and the row reads with the default.
    #[rstest::rstest]
    #[test]
    fn connected_maps_to_running_for_the_dashboard() {
        // Given a Connected update.
        let update = DiscordStatusUpdate::Connected;

        // When projecting into the dashboard vocabulary.
        let projection = to_service_update(&update);

        // Then the row carries the Connected message with no tone opinion.
        assert_eq!(projection.note_tone, None);
        assert_eq!(projection.status_message.as_deref(), Some("Connected"));
        assert_eq!(projection.name, "discord");
    }

    /// A trouper probe recording the status updates its topic
    /// subscription delivers.
    struct TopicProbe {
        seen: Arc<parking_lot::Mutex<Vec<DiscordStatusUpdate>>>,
    }

    impl trouper::actor::ServiceActor for TopicProbe {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "ServiceActor::init is async by trait contract; this test probe never awaits"
        )]
        async fn init(
            _args: &trouper::json::Json,
        ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
            Err(
                error_stack::IntoReport::into_report(trouper::registry::RegistryError::InvalidSpec)
                    .attach("TopicProbe spawns via init_with"),
            )
        }
    }

    impl trouper::actor::MsgHandler<DiscordStatusUpdate> for TopicProbe {
        async fn handle(
            &mut self,
            msg: &DiscordStatusUpdate,
            _ctx: &mut trouper::context::MsgCtx<'_>,
        ) {
            self.seen.lock().push(msg.clone());
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn drained_update_folds_cell_and_publishes_topic_event() {
        use jinn_kernel::common::bus::HarnessServices;

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
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let services = harness.services().await;
        let (tx, rx) = kanal::bounded::<DiscordStatusUpdate>(8);
        let fabric = jinn_testutil::TestFabric::new();
        let seen: Arc<parking_lot::Mutex<Vec<DiscordStatusUpdate>>> = Arc::default();
        let _probe_path = trouper::builder::spawn_service_builder::<TopicProbe>(fabric.system())
            .at(trouper::actor::ActorPath::new("discord-status-probe"))
            .init_with({
                let seen = seen.clone();
                move || {
                    Box::pin(async move { Ok(TopicProbe { seen }) })
                        as Pin<Box<dyn Future<Output = _> + Send>>
                }
            })
            .handles::<DiscordStatusUpdate>()
            .start();
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
