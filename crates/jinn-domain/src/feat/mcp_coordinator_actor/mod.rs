//! MCP lifecycle actor — spawns and kills `McpActor`s.
//!
//! One instance lives for the whole app. It watches session lifecycle events
//! and the per-session enablement command, keeping exactly one `McpActor` alive
//! per (session × enabled-server) pair:
//!
//! - [`SessionLoadCompleted`] — a session restored from disk; spawn actors for
//!   its persisted `enabled_mcp_servers`.
//! - [`SessionCreated`] — a freshly created session; reconcile against its
//!   actual `enabled_mcp_servers` (empty for legacy sessions, possibly
//!   config-seeded via `auto_enable`).
//! - [`McpEnablementChanged`] — the picker committed a new desired set; diff
//!   against the spawned map and spawn/kill the delta.
//! - [`SessionClosed`] / [`SessionArchived`] / [`SessionTeardownFinished`] —
//!   the session is gone; kill all its actors.
//!
//! Each `McpActor` is a supervised child of the root supervisor
//! ([`kameo::Actor::supervise`]) with [`RestartPolicy::Never`], so a single
//! dead server's crash never cascades and never restarts (the user re-enables
//! it). Disabling a server (or closing the session) calls
//! [`ActorRef::stop_gracefully`], which triggers the `McpActor::on_stop` hook
//! that shuts the child process down.

pub mod protocol;

use std::collections::{BTreeSet, HashMap};

use kameo::actor::{ActorRef, Spawn};
use kameo::prelude::{Context, Message};
use kameo::supervision::RestartPolicy;
use parking_lot::Mutex;

use crate::Services;
use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::root_supervisor::RootSupervisorRef;
use crate::common::services::bus_service::BusService;
use crate::feat::mcp::McpServerConfig;
use crate::feat::mcp_actor::protocol::{McpServerLog, McpServerStatus};
use crate::feat::mcp_actor::{ConnectionState, McpActor, McpActorDeps};
use crate::feat::mcp_coordinator_actor::protocol::{
    McpEnablementChanged, RestartError, RestartMcpServer,
};
use crate::feat::session::protocol::session_archived::SessionArchived;
use crate::feat::session::protocol::session_closed::SessionClosed;
use crate::feat::session::protocol::session_load_completed::SessionLoadCompleted;
use crate::feat::session_lifecycle::protocol::event::{SessionCreated, SessionTeardownFinished};
use crate::protocol::SessionId;

/// Key into the spawned-actor map: one `McpActor` per (session × server).
type SpawnKey = (SessionId, String);

/// Maximum time to wait for a restarted `McpActor`'s `on_start` to connect.
///
/// Slow-to-boot HTTP/Python servers can legitimately take tens of seconds;
/// this bounds the tool loop so a wedged server doesn't hang it forever.
/// On timeout the tool reports failure with the STOP-and-wait instruction.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "60s is the intent, not 1min"
)]
const RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The MCP lifecycle actor.
pub struct McpCoordinatorActor {
    deps: ActorDeps,
    root: RootSupervisorRef,
    state: crate::common::state::State,
    cap: crate::common::tcaps::SessionCap,
    /// Tracks every live `McpActor` by (session_id, server_name).
    /// Guarded by a mutex so spawn/kill helpers can borrow `self` while
    /// mutating the map without fighting the borrow checker.
    spawned: Mutex<HashMap<SpawnKey, ActorRef<McpActor>>>,
}

/// Dependencies for [`McpCoordinatorActor`].
#[derive(Clone)]
pub struct McpCoordinatorActorDeps {
    /// Common actor dependencies (services + bus).
    pub deps: ActorDeps,
    /// Root supervisor — `McpActor`s are supervised children of it.
    pub root: RootSupervisorRef,
    /// Shared application state — the per-session MCP server status map is
    /// written here.
    pub state: crate::common::state::State,
    /// Capability to write the session collection.
    pub cap: crate::common::tcaps::SessionCap,
}

impl kameo::Actor for McpCoordinatorActor {
    type Args = McpCoordinatorActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.clone().recipient::<SessionLoadCompleted>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<SessionCreated>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<McpEnablementChanged>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<SessionClosed>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<SessionArchived>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<SessionTeardownFinished>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<RestartMcpServer>())
            .await;
        args.deps
            .subscribe(actor_ref.clone().recipient::<McpServerStatus>())
            .await;
        args.deps
            .subscribe(actor_ref.recipient::<McpServerLog>())
            .await;

        Ok(Self {
            deps: args.deps,
            root: args.root,
            state: args.state,
            cap: args.cap,
            spawned: Mutex::new(HashMap::new()),
        })
    }
}

impl BusPublish for McpCoordinatorActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl McpCoordinatorActor {
    /// Reconciles the spawned-actor map for one session against a desired set.
    ///
    /// Spawns actors for newly-enabled servers, kills actors for
    /// newly-disabled servers. Idempotent: calling with the current set is a
    /// no-op.
    async fn reconcile(&self, session_id: &SessionId, desired: &BTreeSet<String>) {
        let configs = configured_servers(&self.deps.services);

        // Partition the current spawned entries for this session into
        // to-keep and to-kill, based on the desired set.
        let to_kill: Vec<SpawnKey> = {
            let spawned = self.spawned.lock();
            spawned
                .keys()
                .filter(|(sid, _)| sid == session_id)
                .filter(|(_, server)| !desired.contains(server))
                .cloned()
                .collect()
        };

        // Spawn any desired server not yet running. Guarded against duplicate
        // spawns: if an entry already exists for this key, skip it.
        let to_spawn: Vec<String> = {
            let spawned = self.spawned.lock();
            desired
                .iter()
                .filter(|server| !spawned.contains_key(&(session_id.clone(), (*server).clone())))
                .cloned()
                .collect()
        };

        for server in to_spawn {
            if let Some((name, config)) = configs.iter().find(|(n, _)| n == &server) {
                let _ = self.spawn_one(session_id, name, config).await;
            } else {
                tracing::warn!(
                    server = %server,
                    %session_id,
                    "MCP lifecycle: enabled server not found in jinn.toml [mcp_server.<name>], skipping spawn"
                );
            }
        }

        for key in to_kill {
            self.kill_one(&key).await;
        }
    }

    /// Spawns a single `McpActor` for a (session, server) pair and records it.
    ///
    /// Returns the spawned actor's ref so the caller can `wait_for_startup`
    /// and query its connection state (used by [`restart_one`](Self::restart_one)).
    /// `None` if a duplicate-spawn guard fires (another reconcile already
    /// inserted this key).
    async fn spawn_one(
        &self,
        session_id: &SessionId,
        name: &str,
        config: &McpServerConfig,
    ) -> Option<ActorRef<McpActor>> {
        let key = (session_id.clone(), name.to_owned());
        // Duplicate-spawn guard: another in-flight reconcile may have inserted
        // this key between the snapshot and now.
        if self.spawned.lock().contains_key(&key) {
            return None;
        }

        let actor_ref = McpActor::supervise(
            &self.root,
            McpActorDeps::new(
                self.deps.clone(),
                session_id.clone(),
                name.to_owned(),
                config.clone(),
            ),
        )
        .restart_policy(RestartPolicy::Never)
        .spawn()
        .await;

        self.spawned.lock().insert(key, actor_ref.clone());
        tracing::info!(
            server = %name,
            %session_id,
            "MCP lifecycle: spawned McpActor"
        );
        Some(actor_ref)
    }

    /// Stops a single tracked `McpActor` and removes it from the map.
    ///
    /// `stop_gracefully` triggers `McpActor::on_stop`, which shuts the child
    /// process down.
    async fn kill_one(&self, key: &SpawnKey) {
        let actor_ref = self.spawned.lock().remove(key);
        if let Some(actor_ref) = actor_ref {
            let _ = actor_ref.stop_gracefully().await;
            tracing::info!(
                server = %key.1,
                session_id = %key.0,
                "MCP lifecycle: stopped McpActor"
            );
        }
    }

    /// Kills every `McpActor` for a session (used on close/archive/teardown).
    async fn kill_all_for_session(&self, session_id: &SessionId) {
        let keys: Vec<SpawnKey> = self
            .spawned
            .lock()
            .keys()
            .filter(|(sid, _)| sid == session_id)
            .cloned()
            .collect();
        for key in keys {
            self.kill_one(&key).await;
        }
    }

    /// Restarts a single (session × server) `McpActor`: kills the running one
    /// (if any) and respawns it from its configured server entry, then awaits
    /// the new actor's `on_start` and asks it whether it connected.
    ///
    /// This is **deterministic** — unlike the old bus-event approach, the
    /// result reflects the new actor's actual connection state, queried
    /// directly via [`McpActor`]'s `ConnectionState` message after
    /// `wait_for_startup`. No event-ordering race.
    async fn restart_one(&self, session_id: &SessionId, server: &str) -> Result<(), RestartError> {
        self.restart_one_with_timeout(session_id, server, RESTART_TIMEOUT)
            .await
    }

    /// Same as [`restart_one`](Self::restart_one) but with an injectable
    /// `on_start`+connect timeout (for tests).
    async fn restart_one_with_timeout(
        &self,
        session_id: &SessionId,
        server: &str,
        timeout: std::time::Duration,
    ) -> Result<(), RestartError> {
        let key = (session_id.clone(), server.to_owned());
        self.kill_one(&key).await;

        let config = configured_servers(&self.deps.services)
            .into_iter()
            .find(|(n, _)| n == server)
            .ok_or(RestartError::UnknownServer)?;

        let actor_ref = self.spawn_one(session_id, &config.0, &config.1).await;

        let actor_ref = actor_ref.ok_or(RestartError::UnknownServer)?;

        // `on_start` blocks on acquire_client (connect + tools/list); we wait
        // for it to complete, bounded by the restart timeout so a slow-boot
        // server can't hang the tool loop forever.
        let connected = match tokio::time::timeout(timeout, async {
            actor_ref.wait_for_startup().await;
            actor_ref
                .ask(ConnectionState)
                .await
                .map_err(|_send_err| RestartError::Mailbox)
        })
        .await
        {
            Ok(Ok(connected)) => connected,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(RestartError::Timeout),
        };

        if connected {
            Ok(())
        } else {
            Err(RestartError::ConnectFailed)
        }
    }
}

/// Reads the configured `[mcp_server.<name>]` entries from user preferences.
fn configured_servers(services: &Services) -> Vec<(String, McpServerConfig)> {
    let prefs = services.user_preferences_storage.read();
    prefs
        .mcp_server
        .iter()
        .map(|(n, c)| (n.clone(), c.clone()))
        .collect()
}

// ── Message handlers ─────────────────────────────────────────────────────

impl Message<SessionLoadCompleted> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionLoadCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        // Given a session restored from disk.
        let session_id = msg.session.session_id().clone();
        let enabled = msg.session.enabled_mcp_servers().clone();

        // When reconciling its enablement.
        self.reconcile(&session_id, &enabled).await;
    }
}

impl Message<SessionCreated> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionCreated, _ctx: &mut Context<Self, Self::Reply>) {
        // Given a freshly created session.
        // Sessions may carry config-seeded enablement (`auto_enable` in
        // jinn.toml); reconcile against the session's actual set rather than
        // assuming empty. Mirrors the `SessionLoadCompleted` sibling: for
        // legacy sessions the set is empty and this is a no-op; when
        // `McpEnablementChanged` also arrives with the same desired set,
        // reconciliation is diff-based so the duplicate converges harmlessly.
        let enabled = self
            .state
            .read()
            .session
            .get(&msg.session_id)
            .map(|s| s.enabled_mcp_servers().clone())
            .unwrap_or_default();
        self.reconcile(&msg.session_id, &enabled).await;
    }
}

impl Message<McpEnablementChanged> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: McpEnablementChanged, _ctx: &mut Context<Self, Self::Reply>) {
        // Given a new desired enablement set for a session.
        // When reconciling.
        self.reconcile(&msg.session_id, &msg.enabled).await;
    }
}

impl Message<SessionClosed> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut Context<Self, Self::Reply>) {
        self.kill_all_for_session(&msg.session_id).await;
    }
}

impl Message<SessionArchived> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionArchived, _ctx: &mut Context<Self, Self::Reply>) {
        self.kill_all_for_session(&msg.session_id).await;
    }
}

impl Message<SessionTeardownFinished> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SessionTeardownFinished,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        self.kill_all_for_session(&msg.session_id).await;
    }
}

impl Message<RestartMcpServer> for McpCoordinatorActor {
    type Reply = Result<(), RestartError>;

    async fn handle(
        &mut self,
        msg: RestartMcpServer,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.restart_one(&msg.session_id, &msg.server).await
    }
}

#[cfg(test)]
/// Test-only message: restart with an injectable timeout so tests can
/// exercise the `Err(Timeout)` path without a 60s wait.
pub struct RestartForTest {
    pub session_id: SessionId,
    pub server: String,
    pub timeout: std::time::Duration,
}

#[cfg(test)]
impl Message<RestartForTest> for McpCoordinatorActor {
    type Reply = Result<(), RestartError>;

    async fn handle(
        &mut self,
        msg: RestartForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.restart_one_with_timeout(&msg.session_id, &msg.server, msg.timeout)
            .await
    }
}

/// Writes a `McpServerStatus` transition into the owning session's status map.
///
/// This is the single owner of each session's `mcp_server_status` field.
/// There is no sync-sibling actor — the coordinator owns the full MCP
/// lifecycle domain, so it writes the status inline.
impl Message<McpServerStatus> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: McpServerStatus, _ctx: &mut Context<Self, Self::Reply>) {
        self.state.with_session(&self.cap, |view| {
            if let Some(session) = view.session.map().get_mut(&msg.session_id) {
                session.set_mcp_server_status(&msg.server, msg.status);
            }
        });
    }
}

/// Writes a captured stderr tail into the owning session's stderr map.
///
/// Like the status handler, the coordinator owns this field inline.
impl Message<McpServerLog> for McpCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, msg: McpServerLog, _ctx: &mut Context<Self, Self::Reply>) {
        self.state.with_session(&self.cap, |view| {
            if let Some(session) = view.session.map().get_mut(&msg.session_id) {
                session.set_mcp_server_stderr(&msg.server, msg.tail);
            }
        });
    }
}

#[cfg(test)]
mod lifecycle_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]

    use std::collections::BTreeSet;

    use kameo::actor::Spawn;

    use crate::common::actor_deps::ActorDeps;
    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::common::root_supervisor::RootSupervisor;
    use crate::feat::mcp::McpServerConfig;
    use crate::feat::mcp_actor::protocol::{McpConnectionStatus, McpServerStatus};
    use crate::feat::mcp_coordinator_actor::protocol::RestartError;
    use crate::feat::preferences_actor::user_preferences::UserPreferences;
    use crate::protocol::SessionId;

    use super::{McpCoordinatorActor, McpCoordinatorActorDeps};
    use crate::feat::mcp_coordinator_actor::protocol::McpEnablementChanged;
    use crate::feat::session::protocol::session_closed::SessionClosed;

    /// A configured MCP server whose command will never spawn successfully,
    /// so the spawned `McpActor` publishes Starting then Dead (never Running).
    fn unrunnable_server() -> McpServerConfig {
        McpServerConfig {
            command: Some("/this/command/does/not/exist".to_owned()),
            args: vec![],
            ..Default::default()
        }
    }

    /// A server whose command starts but never speaks the MCP protocol: the
    /// `initialize` handshake hangs forever, so `on_start` never completes.
    /// Used to exercise the `Err(Timeout)` path deterministically.
    fn hanging_server() -> McpServerConfig {
        McpServerConfig {
            command: Some("sleep".to_owned()),
            args: vec!["60".to_owned()],
            ..Default::default()
        }
    }

    async fn spawn_lifecycle(
        harness: &TestHarness,
        servers: &[(&str, McpServerConfig)],
    ) -> (
        kameo::actor::ActorRef<McpCoordinatorActor>,
        crate::Services,
        crate::common::state::State,
    ) {
        let services = harness.services().await;
        let mcp_server = servers
            .iter()
            .map(|(name, config)| ((*name).to_owned(), config.clone()))
            .collect();
        services
            .user_preferences_storage
            .save(&UserPreferences {
                mcp_server,
                ..UserPreferences::default()
            })
            .expect("seed prefs");
        let root = RootSupervisor::spawn_root().await;
        let state = crate::common::state::State::new(crate::common::app_state::AppState::default());
        let actor = McpCoordinatorActor::spawn(McpCoordinatorActorDeps {
            deps: ActorDeps {
                services: services.clone(),
            },
            root,
            state: state.clone(),
            cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;
        (actor, services, state)
    }

    fn single_enabled(server: &str) -> BTreeSet<String> {
        let mut s = BTreeSet::new();
        s.insert(server.to_owned());
        s
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enabling_a_configured_server_spawns_an_mcp_actor() {
        // Given a lifecycle actor with one configured server and a status recorder.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, _state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = SessionId::new();

        // When enabling that server for the session.
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;

        // Then the lifecycle actor spawned an McpActor that emitted a status event.
        // (The command is unrunnable, so the actor goes Starting -> Dead, but the
        //  fact that a status event arrived proves an McpActor was spawned.)
        let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(3)).await;
        assert!(
            !events.is_empty(),
            "enabling a configured server must spawn an McpActor that publishes a status"
        );
        assert_eq!(events[0].server, "unrunnable");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn enabling_unknown_server_spawns_nothing() {
        // Given a lifecycle actor with no configured servers.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, _state) = spawn_lifecycle(&harness, &[]).await;
        let session_id = SessionId::new();

        // When enabling a server that is not configured.
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("ghost"),
            })
            .await;

        // Then no McpActor is spawned (no status event arrives within a grace window).
        let events = await_recorded(&recorder, 1, std::time::Duration::from_millis(300)).await;
        assert!(
            events.is_empty(),
            "unknown server should spawn no actor, but got: {events:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn closing_a_session_does_not_panic_after_enable() {
        // Given a lifecycle actor with an enabled server for a session.
        let harness = TestHarness::new().await;
        let _recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, _state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = SessionId::new();
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // When closing the session.
        harness.publish(SessionClosed { session_id }).await;

        // Then the lifecycle actor does not panic and the test completes.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // (The observable behavior is clean teardown; a panic would surface here.)
    }

    /// Disabling a server tears down its actor: re-enabling afterwards must
    /// spawn a *fresh* actor (a new `Starting` status).
    ///
    /// Why this works with an unrunnable server: spawn#1's failed-connect leaves
    /// the entry in the spawned map (the actor stops itself, but nothing removes
    /// the map entry). Only `reconcile`/`kill_one` on disable removes it. So if
    /// disable works, re-enable's duplicate-spawn guard sees an empty slot and
    /// respawns — producing a second `Starting`. If disable were a no-op, the
    /// stale entry would block respawn and we'd see only one `Starting`.
    #[rstest::rstest]
    #[tokio::test]
    async fn disabling_then_re_enabling_respawns_the_actor() {
        // Given a lifecycle actor with one configured server.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, _state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = SessionId::new();

        // When enabling the server (spawn #1: Starting + Dead on failed connect).
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // And disabling it (must remove the map entry via kill_one).
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: BTreeSet::new(),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // And re-enabling it (spawn #2 — only possible if disable freed the slot).
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        // Let the full enable→disable→re-enable sequence settle so no events
        // are still in flight when we read the recorder. GetRecorded drains, so
        // settling first ensures await_recorded's first poll sees the whole
        // sequence at once and returns it intact (no mid-flight draining).
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        // Then two distinct `Starting` statuses arrived (one per spawn) — the
        // second only exists because disable freed the spawned-map slot.
        let events = await_recorded(&recorder, 3, std::time::Duration::from_secs(2)).await;
        let starting_count = events
            .iter()
            .filter(|e| e.status == McpConnectionStatus::Starting)
            .count();
        assert!(
            starting_count >= 2,
            "disable must tear down the actor so re-enable respawns it; \
             expected >=2 Starting events, got {starting_count}: {events:?}"
        );
    }

    /// A 1ms `restart_one` timeout fires before the new actor can finish
    /// `on_start` (connect + tools/list), so it returns `Err(Timeout)`.
    #[rstest::rstest]
    #[tokio::test]
    async fn restart_one_times_out_when_startup_exceeds_the_timeout() {
        // Given a coordinator with a server that hangs forever on the MCP handshake.
        let harness = TestHarness::new().await;
        let (actor, _services, _state) =
            spawn_lifecycle(&harness, &[("hanging", hanging_server())]).await;
        let session_id = SessionId::new();

        // When restarting with a 1ms timeout.
        let result = actor
            .ask(super::RestartForTest {
                session_id,
                server: "hanging".to_owned(),
                timeout: std::time::Duration::from_millis(1),
            })
            .await;

        // Then it returns Timeout (startup couldn't complete in 1ms).
        assert!(
            matches!(
                result,
                Err(kameo::error::SendError::HandlerError(RestartError::Timeout))
            ),
            "startup exceeding the timeout should yield Timeout; got: {result:?}"
        );
    }

    /// Inserts a fresh session carrying `enabled` into the harness's shared
    /// state and returns its id. Bypasses capability checks via
    /// `write_test_no_cap` (coordinator tests only hold their own cap).
    fn insert_session_with_enablement(
        state: &crate::common::state::State,
        enabled: &BTreeSet<String>,
    ) -> SessionId {
        let mut session = crate::feat::session::chat_session::ChatSessionState::new();
        session.set_enabled_mcp_servers(enabled.clone());
        let session_id = session.session_id().clone();
        let mut app_state = state.write_test_no_cap();
        app_state.session.insert(session);
        drop(app_state);
        session_id
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_created_spawns_actors_for_prepopulated_enablement() {
        // Given a coordinator with one configured server and a session whose
        // enabled set already contains it (config-seeded via auto_enable).
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = insert_session_with_enablement(&state, &single_enabled("unrunnable"));

        // When publishing SessionCreated for that session.
        harness
            .publish(
                crate::feat::session_lifecycle::protocol::event::SessionCreated {
                    session_id,
                    cwd: std::env::temp_dir(),
                },
            )
            .await;

        // Then an McpActor was spawned for the seeded server (a Starting
        // status arrives; Dead follows from the unrunnable command).
        let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(3)).await;
        assert!(
            !events.is_empty(),
            "SessionCreated must reconcile against the session's actual set"
        );
        assert_eq!(events[0].server, "unrunnable");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn duplicate_created_and_enablement_messages_spawn_only_once() {
        // Given a coordinator, a server, and a pre-populated session.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = insert_session_with_enablement(&state, &single_enabled("unrunnable"));

        // When both SessionCreated and McpEnablementChanged carry the same
        // desired set (the common seeding flow emits both).
        harness
            .publish(
                crate::feat::session_lifecycle::protocol::event::SessionCreated {
                    session_id: session_id.clone(),
                    cwd: std::env::temp_dir(),
                },
            )
            .await;
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        // Then exactly one spawn happened (a single Starting status): the
        // second reconciliation saw its slot already filled and was a no-op.
        let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        let starting_count = events
            .iter()
            .filter(|e| e.status == McpConnectionStatus::Starting)
            .count();
        assert_eq!(
            starting_count, 1,
            "duplicate notifications must converge to one spawn; got {events:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn created_reconcile_against_shrunk_set_kills_leftover_actor() {
        // Given a coordinator with an already-spawned actor for a session.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = {
            let sid = insert_session_with_enablement(&state, &single_enabled("unrunnable"));
            harness
                .publish(McpEnablementChanged {
                    session_id: sid.clone(),
                    enabled: single_enabled("unrunnable"),
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            sid
        };

        // When the session's enablement shrinks to empty and SessionCreated
        // reconciles against the shrunken set.
        {
            let mut app_state = state.write_test_no_cap();
            if let Some(s) = app_state.session.get_mut(&session_id) {
                s.set_enabled_mcp_servers(BTreeSet::new());
            }
        }
        harness
            .publish(
                crate::feat::session_lifecycle::protocol::event::SessionCreated {
                    session_id: session_id.clone(),
                    cwd: std::env::temp_dir(),
                },
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // And the server is enabled again.
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        // Then two distinct spawns happened (two Starting statuses) — the
        // second only exists because the shrink-to-empty reconcile killed the
        // first actor and freed its slot.
        let events = await_recorded(&recorder, 3, std::time::Duration::from_secs(2)).await;
        let starting_count = events
            .iter()
            .filter(|e| e.status == McpConnectionStatus::Starting)
            .count();
        assert!(
            starting_count >= 2,
            "shrink-to-empty must kill the leftover actor so re-enable respawns; \
             expected >=2 Starting events, got {starting_count}: {events:?}"
        );
    }
}

#[cfg(test)]
mod status_tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use kameo::actor::Spawn;

    use crate::common::actor_deps::ActorDeps;
    use crate::common::app_state::AppState;
    use crate::common::bus::test_harness::TestHarness;
    use crate::common::root_supervisor::RootSupervisor;
    use crate::common::state::State;
    use crate::feat::mcp_actor::protocol::{McpConnectionStatus, McpServerLog, McpServerStatus};
    use crate::feat::preferences_actor::user_preferences::UserPreferences;
    use crate::protocol::SessionId;

    use super::McpCoordinatorActor;
    use crate::feat::mcp_coordinator_actor::McpCoordinatorActorDeps;

    /// Spawns a coordinator and seeds one session into its state so status
    /// events for that session land somewhere to write.
    async fn spawn_with_session(harness: &TestHarness) -> (State, SessionId) {
        let services = harness.services().await;
        services
            .user_preferences_storage
            .save(&UserPreferences::default())
            .expect("seed prefs");
        let root = RootSupervisor::spawn_root().await;
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        // Insert an active session so the coordinator has a target to write to.
        state.write_test_no_cap().session.get_or_create(&session_id);
        let actor = McpCoordinatorActor::spawn(McpCoordinatorActorDeps {
            deps: ActorDeps {
                services: services.clone(),
            },
            root,
            state: state.clone(),
            cap: crate::common::tcaps::mint::mint_session_cap(),
        });
        actor.wait_for_startup().await;
        (state, session_id)
    }

    fn status_of(state: &State, sid: &SessionId, server: &str) -> Option<McpConnectionStatus> {
        let g = state.read();
        let s = g.session.get(sid)?;
        s.mcp_server_status().get(server).copied()
    }

    fn tail_of(state: &State, sid: &SessionId, server: &str) -> Option<String> {
        let g = state.read();
        let s = g.session.get(sid)?;
        s.mcp_server_stderr().get(server).cloned()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dead_status_is_written_to_session_map() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (state, session_id) = spawn_with_session(&harness).await;

        // When publishing a Dead status for one server.
        harness
            .publish(McpServerStatus {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Dead,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Then the session's status map shows Dead.
        assert_eq!(
            status_of(&state, &session_id, "excalimate"),
            Some(McpConnectionStatus::Dead)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn running_status_is_written_to_session_map() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (state, session_id) = spawn_with_session(&harness).await;

        // When publishing a Running status for one server.
        harness
            .publish(McpServerStatus {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Running,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Then the session's status map shows Running.
        assert_eq!(
            status_of(&state, &session_id, "excalimate"),
            Some(McpConnectionStatus::Running)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn status_for_one_session_is_not_visible_in_another() {
        // Given a coordinator with two seeded sessions.
        let harness = TestHarness::new().await;
        let (state, session_a) = spawn_with_session(&harness).await;
        let session_b = SessionId::new();
        state.write_test_no_cap().session.get_or_create(&session_b);

        // When publishing a Running status for session A only.
        harness
            .publish(McpServerStatus {
                session_id: session_a.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Running,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Then session A shows Running, but session B has no status for it.
        assert_eq!(
            status_of(&state, &session_a, "excalimate"),
            Some(McpConnectionStatus::Running)
        );
        assert_eq!(status_of(&state, &session_b, "excalimate"), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stderr_tail_is_written_to_session_map() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (state, session_id) = spawn_with_session(&harness).await;

        // When publishing a stderr tail for one server.
        harness
            .publish(McpServerLog {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                tail: "npm warn something".to_owned(),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Then the session's stderr map shows the latest tail.
        assert_eq!(
            tail_of(&state, &session_id, "excalimate"),
            Some("npm warn something".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stderr_tail_for_one_session_is_not_visible_in_another() {
        // Given a coordinator with two seeded sessions.
        let harness = TestHarness::new().await;
        let (state, session_a) = spawn_with_session(&harness).await;
        let session_b = SessionId::new();
        state.write_test_no_cap().session.get_or_create(&session_b);

        // When publishing a stderr tail for session A only.
        harness
            .publish(McpServerLog {
                session_id: session_a.clone(),
                server: "excalimate".to_owned(),
                tail: "only-in-a".to_owned(),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Then session A shows the tail, but session B has none.
        assert_eq!(
            tail_of(&state, &session_a, "excalimate"),
            Some("only-in-a".to_owned())
        );
        assert_eq!(tail_of(&state, &session_b, "excalimate"), None);
    }
}
