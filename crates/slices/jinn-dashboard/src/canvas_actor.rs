//! The dashboard actor — owns the dashboard slice cell on trouper.
//!
//! Aggregates three data sources into a single dashboard view:
//!
//! - **The runtime's actor census** — receives [`trouper::ActorLifecycle`],
//!   which the runtime broadcasts system-level at every actor spawn and
//!   stop. This is what makes the dashboard a census rather than a
//!   curated list: a row appears for an actor that publishes no
//!   jinn-level event of any kind, and no spawn site in any slice needs
//!   to know the dashboard exists.
//! - **Generic service status** — receives [`ServiceStatusUpdate`]
//!   published by whichever feature owns a service, adding a
//!   human-written description, status message, and note tone to that row.
//! - **Keyboard navigation** — receives [`DashboardNav`], bridged from
//!   the dashboard feature's keybind rows.
//!
//! The two sources are deliberately asymmetric, and the asymmetry is
//! structural rather than promised: the runtime decides *whether a row
//! exists and whether it is alive*, a feature decides *what its row
//! says*. [`ServiceStatusUpdate`] carries no lifecycle field at all, so
//! a feature cannot contradict the runtime's verdict on whether its
//! actor is alive — the dashboard's fold is the sole writer of that
//! state.
//!
//! This actor is a feature-agnostic sink: features translate their own
//! state into the generic event, so no feature-specific type appears
//! here. It owns the dashboard's slice cell exclusively: the cell is
//! minted by [`Slices::register`](jinn_slices::Slices::register)
//! at actor wiring, and this actor holds the one write handle. The
//! renderer and the intent router resolve read handles.
//!
//! The actor runs on the trouper runtime ([`ServiceActor`] tier: a
//! stateless fold into shared state, no journaling). The cell handle
//! cannot ride the runtime's JSON start args, so it is injected
//! through the builder's
//! [`init_with`](trouper::builder::ServiceBuilder::init_with)
//! override.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::nav::DashboardNav;
use crate::{ActorLifecycle, DashboardState, ServiceStatusUpdate};
use jinn_slices::TypedCell;
use trouper::LifecycleState;

/// The dashboard actor on the canvas runtime.
///
/// Receives [`trouper::ActorLifecycle`], [`ServiceStatusUpdate`], and
/// [`DashboardNav`], folding all of them into the slice cell.
pub struct DashboardCanvasActor {
    /// The dashboard's slice cell — minted at wiring, owned here.
    cell: TypedCell<DashboardState>,
}

impl ServiceActor for DashboardCanvasActor {
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the cell via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "DashboardCanvasActor is spawned via init_with; start requires the typed cell",
            ),
        )
    }
}

impl DashboardCanvasActor {
    /// Spawns the actor at `dashboard` and subscribes it to the census
    /// and feature-status schemas.
    ///
    /// A successful spawn is the ordering guarantee: `.handles` registers
    /// the schemas, so every later broadcast reaches this actor's inbox.
    /// This is what lets the activation sequence be spawn-then-activate-
    /// the-world without missing a spawn announcement.
    ///
    /// # Panics
    ///
    /// Panics if the spawn fails, which can only happen on a broken actor
    /// system; the spawn-then-activate ordering relies on it.
    pub fn spawn(system: &ActorSystem, cell: &TypedCell<DashboardState>) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new("dashboard"))
            // Deep inbox: the startup burst (one announcement per actor,
            // plus a per-row status message where a feature sends one)
            // must not fill the dashboard's inbox — a full inbox stalls
            // dispatch while the retained log evicts, silently dropping
            // rows.
            .mailbox(64 * 1024, trouper::inbox::OverloadPolicy::Block)
            .init_with({
                let cell = cell.clone();
                move || Box::pin(async move { Ok(Self { cell }) })
            })
            .handles::<trouper::ActorLifecycle>()
            .handles::<ServiceStatusUpdate>()
            .handles::<DashboardNav>()
            .start()
    }

    /// Folds a runtime lifecycle announcement into the cell.
    ///
    /// The ONLY writer of an entry's lifecycle. A feature cannot reach
    /// this field: `ServiceStatusUpdate` carries no lifecycle, so the
    /// runtime's announcement is the sole source of a row's state.
    fn apply_lifecycle(&self, msg: &trouper::ActorLifecycle) {
        self.cell.update(|s| {
            let name = msg.path.to_string();
            // Exhaustive, deliberately, and with no catch-all arm. Every
            // runtime state names what it does to the row it concerns, so
            // a new state added upstream fails to COMPILE here and forces
            // an explicit decision. A catch-all would instead route it
            // silently somewhere harmless-looking — which is exactly how
            // passivation came to be reported as death here.
            match msg.state {
                LifecycleState::Running => s.mark_running(name, None),
                // Passivation is NOT death. The runtime evicted an idle
                // actor and will re-spawn it on the next send, so the row
                // reads Idle: calling a dormant partition-set entity
                // "Crashed" reports a failure that did not happen.
                LifecycleState::Passivated => s.mark_idle(name),
                // A failure is the one stop worth keeping on screen. The
                // actor will not re-announce itself — there is nothing
                // left to announce — so dropping the row would erase the
                // evidence that it broke.
                LifecycleState::Crashed => s.mark_failed(name, ActorLifecycle::Crashed),
                LifecycleState::Escalated => s.mark_failed(name, ActorLifecycle::Escalated),
                // The two SANCTIONED stops. The actor finished on its own
                // or was torn down by the shutdown sweep; either way it is
                // not a failure, and retaining the row would grow the list
                // without bound and report a clean ending as an incident.
                LifecycleState::Normal | LifecycleState::Shutdown => s.remove(name),
            }
        });
    }

    /// Folds a [`ServiceStatusUpdate`] into the cell: the owning
    /// feature's projection onto its row (optional description, optional
    /// status message, optional note tone).
    fn apply_service_status(&self, msg: &ServiceStatusUpdate) {
        self.cell.update(|s| apply_service_update(s, msg));
    }

    /// Folds a [`DashboardNav`] into the cell.
    fn apply_nav(&self, msg: &DashboardNav) {
        self.cell.update(|s| match msg {
            DashboardNav::Up => s.select_prev(),
            DashboardNav::Down => s.select_next(),
            DashboardNav::First => s.select_first(),
            DashboardNav::Last => s.select_last(),
        });
    }
}

impl MsgHandler<trouper::ActorLifecycle> for DashboardCanvasActor {
    async fn handle(&mut self, msg: &trouper::ActorLifecycle, _ctx: &mut MsgCtx<'_>) {
        self.apply_lifecycle(msg);
    }
}

impl MsgHandler<ServiceStatusUpdate> for DashboardCanvasActor {
    async fn handle(&mut self, msg: &ServiceStatusUpdate, _ctx: &mut MsgCtx<'_>) {
        self.apply_service_status(msg);
    }
}

impl MsgHandler<DashboardNav> for DashboardCanvasActor {
    async fn handle(&mut self, msg: &DashboardNav, _ctx: &mut MsgCtx<'_>) {
        self.apply_nav(msg);
    }
}

/// Apply a generic service status update to the dashboard state.
///
/// The dashboard is a feature-agnostic sink: the owning feature
/// translates its own state and publishes this projection; the fold
/// applies whichever optional fields the event carries (`None`
/// description preserves the existing one, `None` tone leaves the row
/// where the feature last placed it).
///
/// There is deliberately no lifecycle arm. A feature describes its own
/// service; whether the actor behind it is alive is the runtime's to
/// announce, and letting a feature answer that question is how a
/// handshake failure ended up painted as a dead actor.
fn apply_service_update(dashboard: &mut DashboardState, update: &ServiceStatusUpdate) {
    if let Some(description) = &update.description {
        dashboard.set_description(&update.name, Some(description.clone()));
    }
    if let Some(message) = &update.status_message {
        dashboard.set_status_message(&update.name, Some(message.clone()));
    }
    // Every update expresses a tone: a toneless note is an ordinary one
    // (`Muted`), which is also how a feature un-flags an earlier error
    // — the config-watch row must stop reading red once it reloads
    // cleanly.
    dashboard.set_note_tone(&update.name, update.note_tone.unwrap_or_default());
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
    use crate::DashboardState;
    use crate::contracts::ServiceStatusUpdate;
    use crate::dashboard_slot;
    use crate::nav::DashboardNav;
    use jinn_slices::NoteTone;
    use jinn_slices::Slices;
    use jinn_slices::TypedCell;
    use jinn_testutil::TestFabric;
    use trouper::actor::ActorKind;
    use trouper::schema::Schema;

    /// Polls `check` until it passes or the bounded retry budget runs out.
    /// A snapshot of one dashboard row, by field name.
    ///
    /// Named fields rather than a positional tuple: these tests assert
    /// on one field at a time, and `row.status` says which one a failure
    /// is about.
    #[derive(Debug, Clone)]
    struct Row {
        lifecycle: ActorLifecycle,
        status: Option<String>,
        description: Option<String>,
        tone: NoteTone,
    }

    /// Reads the named row out of the dashboard cell.
    fn row(cell: &TypedCell<DashboardState>, name: &str) -> Option<Row> {
        let s = cell.read();
        s.actors().iter().find(|e| e.name == name).map(|e| Row {
            lifecycle: e.lifecycle,
            status: e.status_message.clone(),
            description: e.description.clone(),
            tone: e.note_tone,
        })
    }

    /// The names of every row currently in the cell, in display order.
    fn row_names(cell: &TypedCell<DashboardState>) -> Vec<String> {
        cell.read()
            .actors()
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect()
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

    /// A bare service actor that handles and emits nothing, spawned at
    /// `name`. Spawning one of these is the strongest form of the
    /// census claim: the runtime announces it and nothing in jinn ever
    /// names it, so a row can only come from the runtime.
    struct CensusProbe;

    impl trouper::actor::ServiceActor for CensusProbe {
        fn manifest() -> trouper::schema::ActorManifest {
            trouper::schema::ActorManifest::new()
        }
        async fn init(
            _args: &trouper::json::Json,
        ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
            Ok(Self)
        }
    }

    /// Spawns a [`CensusProbe`] and returns its path.
    fn spawn_probe(system: &ActorSystem, name: &str) -> ActorPath {
        trouper::builder::spawn_service_builder::<CensusProbe>(system)
            .at(ActorPath::new(name))
            .start()
    }

    /// A runtime spawn announcement for `path`.
    fn running(path: &str) -> trouper::ActorLifecycle {
        trouper::ActorLifecycle {
            path: ActorPath::new(path),
            state: LifecycleState::Running,
            kind: ActorKind::Service,
        }
    }

    /// A runtime stop announcement for `path` with `state`.
    fn stopped(path: &str, state: LifecycleState) -> trouper::ActorLifecycle {
        trouper::ActorLifecycle {
            path: ActorPath::new(path),
            state,
            kind: ActorKind::Service,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_spawn_announcement_creates_a_running_row() {
        // Given a dashboard canvas actor subscribed to the census.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When a spawn announcement lands.
        fabric.send_to_topic(running("session")).await;

        // Then the actor has a row, and it reads Running.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "session").is_some_and(|r| r.lifecycle == ActorLifecycle::Running)
        })
        .await;
    }

    /// THE decisive test: a row appears for an actor that publishes no
    /// jinn-level event of any kind. The dashboard used to learn about
    /// actors only from hand-written publishes at two spawn sites, so it
    /// listed two of roughly forty-five live actors.
    #[rstest::rstest]
    #[tokio::test]
    async fn an_actor_that_publishes_nothing_gets_a_row() {
        // Given a dashboard canvas actor subscribed to the census.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When the runtime announces a spawn for an arbitrary path —
        // no `ServiceStatusUpdate`, no feature of any kind involved.
        fabric.send_to_topic(running("mcp/coordinator")).await;

        // Then the row exists purely because the runtime said so.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "mcp/coordinator").is_some_and(|r| r.lifecycle == ActorLifecycle::Running)
        })
        .await;
    }

    /// The dashboard's census is fed by the runtime's announcement, not
    /// by any observation handler — so it must work on a system where
    /// one was never installed. `TestFabric` builds a production-config
    /// system, which installs none.
    #[rstest::rstest]
    #[tokio::test]
    async fn rows_appear_without_any_observation_handler() {
        // Given a fabric with no observation handler installed.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When actors spawn.
        let paths: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|name| spawn_probe(fabric.system(), name).to_string())
            .collect();

        // Then every row exists, keyed by the runtime's own path.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            let names = row_names(&cell);
            paths.iter().all(|p| names.contains(p))
        })
        .await;
    }

    /// A passivated actor is DORMANT, not a failure. Partition-set
    /// entities (per-session discovery workers, for one) passivate on an
    /// idle window and re-spawn on the next send, so a row reading
    /// "Crashed" reports a failure that did not happen.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_passivated_actor_reads_as_idle() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("jinn.discovery/abc")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "jinn.discovery/abc").is_some()
        })
        .await;

        // When the runtime announces a passivation.
        fabric
            .send_to_topic(stopped("jinn.discovery/abc", LifecycleState::Passivated))
            .await;

        // Then the row reads Idle — the actor is dormant, not gone.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "jinn.discovery/abc").is_some_and(|r| r.lifecycle == ActorLifecycle::Idle)
        })
        .await;
    }

    /// Idle needs no note. The state word already says the actor is
    /// dormant; "idle; re-spawns on next send" would restate the column
    /// that should be reserved for reasons a reader cannot infer.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_passivated_actor_has_no_note() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("idle-worker")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "idle-worker").is_some()
        })
        .await;

        // When the runtime announces a passivation.
        fabric
            .send_to_topic(stopped("idle-worker", LifecycleState::Passivated))
            .await;

        // Then the row is Idle with an empty Notes cell.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "idle-worker").is_some_and(|r| r.lifecycle == ActorLifecycle::Idle)
        })
        .await;
        let entry = row(&cell, "idle-worker").unwrap();
        assert_eq!(entry.status, None, "and no feature status message");
    }

    /// A dormant actor that wakes is Running again: the row is live, and
    /// a stale failure would misreport the present.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_passivated_actor_that_wakes_reads_as_running() {
        // Given a wired actor whose row went Idle.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("wake-me")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "wake-me").is_some()
        })
        .await;
        fabric
            .send_to_topic(stopped("wake-me", LifecycleState::Passivated))
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "wake-me").is_some_and(|r| r.lifecycle == ActorLifecycle::Idle)
        })
        .await;

        // When the partition factory re-spawns it on the next send.
        fabric.send_to_topic(running("wake-me")).await;

        // Then the row is Running.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "wake-me").is_some_and(|r| r.lifecycle == ActorLifecycle::Running)
        })
        .await;
    }

    /// A crash keeps its row. The actor will never re-announce itself —
    /// there is nothing left to announce — so dropping the row would erase
    /// the only evidence that it broke.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_crashed_actor_keeps_its_row_reading_crashed() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("worker")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "worker").is_some()
        })
        .await;

        // When the runtime announces a crash.
        fabric
            .send_to_topic(stopped("worker", LifecycleState::Crashed))
            .await;

        // Then the row survives, reading Crashed.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "worker").is_some_and(|r| r.lifecycle == ActorLifecycle::Crashed)
        })
        .await;
    }

    /// Escalation is a worse outcome than a crash, and the State column
    /// must name which of the two happened.
    #[rstest::rstest]
    #[tokio::test]
    async fn an_escalated_actor_reads_as_escalated_not_crashed() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("worker")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "worker").is_some()
        })
        .await;

        // When the runtime escalates it.
        fabric
            .send_to_topic(stopped("worker", LifecycleState::Escalated))
            .await;

        // Then the row reads Escalated.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "worker").is_some_and(|r| r.lifecycle == ActorLifecycle::Escalated)
        })
        .await;
    }

    /// THE bug this change exists for. Every session archive tears down
    /// that session's MCP connection actors, and every subagent teardown
    /// retires its listener actors — all of them sanctioned stops. Retaining
    /// those rows turned the dashboard into a wall of red entries that
    /// grew with every archive and never came back down.
    #[rstest::rstest]
    #[case(LifecycleState::Normal)]
    #[case(LifecycleState::Shutdown)]
    #[tokio::test]
    async fn a_sanitized_stop_removes_the_row(#[case] state: LifecycleState) {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric
            .send_to_topic(running("jinn.mcp.connection.abc"))
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "jinn.mcp.connection.abc").is_some()
        })
        .await;

        // When the runtime announces the sanctioned stop.
        fabric
            .send_to_topic(stopped("jinn.mcp.connection.abc", state))
            .await;

        // Then the row is gone from the list entirely.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            !row_names(&cell).contains(&"jinn.mcp.connection.abc".to_owned())
        })
        .await;
    }

    /// Repeated teardowns leave the list flat rather than growing one
    /// row per archived session.
    #[rstest::rstest]
    #[tokio::test]
    async fn repeated_sanitized_stops_do_not_accumulate_rows() {
        // Given a wired actor and a surviving long-lived row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("long-lived")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "long-lived").is_some()
        })
        .await;
        // The census also lists the dashboard actor itself, so this
        // pins the transient rows' absence rather than a total count.
        let transient: Vec<String> = (0..5)
            .map(|i| format!("jinn.tools.subagent-settle-listener.{i}"))
            .collect();

        // When five transient actors each start and then stop cleanly.
        for name in &transient {
            fabric.send_to_topic(running(name)).await;
            jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
                row(&cell, name).is_some()
            })
            .await;
            fabric
                .send_to_topic(stopped(name, LifecycleState::Normal))
                .await;
        }

        // Then every one of them is gone, leaving the list no taller
        // than it was.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            let names = row_names(&cell);
            transient.iter().all(|name| !names.contains(name))
        })
        .await;
        assert!(row_names(&cell).contains(&"long-lived".to_owned()));
    }

    /// A failure for a path the dashboard never saw start still produces
    /// a row, or a missed spawn announcement would drop the failure.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_failure_for_an_unseen_actor_still_creates_its_row() {
        // Given a wired actor with no row for this path.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When a crash announcement arrives for an actor it never saw.
        fabric
            .send_to_topic(stopped("vanished", LifecycleState::Crashed))
            .await;

        // Then the row exists and reads Crashed.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "vanished").is_some_and(|r| r.lifecycle == ActorLifecycle::Crashed)
        })
        .await;
    }

    /// A stop for an unseen actor is nothing to report, so it leaves no
    /// row: the sanitized path does not resurrect what it also deletes.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_sanitized_stop_for_an_unseen_actor_creates_no_row() {
        // Given a wired actor with no row for this path.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);

        // When a shutdown announcement arrives for an actor it never saw.
        fabric
            .send_to_topic(stopped("vanished", LifecycleState::Shutdown))
            .await;
        // The canvas actor's inbox draining is the positive signal that the
        // announcement was *processed*. Asserting the absence before that
        // would prove nothing — the actor might simply not have run yet.
        jinn_testutil::wait::await_inbox_drained(
            fabric.system(),
            &trouper::actor::ActorPath::new("dashboard"),
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then no row was conjured up for it.
        assert!(
            !row_names(&cell).contains(&"vanished".to_owned()),
            "a clean stop has nothing to report: {:?}",
            row_names(&cell)
        );
    }

    /// A crash does not erase the owning feature's last word on the row:
    /// the note is the only column carrying detail about what the actor
    /// was doing.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_failure_preserves_the_features_status_message() {
        // Given a wired actor whose row carries a feature status message.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("discord")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "discord").is_some()
        })
        .await;
        fabric
            .send_to_topic(ServiceStatusUpdate {
                name: "discord".to_owned(),
                description: Some("Discord gateway".to_owned()),
                status_message: Some("connected".to_owned()),
                note_tone: None,
            })
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "discord").is_some_and(|r| r.status.as_deref() == Some("connected"))
        })
        .await;

        // When the actor crashes.
        fabric
            .send_to_topic(stopped("discord", LifecycleState::Crashed))
            .await;

        // Then the row survives with its feature message and description.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "discord").is_some_and(|r| r.lifecycle == ActorLifecycle::Crashed)
        })
        .await;
        let row = row(&cell, "discord").unwrap();
        assert_eq!(row.status.as_deref(), Some("connected"));
        assert_eq!(row.description.as_deref(), Some("Discord gateway"));
    }

    /// A respawn clears the failure: the row is live again, and keeping
    /// the old verdict would misreport the present.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_respawn_clears_a_prior_failure() {
        // Given a wired actor whose row went Crashed.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("flaky")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "flaky").is_some()
        })
        .await;
        fabric
            .send_to_topic(stopped("flaky", LifecycleState::Crashed))
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "flaky").is_some_and(|r| r.lifecycle == ActorLifecycle::Crashed)
        })
        .await;

        // When the runtime announces it running again — which is exactly
        // what a supervised restart does, indistinguishably.
        fabric.send_to_topic(running("flaky")).await;

        // Then the row reads Running again.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "flaky").is_some_and(|r| r.lifecycle == ActorLifecycle::Running)
        })
        .await;
    }

    /// A failure sorts to the top of the dashboard, above every healthy
    /// actor — the whole point of ordering by brokenness.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_actor_sorts_to_the_top() {
        // Given a wired actor with several healthy rows and one failure.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        for name in ["a", "b", "c"] {
            fabric.send_to_topic(running(name)).await;
        }
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            let names = row_names(&cell);
            ["a", "b", "c"]
                .iter()
                .all(|n| names.contains(&(*n).to_owned()))
        })
        .await;
        fabric
            .send_to_topic(stopped("b", LifecycleState::Crashed))
            .await;

        // When reading the display order.
        // Then the failed row sorts to the top of the dashboard.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row_names(&cell).first() == Some(&"b".to_owned())
        })
        .await;
    }

    /// The dashboard folds the runtime's OWN type. Delivery is by schema
    /// id, so a schema-identical mirror would silently drop every
    /// announcement — the exact failure this slice once had. Pinning the
    /// type identity makes a future mirror a compile error rather than
    /// an empty dashboard.
    #[rstest::rstest]
    #[test]
    fn the_census_message_is_the_runtime_type_under_its_bare_name() {
        // Given the type the dashboard declares `.handles` on.
        let announcement = running("llm");

        // When reading its schema id and round-tripping it.
        let id = trouper::ActorLifecycle::schema_id().to_string();
        let roundtripped: trouper::ActorLifecycle =
            serde_json::from_value(serde_json::to_value(&announcement).unwrap()).unwrap();

        // Then the id is the bare type name (no version component), and
        // the payload survived the trip.
        assert_eq!(id, "ActorLifecycle", "schema id was {id}");
        assert_eq!(roundtripped.path, ActorPath::new("llm"));
        assert_eq!(roundtripped.state, LifecycleState::Running);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_feature_status_message_sets_the_notes_column() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("sample-actor")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "sample-actor").is_some()
        })
        .await;

        // When a ServiceStatusUpdate projection arrives with a status message.
        fabric
            .send_to_topic(ServiceStatusUpdate {
                name: "sample-actor".to_owned(),
                description: None,
                status_message: Some("3 urls verified".to_owned()),
                note_tone: None,
            })
            .await;

        // Then the row carries the message.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "sample-actor")
                .is_some_and(|r| r.status.as_deref() == Some("3 urls verified"))
        })
        .await;
    }

    /// A feature colours its own note without touching the row's state:
    /// the note tone lands, and the lifecycle the runtime last announced
    /// is untouched.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_feature_note_tone_does_not_change_the_row_state() {
        // Given a wired actor with a running row.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("gateway")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "gateway").is_some()
        })
        .await;

        // When the owning feature reports a failure with an error tone.
        fabric
            .send_to_topic(ServiceStatusUpdate {
                name: "gateway".to_owned(),
                description: None,
                status_message: Some("401: invalid bot token".to_owned()),
                note_tone: Some(NoteTone::Error),
            })
            .await;

        // Then the tone lands on the row.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "gateway").is_some_and(|r| r.tone == NoteTone::Error)
        })
        .await;
        // And the State cell still reports what the runtime last said.
        let row = row(&cell, "gateway").unwrap();
        assert_eq!(
            row.lifecycle,
            ActorLifecycle::Running,
            "a feature cannot declare its own actor dead"
        );
    }

    /// A toneless follow-up un-flags an earlier error: a feature saying
    /// "this is ordinary now" (e.g. the config-watch row reloading
    /// cleanly after a refused edit) must stop the row reading red.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_toneless_update_clears_an_earlier_error_tone() {
        // Given a wired actor whose row carries an error-toned note.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        fabric.send_to_topic(running("config")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "config").is_some()
        })
        .await;
        fabric
            .send_to_topic(ServiceStatusUpdate {
                name: "config".to_owned(),
                description: None,
                status_message: Some("refused: [skills] is malformed".to_owned()),
                note_tone: Some(NoteTone::Error),
            })
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "config").is_some_and(|r| r.tone == NoteTone::Error)
        })
        .await;

        // When a later, toneless update lands (the clean reload).
        fabric
            .send_to_topic(ServiceStatusUpdate {
                name: "config".to_owned(),
                description: None,
                status_message: Some("reloaded jinn.toml".to_owned()),
                note_tone: None,
            })
            .await;

        // Then the row reads ordinary again.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "config").is_some_and(|r| r.tone == NoteTone::Muted)
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn nav_messages_move_the_selection_cursor() {
        // Given a wired actor with three named entries among its rows.
        // NOTE: the census also lists the dashboard actor itself, so
        // this asserts on the named rows, never on a total count.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        for name in ["a", "b", "c"] {
            fabric.send_to_topic(running(name)).await;
        }
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            let names = row_names(&cell);
            ["a", "b", "c"]
                .iter()
                .all(|n| names.contains(&(*n).to_owned()))
        })
        .await;

        // When DashboardNav::Down envelopes arrive twice.
        fabric.send_to_topic(DashboardNav::Down).await;
        fabric.send_to_topic(DashboardNav::Down).await;

        // Then the cursor advances twice.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            cell.read().selected_index() == 2
        })
        .await;
    }

    /// The cursor addresses a position, so a row that sorts away from
    /// under it leaves the cursor where it is — the alternative (following
    /// the row) makes `j` jump to wherever the target happened to land.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_re_sort_leaves_the_cursor_where_it_is() {
        // Given a wired actor with rows the cursor sits past the start of.
        let fabric = TestFabric::new();
        let cell = wire_actor(&fabric);
        for name in ["a", "b", "c", "d"] {
            fabric.send_to_topic(running(name)).await;
        }
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            let names = row_names(&cell);
            ["a", "b", "c", "d"]
                .iter()
                .all(|n| names.contains(&(*n).to_owned()))
        })
        .await;
        for _ in 0..2 {
            fabric.send_to_topic(DashboardNav::Down).await;
        }
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            cell.read().selected_index() == 2
        })
        .await;
        let before = row_names(&cell)[2].clone();

        // When a later-announced actor fails and sorts to the top.
        fabric.send_to_topic(running("e")).await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row(&cell, "e").is_some()
        })
        .await;
        fabric
            .send_to_topic(stopped("e", LifecycleState::Crashed))
            .await;
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            row_names(&cell).first() == Some(&"e".to_owned())
        })
        .await;

        // Then the cursor still holds its POSITION, and that position
        // now addresses a different actor — the failed row slid to the
        // top and everything under it shifted down by one.
        assert_eq!(cell.read().selected_index(), 2);
        let after = row_names(&cell);
        assert_ne!(
            after[2], before,
            "the row that slid away must not drag the cursor with it"
        );
        // And the row the cursor used to address is still present, one
        // position further down: it moved, the cursor did not follow.
        let moved_to = after
            .iter()
            .position(|name| *name == before)
            .expect("the previously-selected row survives");
        assert_eq!(moved_to, 3, "it shifted down by exactly one row");
    }
}
