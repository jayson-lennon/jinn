//! The dashboard actor — owns the dashboard slice cell on trouper.
//!
//! Aggregates three data sources into a single dashboard view:
//!
//! - **Generic actor lifecycle** — receives the lifecycle events
//!   [`ActorStarting`], [`ActorStarted`], and [`ActorShutdownCompleted`] to
//!   track every actor's `Starting`/`Running`/`Dead` phase.
//! - **Generic service status** — receives [`ServiceStatusUpdate`] events
//!   published by whichever feature owns a service, applying the optional
//!   lifecycle, description, and status message to the named row.
//! - **Keyboard navigation** — receives [`DashboardNav`], bridged onto the
//!   `jinn.dashboard` topic from the dashboard feature's keybind rows.
//!
//! This actor is a feature-agnostic sink: features translate their own
//! state into the generic events, so no feature-specific type appears
//! here. It owns the dashboard's slice cell exclusively: the cell is
//! minted by [`Slices::register`](jinn_slices::Slices::register)
//! at actor wiring, and this actor holds the one write handle. The
//! renderer and the intent router resolve read handles. Status sources
//! are symmetric producers: they publish events, and this actor is the
//! single sink.
//!
//! The actor runs on the trouper runtime ([`ServiceActor`] tier: a
//! stateless fold into shared state, no journaling). The kameo→canvas
//! bridge ([`crate::common::trouper_bridge`]) translates the bus messages
//! onto its topics; the cell handle cannot ride the runtime's JSON start
//! args, so it is injected through the builder's
//! [`start_with`](trouper::builder::ServiceBuilder::start_with)
//! override.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::fabric_events::{ActorShutdownCompleted, ActorStarted, ActorStarting};
use crate::nav::DashboardNav;
use crate::{ActorLifecycle, DashboardState, ServiceStatusUpdate};
use jinn_slices::TypedCell;

/// The dashboard actor on the canvas runtime.
///
/// Receives lifecycle events, [`ServiceStatusUpdate`], and
/// [`DashboardNav`] on its topics, folding all of them into the slice
/// cell.
pub struct DashboardCanvasActor {
    /// The dashboard's slice cell — minted at wiring, owned here.
    cell: TypedCell<DashboardState>,
}

impl ServiceActor for DashboardCanvasActor {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the cell via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "DashboardCanvasActor is spawned via start_with; start requires the typed cell",
            ),
        )
    }
}

impl DashboardCanvasActor {
    /// Spawns the actor at `dashboard` and subscribes it to both its
    /// topics (`jinn.fabric` + `jinn.dashboard`).
    ///
    /// A successful [`ActorSystem::subscribe`] is the ordering guarantee:
    /// the topic cursors are registered, so every later publish reaches
    /// the actor's inbox. This is what lets the activation sequence be
    /// spawn-then-activate-the-world without missed lifecycle events.
    ///
    /// # Panics
    ///
    /// Panics if the topic subscriptions fail, which can only happen on a
    /// broken actor system; the spawn-then-activate ordering relies on it.
    pub fn spawn(system: &ActorSystem, cell: &TypedCell<DashboardState>) -> ActorPath {
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new("dashboard"))
            // Deep inbox: the startup lifecycle burst (hundreds of
            // events in under a second) must not fill the dashboard's
            // inbox — a full inbox stalls the topic-pump cursor while
            // the retained log evicts, silently dropping events.
            .mailbox(64 * 1024, trouper::inbox::OverloadPolicy::Block)
            .start_with({
                let cell = cell.clone();
                move || Box::pin(async move { Ok(Self { cell }) })
            })
            .handles::<ActorStarting>()
            .handles::<ActorStarted>()
            .handles::<ActorShutdownCompleted>()
            .handles::<ServiceStatusUpdate>()
            .handles::<DashboardNav>()
            .start();
        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug;                       the spawn-then-activate ordering relies on the cursor being registered"
        )]
        system
            .subscribe(&path, &crate::bridge::fabric_topic(), None)
            .expect("dashboard actor subscribes to the fabric topic");
        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug"
        )]
        system
            .subscribe(&path, &crate::bridge::dashboard_topic(), None)
            .expect("dashboard actor subscribes to the dashboard topic");
        path
    }

    /// Folds an [`ActorStarting`] into the cell.
    fn apply_starting(&self, msg: &ActorStarting) {
        self.cell
            .update(|s| s.mark_starting(&msg.name, msg.description.clone()));
    }

    /// Folds an [`ActorStarted`] into the cell.
    fn apply_started(&self, msg: &ActorStarted) {
        self.cell
            .update(|s| s.mark_running(&msg.name, msg.description.clone()));
    }

    /// Folds an [`ActorShutdownCompleted`] into the cell.
    fn apply_shutdown(&self, msg: &ActorShutdownCompleted) {
        self.cell.update(|s| s.mark_dead(&msg.name, None));
    }

    /// Folds a [`ServiceStatusUpdate`] into the cell: the owning
    /// feature's projection onto its row (optional lifecycle, optional
    /// description, optional status message).
    fn apply_service_status(&self, msg: &ServiceStatusUpdate) {
        self.cell.update(|s| apply_service_update(s, msg));
    }

    /// Folds a [`DashboardNav`] into the cell.
    fn apply_nav(&self, msg: DashboardNav) {
        self.cell.update(|s| match msg {
            DashboardNav::Up => s.select_prev(),
            DashboardNav::Down => s.select_next(),
            DashboardNav::First => s.select_first(),
            DashboardNav::Last => s.select_last(),
        });
    }
}

impl MsgHandler<ActorStarting> for DashboardCanvasActor {
    async fn handle(&mut self, msg: ActorStarting, _ctx: &mut MsgCtx<'_>) {
        self.apply_starting(&msg);
    }
}

impl MsgHandler<ActorStarted> for DashboardCanvasActor {
    async fn handle(&mut self, msg: ActorStarted, _ctx: &mut MsgCtx<'_>) {
        self.apply_started(&msg);
    }
}

impl MsgHandler<ActorShutdownCompleted> for DashboardCanvasActor {
    async fn handle(&mut self, msg: ActorShutdownCompleted, _ctx: &mut MsgCtx<'_>) {
        self.apply_shutdown(&msg);
    }
}

impl MsgHandler<ServiceStatusUpdate> for DashboardCanvasActor {
    async fn handle(&mut self, msg: ServiceStatusUpdate, _ctx: &mut MsgCtx<'_>) {
        self.apply_service_status(&msg);
    }
}

impl MsgHandler<DashboardNav> for DashboardCanvasActor {
    async fn handle(&mut self, msg: DashboardNav, _ctx: &mut MsgCtx<'_>) {
        self.apply_nav(msg);
    }
}

/// Apply a generic service status update to the dashboard state.
///
/// The dashboard is a feature-agnostic sink: the owning feature
/// translates its own state and publishes this projection; the fold
/// applies whichever optional fields the event carries (`None`
/// lifecycle leaves the row's phase untouched; `None` description
/// preserves the existing one).
fn apply_service_update(dashboard: &mut DashboardState, update: &ServiceStatusUpdate) {
    if let Some(lifecycle) = update.lifecycle {
        match lifecycle {
            ActorLifecycle::Starting => {
                dashboard.mark_starting(&update.name, update.description.clone());
            }
            ActorLifecycle::Running => {
                dashboard.mark_running(&update.name, update.description.clone());
            }
            ActorLifecycle::Dead => {
                dashboard.mark_dead(&update.name, update.description.clone());
            }
        }
    }
    if update.status_message.is_some() {
        dashboard.set_status_message(&update.name, update.status_message.clone());
    }
}

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
    use crate::contracts::ServiceStatusUpdate;
    use crate::dashboard_slot;
    use crate::fabric_events::{ActorShutdownCompleted, ActorStarted, ActorStarting};
    use crate::nav::DashboardNav;
    use crate::state::DashboardState;
    use jinn_slices::Slices;
    use jinn_slices::TypedCell;
    use jinn_testutil::TestFabric;
    use trouper::schema::Schema;

    /// Polls `check` until it passes or the bounded retry budget runs out.
    async fn wait_for(check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("condition never held within the retry budget");
    }

    fn dashboard_entry(
        cell: &TypedCell<DashboardState>,
        name: &str,
    ) -> Option<(ActorLifecycle, Option<String>, Option<String>)> {
        let s = cell.read();
        s.actors()
            .iter()
            .find(|e| e.name == name)
            .map(|e| (e.lifecycle, e.status_message.clone(), e.description.clone()))
    }

    /// Wires one dashboard cell + canvas actor onto the test fabric.
    fn wire_actor(fabric: &TestFabric) -> TypedCell<DashboardState> {
        let slices = Slices::new();
        let cell = slices
            .register(dashboard_slot(), DashboardState::new())
            .expect("fresh registry");
        DashboardCanvasActor::spawn(fabric.system(), &cell);
        cell
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn actor_starting_event_creates_entry_with_starting_lifecycle() {
        // Given a dashboard canvas actor subscribed on the fabric.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When an ActorStarting envelope lands on the fabric topic.
        fabric
            .send_to_topic(
                &ActorStarting {
                    name: "llm".to_owned(),
                    description: None,
                },
                &crate::bridge::fabric_topic(),
            )
            .await;

        // Then the dashboard shows the actor as Starting.
        wait_for(|| {
            dashboard_entry(&cell, "llm").is_some_and(|(l, _, _)| l == ActorLifecycle::Starting)
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn actor_started_event_promotes_entry_to_running() {
        // Given a wired actor that has seen its subject start.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric
            .send_to_topic(
                &ActorStarting {
                    name: "llm".to_owned(),
                    description: Some("LlmActor".to_owned()),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;
        wait_for(|| dashboard_entry(&cell, "llm").is_some()).await;

        // When the ActorStarted envelope arrives.
        fabric
            .send_to_topic(
                &ActorStarted {
                    name: "llm".to_owned(),
                    description: Some("LlmActor".to_owned()),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;

        // Then the entry promotes to Running with the description.
        wait_for(|| {
            dashboard_entry(&cell, "llm").is_some_and(|(l, _, _)| l == ActorLifecycle::Running)
        })
        .await;
        assert_eq!(
            dashboard_entry(&cell, "llm").unwrap().2.as_deref(),
            Some("LlmActor")
        );
    }

    /// REGRESSION (relay reorder): the `ActorStarting` and `ActorStarted`
    /// forward relays are independent actors, so under the startup burst
    /// the `Started` envelope can cross the fabric before its `Starting`
    /// twin. The fold used to apply events blindly: `Running` then a
    /// stale `Starting` left the row stuck at `Starting` forever — a
    /// different random set of actors on every launch.
    #[rstest::rstest]
    #[tokio::test]
    async fn stale_starting_after_running_leaves_the_row_running() {
        // Given a wired actor that has already seen its subject running.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric
            .send_to_topic(
                &ActorStarted {
                    name: "llm".to_owned(),
                    description: Some("LlmActor".to_owned()),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;
        wait_for(|| {
            dashboard_entry(&cell, "llm").is_some_and(|(l, _, _)| l == ActorLifecycle::Running)
        })
        .await;

        // When the racing ActorStarting envelope lands afterwards.
        fabric
            .send_to_topic(
                &ActorStarting {
                    name: "llm".to_owned(),
                    description: Some("LlmActor".to_owned()),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;
        wait_for(|| dashboard_entry(&cell, "llm").is_some()).await;

        // Then the row stays Running.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            dashboard_entry(&cell, "llm").map(|(l, _, _)| l),
            Some(ActorLifecycle::Running),
            "a stale Starting report must not demote a Running row"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn shutdown_event_marks_entry_dead() {
        // Given a wired actor with a Running entry.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric
            .send_to_topic(
                &ActorStarted {
                    name: "llm".to_owned(),
                    description: None,
                },
                &crate::bridge::fabric_topic(),
            )
            .await;
        wait_for(|| dashboard_entry(&cell, "llm").is_some()).await;

        // When the ActorShutdownCompleted envelope arrives.
        fabric
            .send_to_topic(
                &ActorShutdownCompleted {
                    name: "llm".to_owned(),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;

        // Then the entry is Dead.
        wait_for(|| {
            dashboard_entry(&cell, "llm").is_some_and(|(l, _, _)| l == ActorLifecycle::Dead)
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn status_update_sets_status_message_and_lifecycle() {
        // Given a wired actor with a Running entry for "sample-actor".
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric
            .send_to_topic(
                &ActorStarted {
                    name: "sample-actor".to_owned(),
                    description: None,
                },
                &crate::bridge::fabric_topic(),
            )
            .await;
        wait_for(|| dashboard_entry(&cell, "sample-actor").is_some()).await;

        // When a ServiceStatusUpdate projection arrives with a status message.
        fabric
            .send_to_topic(
                &ServiceStatusUpdate {
                    name: "sample-actor".to_owned(),
                    description: None,
                    lifecycle: None,
                    status_message: Some("3 urls verified".to_owned()),
                },
                &crate::bridge::fabric_topic(),
            )
            .await;

        // Then the Notes column carries the message.
        wait_for(|| {
            dashboard_entry(&cell, "sample-actor")
                .is_some_and(|(_, m, _)| m.as_deref() == Some("3 urls verified"))
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn nav_messages_move_the_selection_cursor() {
        // Given a wired actor with three entries, none selected.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        for name in ["a", "b", "c"] {
            fabric
                .send_to_topic(
                    &ActorStarted {
                        name: name.to_owned(),
                        description: None,
                    },
                    &crate::bridge::fabric_topic(),
                )
                .await;
        }
        wait_for(|| cell.read().actors().len() == 3).await;

        // When DashboardNav::Down envelopes arrive twice.
        fabric
            .send_to_topic(&DashboardNav::Down, &crate::bridge::dashboard_topic())
            .await;
        fabric
            .send_to_topic(&DashboardNav::Down, &crate::bridge::dashboard_topic())
            .await;

        // Then the cursor lands on the third row.
        wait_for(|| cell.read().selected_index() == 2).await;
    }

    /// The lifecycle events the dashboard folds are the **same Rust
    /// types** the kernel publishes (`jinn_slices::fabric` re-exported
    /// here via `fabric_events`) — kameo bus dispatch is by `TypeId`,
    /// so schema-id-equal mirrors would silently drop every event.
    /// This pins the shared identity plus the wire schema id.
    #[rstest::rstest]
    #[test]
    fn lifecycle_events_are_the_shared_fabric_types() {
        // Given one instance of each lifecycle event.
        let starting = ActorStarting {
            name: "llm".to_owned(),
            description: None,
        };

        // When round-tripping through serde.
        let roundtripped: ActorStarting =
            serde_json::from_value(serde_json::to_value(&starting).unwrap()).unwrap();

        // Then the payload survived and the schema id is name@1.
        assert_eq!(roundtripped.name, "llm");
        let id = ActorStarting::schema_id().to_string();
        assert!(id.starts_with("ActorStarting@"), "id was {id}");
        // And the dashboard's import surface IS the shared fabric type.
        let _: jinn_slices::fabric::ActorStarting = starting;
    }
}
