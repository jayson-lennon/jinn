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
//! A server configured with `singleton = true` is exempt from all of the
//! above session coupling: it spawns at most once per process, on the first
//! reconcile that finds it in any session's enablement set, and the teardown
//! events never kill it (they only ever walk the per-session map). Its status
//! and stderr land in the runtime cell's singleton bucket, readable without a
//! session id — which is also what the orchestrator's dispatch gate reads for
//! calls arriving from any session. The one behavior the singleton UI does not
//! get is a per-session inspector entry: the inspector renders session-keyed
//! state, so a singleton's live status shows only in the runtime cell, not in
//! the picker's connection list (an accepted gap, documented here).
//!
//! Each `McpActor` is spawned on the trouper system with no restart
//! policy, so a single dead server's crash never cascades and never
//! restarts (the user re-enables it). Disabling a server (or closing the
//! session) kills the actor, which triggers the `McpActor::on_stop` hook
//! that shuts the child process down.

use std::collections::{BTreeSet, HashMap};

use parking_lot::Mutex;
use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use crate::connection::{McpActor, McpActorDeps, McpConnectionStateProbe, McpConnectionStateReply};
use jinn_boot_msg::JinnTomlReloaded;
use jinn_core_types::SessionId;
use jinn_kernel::Services;
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::services::bus_service::BusService;
use jinn_mcp_msg::{McpEnablementChanged, McpRuntimeState, RestartError, RestartMcpServer};
use jinn_mcp_msg::{McpServerLog, McpServerStatus};
use jinn_preferences_config::schemas::mcp::McpServerConfig;
use jinn_preferences_config::schemas::mcp::McpServersConfig;
use jinn_session_lifecycle_msg::SessionCreated;
use jinn_session_lifecycle_msg::SessionTeardownFinished;
use jinn_session_msg::SessionArchived;
use jinn_session_msg::SessionClosed;
use jinn_session_store_msg::SessionLoadCompleted;
use jinn_slices::NoteTone;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::TypedCell;

/// Key into the spawned-actor map: one `McpActor` per (session × server).
type SpawnKey = (SessionId, String);

/// Maximum time to wait for a restarted `McpActor`'s `on_start` to connect.
///
/// Slow-to-boot HTTP/Python servers can legitimately take tens of seconds;
/// this bounds the tool loop so a wedged server doesn't hang it forever.
/// On timeout the tool reports failure with the STOP-and-wait instruction.
const RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Static path the coordinator spawns at (one instance per process).
pub const MCP_COORDINATOR_PATH: &str = "jinn.mcp.coordinator";

/// The MCP lifecycle actor.
pub struct McpCoordinatorActor {
    deps: ActorDeps,
    system: trouper::system::ActorSystem,
    state: jinn_kernel::common::state::State,
    runtime: TypedCell<McpRuntimeState>,
    /// Tracks every live `McpActor` by (session_id, server_name).
    /// Guarded by a mutex so spawn/kill helpers can borrow `self` while
    /// mutating the map without fighting the borrow checker.
    spawned: Mutex<HashMap<SpawnKey, ActorPath>>,
    /// Tracks every live singleton `McpActor` by server name.
    ///
    /// Deliberately a separate namespace from [`Self::spawned`]: a singleton
    /// has no owning session, so keying it under a synthetic `SessionId`
    /// would put it in the path every teardown handler sweeps — exactly the
    /// coupling the flag exists to break.
    singleton_spawned: Mutex<HashMap<String, ActorPath>>,
    /// The entry hash each running server was spawned under, keyed by
    /// server name.
    ///
    /// Seeded when a spawn lands; compared on a
    /// [`JinnTomlReloaded`]. A changed hash is an edited entry and
    /// triggers that server's auto-restart. The row describes *what the
    /// running process was started with*, so it outlives the entry
    /// vanishing from the config (a deleted entry leaves the connection,
    /// and an identical re-add is not an edit) and dies with the last
    /// actor for that server (dropped in [`Self::kill_one`]).
    entry_hashes: Mutex<HashMap<String, u64>>,
}

/// Dependencies for [`McpCoordinatorActor`].
#[derive(Clone)]
pub struct McpCoordinatorActorDeps {
    /// Common actor dependencies (services + bus).
    pub deps: ActorDeps,
    /// Shared application state used for durable session enablement.
    pub state: jinn_kernel::common::state::State,
    /// Runtime-only status and stderr projection written by the coordinator.
    pub runtime: TypedCell<McpRuntimeState>,
}

impl ServiceActor for McpCoordinatorActor {
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("McpCoordinatorActor spawns via init_with"))
    }
}

impl McpCoordinatorActor {
    /// Spawns the coordinator onto the trouper system.
    ///
    /// Returns only after all subscriptions are live, so composition's
    /// orchestrator-before-coordinator and coordinator-before-EnvironmentLoaded
    /// contracts hold by construction.
    pub async fn spawn(
        system: &trouper::system::ActorSystem,
        deps: McpCoordinatorActorDeps,
    ) -> ActorPath {
        let path = ActorPath::new(MCP_COORDINATOR_PATH);
        let system_for_start = system.clone();
        let builder = trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                move || {
                    let deps = deps.clone();
                    let system = system_for_start.clone();
                    Box::pin(async move {
                        Ok(Self {
                            deps: deps.deps,
                            system,
                            state: deps.state,
                            runtime: deps.runtime,
                            spawned: Mutex::new(HashMap::new()),
                            singleton_spawned: Mutex::new(HashMap::new()),
                            entry_hashes: Mutex::new(HashMap::new()),
                        })
                    })
                }
            })
            .handles::<SessionLoadCompleted>()
            .handles::<SessionCreated>()
            .handles::<McpEnablementChanged>()
            .handles::<SessionClosed>()
            .handles::<SessionArchived>()
            .handles::<SessionTeardownFinished>()
            .handles::<RestartMcpServer>()
            .handles::<McpServerStatus>()
            // Live config: a jinn.toml reload reconciles every session
            // against the re-read `[mcp.server.<name>]` entries.
            .handles::<JinnTomlReloaded>()
            .handles::<McpServerLog>()
            // Restart-ask replies leave the handler through ctx.reply; the
            // flush gate drops any outbound type not declared here.
            .emits::<McpRestartOutcome>()
            // The singleton visibility pass publishes per-session opt-outs
            // when a reconcile finds a singleton absent from (or back in) a
            // session's enabled set.
            .emits::<jinn_tools_msg::ProviderVisibilityChanged>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block);
        // Test-only ask surface: `RestartForTest` carries the injectable
        // startup budget the production `RestartMcpServer` message has no
        // field for. Without the entry the loop drops the ask silently
        // (no adapter for its schema) and the asker waits out its timeout.
        #[cfg(test)]
        let builder = builder.handles::<McpRestartForTest>();
        builder.start();
        path
    }
}

impl BusPublish for McpCoordinatorActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl McpCoordinatorActor {
    /// The content hash of one server's configured entry.
    fn entry_hash(config: &McpServerConfig) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(config, &mut hasher);
        hasher.finish()
    }

    /// Records the entry hash a spawn ran under.
    fn record_entry_hash(&self, name: &str, config: &McpServerConfig) {
        self.entry_hashes
            .lock()
            .insert(name.to_owned(), Self::entry_hash(config));
    }

    /// Drops the entry hash of a server whose last actor just stopped —
    /// the row describes what the process ran under, so with no process
    /// there is nothing to compare.
    fn forget_entry_hash(&self, name: &str) {
        self.entry_hashes.lock().remove(name);
    }

    /// Whether any actor for `server` is still tracked.
    fn has_running_actor(&self, server: &str) -> bool {
        self.spawned.lock().keys().any(|(_, name)| name == server)
            || self.singleton_spawned.lock().contains_key(server)
    }
}

impl McpCoordinatorActor {
    /// Clears all runtime-only status and stderr for one session.
    fn clear_runtime_for_session(&self, session_id: &SessionId) {
        self.runtime
            .update(|runtime| runtime.clear_session(session_id));
    }

    /// Reconciles the spawned-actor map for one session against a desired set.
    ///
    /// Spawns actors for newly-enabled servers, kills actors for
    /// newly-disabled servers. Idempotent: calling with the current set is a
    /// no-op.
    ///
    /// Singleton-configured servers never enter this per-session pass: any
    /// appearance in `desired` instead triggers the once-per-process
    /// singleton spawn, and their absence here never kills the shared actor.
    async fn reconcile(&self, session_id: &SessionId, desired: &BTreeSet<String>) {
        let configs = configured_servers(&self.deps.services);
        let singleton_names: Vec<&str> = configs
            .iter()
            .filter(|(_, config)| config.singleton)
            .map(|(name, _)| name.as_str())
            .collect();

        // Singleton pass: spawn each configured singleton found in the
        // desired set, at most once per process. Absence from `desired`
        // never kills — only an explicit restart does. But absence changes
        // what the session *sees*: the singleton registers globally, so a
        // session that disabled it needs its view of that provider withheld
        // (and a session that re-enabled it needs the view restored).
        for name in singleton_names.iter() {
            if desired.contains(*name) {
                let _ = self
                    .spawn_singleton_if_absent(configs.as_slice(), name)
                    .await;
            }
            let provider = jinn_mcp::tool_mapping::provider_name(name);
            let hidden = !desired.contains(*name);
            self.deps
                .services
                .bus
                .publish(jinn_tools_msg::ProviderVisibilityChanged {
                    provider: provider.clone(),
                    session_id: session_id.clone(),
                    hidden,
                })
                .await;
        }

        let per_session: BTreeSet<String> = desired
            .iter()
            .filter(|server| {
                !configs
                    .iter()
                    .any(|(n, config)| n == *server && config.singleton)
            })
            .cloned()
            .collect();

        // Partition the current spawned entries for this session into
        // to-keep and to-kill, based on the desired set.
        let to_kill: Vec<SpawnKey> = {
            let spawned = self.spawned.lock();
            spawned
                .keys()
                .filter(|(sid, _)| sid == session_id)
                .filter(|(_, server)| !per_session.contains(server))
                .cloned()
                .collect()
        };

        // Spawn any desired server not yet running. Guarded against duplicate
        // spawns: if an entry already exists for this key, skip it.
        let to_spawn: Vec<String> = {
            let spawned = self.spawned.lock();
            per_session
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
                    "MCP lifecycle: enabled server not found in jinn.toml [mcp.<name>], skipping spawn"
                );
            }
        }

        for key in to_kill {
            self.kill_one(&key).await;
        }
    }

    /// Spawns the singleton actor for `name` unless it is already running.
    ///
    /// The duplicate-spawn guard makes every reconcile converge: the first
    /// enablement that mentions the server spawns it, all later ones are
    /// no-ops regardless of which session they come from.
    async fn spawn_singleton_if_absent(
        &self,
        configs: &[(String, McpServerConfig)],
        name: &str,
    ) -> Option<ActorPath> {
        // Duplicate-spawn guard: a singleton is spawned at most once per
        // process — even while another reconcile is in flight (the same
        // guard window `spawn_one` closes for the per-session map).
        if self.singleton_spawned.lock().contains_key(name) {
            return None;
        }

        let (_, config) = configs.iter().find(|(n, _)| n == name)?;
        let path = McpActor::spawn(
            &self.system,
            McpActorDeps::new(
                self.deps.clone(),
                // The owning-session id only feeds status/log addressing and
                // the singleton actor ignores it; see `connection.rs`.
                SessionId::new(),
                name.to_owned(),
                config.clone(),
            )
            .as_singleton(),
        )
        .await;

        self.singleton_spawned
            .lock()
            .insert(name.to_owned(), path.clone());
        self.record_entry_hash(name, config);
        tracing::info!(server = %name, "MCP lifecycle: spawned singleton McpActor");
        Some(path)
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
    ) -> Option<ActorPath> {
        let key = (session_id.clone(), name.to_owned());
        // Duplicate-spawn guard: another in-flight reconcile may have inserted
        // this key between the snapshot and now.
        if self.spawned.lock().contains_key(&key) {
            return None;
        }

        let path = McpActor::spawn(
            &self.system,
            McpActorDeps::new(
                self.deps.clone(),
                session_id.clone(),
                name.to_owned(),
                config.clone(),
            ),
        )
        .await;

        self.spawned.lock().insert(key, path.clone());
        self.record_entry_hash(name, config);
        tracing::info!(
            server = %name,
            %session_id,
            "MCP lifecycle: spawned McpActor"
        );
        Some(path)
    }

    /// Stops a single tracked `McpActor` and removes it from the map.
    ///
    /// `stop_gracefully` triggers `McpActor::on_stop`, which shuts the child
    /// process down.
    async fn kill_one(&self, key: &SpawnKey) {
        let path = self.spawned.lock().remove(key);
        if let Some(path) = path {
            // With this actor gone, maybe no actor for the server is left:
            // the entry-hash row describes what the (now possibly absent)
            // process ran under, so it goes with the last one.
            if !self.has_running_actor(&key.1) {
                self.forget_entry_hash(&key.1);
            }
            self.system.stop(&path).await;
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
        let config = configured_servers(&self.deps.services)
            .into_iter()
            .find(|(n, _)| n == server)
            .ok_or(RestartError::UnknownServer)?;

        // A singleton restarts in its own namespace: kill/respawn the shared
        // actor by name, never the (session × server) per-session key.
        let actor_path = if config.1.singleton {
            {
                let existing = self.singleton_spawned.lock().remove(server);
                if let Some(path) = existing {
                    self.system.stop(&path).await;
                }
            }
            self.spawn_singleton_if_absent(&configured_servers(&self.deps.services), server)
                .await
                .ok_or(RestartError::UnknownServer)?
        } else {
            let key = (session_id.clone(), server.to_owned());
            self.kill_one(&key).await;
            self.spawn_one(session_id, &config.0, &config.1)
                .await
                .ok_or(RestartError::UnknownServer)?
        };

        // `on_start` blocks on acquire_client (connect + tools/list); we wait
        // for it to complete, bounded by the restart timeout so a slow-boot
        // server can't hang the tool loop forever. The trouper ask carries
        // its own MANDATORY timeout — the outer bound covers startup too.
        let reply = self
            .system
            .ask(actor_path, McpConnectionStateProbe, timeout)
            .await;
        let connected = match reply {
            Ok(value) => value
                .decode::<McpConnectionStateReply>()
                .map(|r| r.connected)
                .unwrap_or(false),
            Err(_) => return Err(RestartError::Timeout),
        };

        if connected {
            Ok(())
        } else {
            Err(RestartError::ConnectFailed)
        }
    }
}

/// Reads the configured `[mcp.server.<name>]` entries from the
/// configuration layer, at the point of use so a reload is observed
/// without a restart.
fn configured_servers(services: &Services) -> Vec<(String, McpServerConfig)> {
    services
        .config
        .get::<McpServersConfig>()
        .unwrap_or_default()
        .0
        .into_iter()
        .collect()
}

// ── Message handlers ─────────────────────────────────────────────────────

impl MsgHandler<SessionLoadCompleted> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &SessionLoadCompleted, _ctx: &mut MsgCtx<'_>) {
        // Given a session restored from disk.
        self.clear_runtime_for_session(msg.session_id());
        let enabled = self
            .state
            .read()
            .session
            .get(msg.session_id())
            .map(|session| session.enabled_mcp_servers().clone());

        // When reconciling its enablement.
        if let Some(enabled) = enabled {
            self.reconcile(msg.session_id(), &enabled).await;
        }
    }
}

impl MsgHandler<SessionCreated> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &SessionCreated, _ctx: &mut MsgCtx<'_>) {
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

impl MsgHandler<McpEnablementChanged> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &McpEnablementChanged, _ctx: &mut MsgCtx<'_>) {
        // Given a new desired enablement set for a session.
        // When reconciling.
        self.reconcile(&msg.session_id, &msg.enabled).await;
    }
}

// A jinn.toml reload re-reads `[mcp.server.<name>]` at the point of use
// (reconcile's first line), so every live session reconciles against the
// new entries: added servers spawn on the next reconcile pass, removed
// servers are killed. Enablement per session is untouched — a reload
// changes what *can* run, not what the user chose to run. No config
// write happens here: writing would re-trigger the config watcher.
//
// An *edited* entry (command, url, transport — anything the entry hash
// covers) does more: the running server restarts so the change applies
// without a session cycle. A *deleted* entry never kills a working
// connection — it runs until session end, and new sessions simply don't
// have it. A failed restart logs once and leaves the respawned process
// (which may be unconnected) in place; nothing retries.
impl MsgHandler<JinnTomlReloaded> for McpCoordinatorActor {
    async fn handle(&mut self, _msg: &JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        // Given a live config file whose MCP section just changed.
        let desired_by_session: Vec<(SessionId, BTreeSet<String>)> = self
            .state
            .read()
            .session
            .iter()
            .map(|(id, session)| (id.clone(), session.enabled_mcp_servers().clone()))
            .collect();

        // When reconciling every live session (diff-based, so a session
        // whose enablement is unchanged under the new config is a no-op).
        for (session_id, enabled) in &desired_by_session {
            self.reconcile(session_id, enabled).await;
        }

        // Then restart the running servers whose entry changed, and
        // summarize on the dashboard row.
        let configs = configured_servers(&self.deps.services);
        let stale: Vec<(String, SessionId)> = {
            let hashes = self.entry_hashes.lock();
            let per_session = self.spawned.lock();
            let singleton = self.singleton_spawned.lock();
            let mut stale = Vec::new();
            for (session_id, server) in per_session.keys() {
                let Some(config) = configs.iter().find(|(n, _)| n == server) else {
                    // Deleted entry: the connection stays until session
                    // end. The hash row stays too — it describes what the
                    // running process was started with, so an identical
                    // re-add later is still not an edit.
                    continue;
                };
                let recorded = hashes.get(server);
                let fresh = Self::entry_hash(&config.1);
                if recorded != Some(&fresh) {
                    stale.push((server.clone(), session_id.clone()));
                }
            }
            for server in singleton.keys() {
                let Some(config) = configs.iter().find(|(n, _)| n == server) else {
                    // Deleted singleton entry: same stance as per-session.
                    continue;
                };
                let recorded = hashes.get(server);
                let fresh = Self::entry_hash(&config.1);
                if recorded != Some(&fresh) && !stale.iter().any(|(s, _)| s == server) {
                    // One synthetic row stands in for the shared actor's
                    // single restart (the session id is never used for a
                    // singleton — the restart routes to the singleton
                    // namespace).
                    stale.push((server.clone(), SessionId::new()));
                }
            }
            stale
        };
        if stale.is_empty() {
            return;
        }

        let mut restarted: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for (server, session_id) in &stale {
            match self.restart_one(session_id, server).await {
                Ok(()) => {
                    restarted.push(server.clone());
                    // `restart_one` respawned through `spawn_one`/
                    // `spawn_singleton_if_absent`, which re-recorded the
                    // fresh hash. Drop a stale row only if the respawn's
                    // own record missed (defensive; the guards make it
                    // unreachable in practice).
                    let configs_now = configured_servers(&self.deps.services);
                    if let Some((_, config)) = configs_now.iter().find(|(n, _)| n == server) {
                        self.record_entry_hash(server, config);
                    } else {
                        self.forget_entry_hash(server);
                    }
                }
                Err(error) => {
                    failed.push(server.clone());
                    tracing::warn!(
                        server = %server,
                        error = ?error,
                        "MCP lifecycle: auto-restart after a config edit failed; \
                         leaving the respawned process in place, no retry"
                    );
                }
            }
        }

        let (note, tone) = if !failed.is_empty() {
            (
                format!(
                    "config reload: restarted {}; restart failed for {} (kept running)",
                    restarted.join(", "),
                    failed.join(", ")
                ),
                NoteTone::Error,
            )
        } else {
            (
                format!("config reload: restarted {}", restarted.join(", ")),
                NoteTone::Muted,
            )
        };
        self.publish(ServiceStatusUpdate {
            name: MCP_COORDINATOR_PATH.to_owned(),
            description: None,
            status_message: Some(note),
            note_tone: Some(tone),
        })
        .await;
    }
}

impl MsgHandler<SessionClosed> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &SessionClosed, _ctx: &mut MsgCtx<'_>) {
        self.kill_all_for_session(&msg.session_id).await;
        self.clear_runtime_for_session(&msg.session_id);
    }
}

impl MsgHandler<SessionArchived> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &SessionArchived, _ctx: &mut MsgCtx<'_>) {
        self.kill_all_for_session(&msg.session_id).await;
        self.clear_runtime_for_session(&msg.session_id);
    }
}

impl MsgHandler<SessionTeardownFinished> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &SessionTeardownFinished, _ctx: &mut MsgCtx<'_>) {
        self.kill_all_for_session(&msg.session_id).await;
        self.clear_runtime_for_session(&msg.session_id);
    }
}

impl MsgHandler<RestartMcpServer> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &RestartMcpServer, ctx: &mut MsgCtx<'_>) {
        let outcome = self.restart_one(&msg.session_id, &msg.server).await;
        ctx.reply(McpRestartOutcome {
            ok: outcome.is_ok(),
            error: outcome.err().map(|e| {
                match e {
                    RestartError::UnknownServer => "UnknownServer",
                    RestartError::ConnectFailed => "ConnectFailed",
                    RestartError::Timeout => "Timeout",
                    RestartError::Mailbox => "Mailbox",
                }
                .to_owned()
            }),
        });
    }
}

/// Wire payload for the restart ask's reply (a JSON-friendly
/// success/error pair).
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug, trouper::schema::Event)]
#[schema(description = "Reply payload for the restart ask.")]
pub struct McpRestartOutcome {
    pub ok: bool,
    pub error: Option<String>,
}

impl jinn_slices::BusMessage for McpRestartOutcome {}

#[cfg(test)]
/// Test-only message: restart with an injectable timeout so tests can
/// exercise the `Err(Timeout)` path without a 60s wait.
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug, trouper::schema::Command)]
#[schema(description = "Test-only restart ask with an injectable timeout.")]
pub struct McpRestartForTest {
    pub session_id: SessionId,
    pub server: String,
    pub timeout: std::time::Duration,
}

#[cfg(test)]
impl MsgHandler<McpRestartForTest> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &McpRestartForTest, ctx: &mut MsgCtx<'_>) {
        let outcome = self
            .restart_one_with_timeout(&msg.session_id, &msg.server, msg.timeout)
            .await;
        ctx.reply(McpRestartOutcome {
            ok: outcome.is_ok(),
            error: outcome.err().map(|e| {
                match e {
                    RestartError::UnknownServer => "UnknownServer",
                    RestartError::ConnectFailed => "ConnectFailed",
                    RestartError::Timeout => "Timeout",
                    RestartError::Mailbox => "Mailbox",
                }
                .to_owned()
            }),
        });
    }
}

/// Writes a `McpServerStatus` transition into the MCP runtime cell.
///
/// The coordinator is the sole writer of status and stderr projections. They
/// remain outside the durable session aggregate and snapshot.
///
/// Singleton-configured servers write the singleton bucket instead of the
/// spawning session's entry — the shared server has no owning session, and
/// the dispatch gate must resolve its status for whichever session called.
impl MsgHandler<McpServerStatus> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &McpServerStatus, _ctx: &mut MsgCtx<'_>) {
        let singleton = configured_servers(&self.deps.services)
            .into_iter()
            .any(|(name, config)| name == msg.server && config.singleton);
        self.runtime.update(|runtime| {
            if singleton {
                runtime.set_singleton_status(&msg.server, msg.status);
            } else {
                runtime.set_status(&msg.session_id, &msg.server, msg.status);
            }
        });
    }
}

/// Writes a captured stderr tail into the MCP runtime cell.
///
/// Singleton servers land in the singleton bucket (see [`McpServerStatus`]'s
/// handler for why).
impl MsgHandler<McpServerLog> for McpCoordinatorActor {
    async fn handle(&mut self, msg: &McpServerLog, _ctx: &mut MsgCtx<'_>) {
        let singleton = configured_servers(&self.deps.services)
            .into_iter()
            .any(|(name, config)| name == msg.server && config.singleton);
        self.runtime.update(|runtime| {
            if singleton {
                runtime.set_singleton_stderr(&msg.server, msg.tail.clone());
            } else {
                runtime.set_stderr(&msg.session_id, &msg.server, msg.tail.clone());
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

    use super::JinnTomlReloaded;
    use super::ServiceStatusUpdate;
    use std::collections::BTreeSet;

    use jinn_core_types::SessionId;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_mcp_msg::{McpConnectionStatus, McpServerStatus};
    use jinn_preferences_config::schemas::mcp::McpServerConfig;
    use jinn_preferences_config::schemas::mcp::McpServersConfig;
    use jinn_testutil::bus_harness::{TestHarness, await_recorded};

    use super::{McpCoordinatorActor, McpCoordinatorActorDeps};
    use jinn_mcp_msg::McpEnablementChanged;
    use jinn_session_msg::SessionClosed;
    use jinn_session_store_msg::SessionLoadCompleted;

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
        let (program, args) = jinn_testutil::platform::idle_command(60);
        McpServerConfig {
            command: Some(program.to_owned()),
            args,
            ..Default::default()
        }
    }

    async fn spawn_lifecycle(
        harness: &TestHarness,
        servers: &[(&str, McpServerConfig)],
    ) -> (
        trouper::actor::ActorPath,
        jinn_kernel::Services,
        jinn_kernel::common::state::State,
    ) {
        let services = harness.services().await;
        let mcp_server = servers
            .iter()
            .map(|(name, config)| ((*name).to_owned(), config.clone()))
            .collect();
        // Seeded through the layer the coordinator reads from, so the test
        // exercises the real resolution path.
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(mcp_server))
            .expect("seed the mcp.server section");
        let state = jinn_kernel::common::state::State::new(
            jinn_kernel::common::app_state::AppState::default(),
        );
        let runtime = crate::activate_runtime(&services.slices);
        let path = McpCoordinatorActor::spawn(
            &services.trouper_system,
            McpCoordinatorActorDeps {
                deps: ActorDeps {
                    services: services.clone(),
                },
                state: state.clone(),
                runtime,
            },
        )
        .await;
        (path, services, state)
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
        let (actor, services, _state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = SessionId::new();
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        // The coordinator has drained its inbox, so the enablement above has
        // been processed before the close arrives.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // When closing the session.
        harness.publish(SessionClosed { session_id }).await;

        // Then the lifecycle actor does not panic and the test completes.
        // The barrier is what makes "does not panic" observable: without it
        // the close may not have been processed at all, and a panic inside
        // the handler would not have happened yet.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;
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
        let (harness, recorder, session_id) = {
            let harness = TestHarness::new().await;
            let recorder = harness.spawn_recorder::<McpServerStatus>().await;
            let (_actor, _services, _state) =
                spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
            let session_id = SessionId::new();
            (harness, recorder, session_id)
        };

        // When enabling the server (spawn #1: Starting + Dead on failed connect).
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

        // And disabling it (must remove the map entry via kill_one).
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: BTreeSet::new(),
            })
            .await;
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

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
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

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
    ///
    /// `#[timeout]` is load-bearing: the `sleep 60` handshake never
    /// completes, so any unbounded await in the restart path would wedge
    /// the whole test binary without it.
    #[rstest::rstest]
    #[timeout(std::time::Duration::from_secs(20))]
    #[tokio::test]
    async fn restart_one_times_out_when_startup_exceeds_the_timeout() {
        // Given a coordinator with a server that hangs forever on the MCP handshake.
        let harness = TestHarness::new().await;
        let (actor, services, _state) =
            spawn_lifecycle(&harness, &[("hanging", hanging_server())]).await;
        let session_id = SessionId::new();

        // When restarting with a 1ms timeout (a direct system ask — the
        // seam route does not expose the injectable-timeout variant).
        let reply = services
            .trouper_system
            .ask(
                actor,
                super::McpRestartForTest {
                    session_id,
                    server: "hanging".to_owned(),
                    timeout: std::time::Duration::from_millis(1),
                },
                std::time::Duration::from_secs(10),
            )
            .await;
        // The handler replies with RestartOutcome even on failure.
        let timed_out: bool = match &reply {
            Ok(value) => {
                let outcome: super::McpRestartOutcome = value.clone().decode().unwrap();
                !outcome.ok && outcome.error.as_deref() == Some("Timeout")
            }
            Err(_) => false,
        };

        // Then it returns Timeout (startup couldn't complete in 1ms).
        assert!(
            timed_out,
            "startup exceeding the timeout should yield Timeout; got: {reply:?}"
        );
    }

    /// Inserts a fresh session carrying `enabled` into the harness's shared
    /// state and returns its id.
    fn insert_session_with_enablement(
        state: &jinn_kernel::common::state::State,
        enabled: &BTreeSet<String>,
    ) -> SessionId {
        let mut session = jinn_session_state::ChatSessionState::new();
        session.set_enabled_mcp_servers(enabled.clone());
        let session_id = session.session_id().clone();
        let mut app_state = state.write();
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
        let dir = jinn_testutil::tempdir();

        // When publishing SessionCreated for that session.
        harness
            .publish(jinn_session_lifecycle_msg::SessionCreated {
                session_id,
                cwd: dir.path.clone(),
            })
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
    async fn session_load_completed_spawns_actors_for_loaded_enablement() {
        // Given a coordinator with one configured server and a loaded session
        // whose enabled set contains it (the SessionLoadCompleted reconcile
        // path — a session restored from disk).
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let (_actor, _services, state) =
            spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
        let session_id = insert_session_with_enablement(&state, &single_enabled("unrunnable"));

        // When publishing SessionLoadCompleted for that session.
        harness
            .publish(SessionLoadCompleted {
                session_id: session_id.clone(),
            })
            .await;

        // Then an McpActor was spawned for the loaded server (a Starting
        // status arrives; Dead follows from the unrunnable command).
        let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(3)).await;
        assert!(
            !events.is_empty(),
            "SessionLoadCompleted must reconcile against the loaded session's enabled set"
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
        let dir = jinn_testutil::tempdir();

        // When both SessionCreated and McpEnablementChanged carry the same
        // desired set (the common seeding flow emits both).
        harness
            .publish(jinn_session_lifecycle_msg::SessionCreated {
                session_id: session_id.clone(),
                cwd: dir.path.clone(),
            })
            .await;
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

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
        let (harness, recorder, state, dir) = {
            let harness = TestHarness::new().await;
            let recorder = harness.spawn_recorder::<McpServerStatus>().await;
            let (_actor, _services, state) =
                spawn_lifecycle(&harness, &[("unrunnable", unrunnable_server())]).await;
            let dir = jinn_testutil::tempdir();
            (harness, recorder, state, dir)
        };
        let session_id = {
            let sid = insert_session_with_enablement(&state, &single_enabled("unrunnable"));
            harness
                .publish(McpEnablementChanged {
                    session_id: sid.clone(),
                    enabled: single_enabled("unrunnable"),
                })
                .await;
            // The spawn counter is the assertion later in this test, so
            // wait for the status stream to quiesce: the count must have
            // stopped changing. A fixed sleep would pass on a slow host
            // simply because the expected spawn had not happened yet.
            jinn_testutil::wait::await_settled(
                &recorder,
                std::time::Duration::from_millis(150),
                std::time::Duration::from_secs(10),
            )
            .await;
            sid
        };

        // When the session's enablement shrinks to empty and SessionCreated
        // reconciles against the shrunken set.
        {
            let mut app_state = state.write();
            if let Some(s) = app_state.session.get_mut(&session_id) {
                s.set_enabled_mcp_servers(BTreeSet::new());
            }
        }
        harness
            .publish(jinn_session_lifecycle_msg::SessionCreated {
                session_id: session_id.clone(),
                cwd: dir.path.clone(),
            })
            .await;
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

        // And the server is enabled again.
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled("unrunnable"),
            })
            .await;
        // The spawn counter is the assertion here, so wait for the status
        // stream to quiesce: the count must have stopped changing. A fixed
        // sleep would pass on a slow host simply because the expected spawn
        // had not happened yet, which is the opposite of what the test
        // claims.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;

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

    // ── The reload auto-restart (entry-hash diff) ────────────────────────

    /// Spawns the coordinator and enables `server` for a fresh session so
    /// its entry hash is seeded by the spawn. Returns the harness, the
    /// services (for config writes and inbox drains), the state, the
    /// session id, the actor path, and the two pre-spawned recorders —
    /// a `McpServerStatus` tap and a `ServiceStatusUpdate` tap, both live
    /// before any publish so the baseline spawn and any reload note land
    /// in them.
    #[allow(clippy::type_complexity, reason = "test fixture tuple")]
    async fn spawned_server_fixture(
        servers: &[(&str, McpServerConfig)],
        server: &str,
    ) -> (
        TestHarness,
        jinn_kernel::Services,
        jinn_kernel::common::state::State,
        SessionId,
        trouper::actor::ActorPath,
        jinn_testutil::bus_harness::Recorder<McpServerStatus>,
        jinn_testutil::bus_harness::Recorder<ServiceStatusUpdate>,
    ) {
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<McpServerStatus>().await;
        let notes = harness.spawn_recorder::<ServiceStatusUpdate>().await;
        let (actor, services, state) = spawn_lifecycle(&harness, servers).await;
        let session_id = insert_session_with_enablement(&state, &single_enabled(server));
        harness
            .publish(McpEnablementChanged {
                session_id: session_id.clone(),
                enabled: single_enabled(server),
            })
            .await;
        // The status stream quiescing proves the baseline spawn landed (its
        // hash seeded) before the reload under test arrives.
        jinn_testutil::wait::await_settled(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;
        (harness, services, state, session_id, actor, recorder, notes)
    }

    /// An unrunnable server whose entry differs from
    /// [`unrunnable_server`] only in its args — an entry *edit* by hash.
    fn edited_unrunnable_server() -> McpServerConfig {
        McpServerConfig {
            command: Some("/this/command/does/not/exist".to_owned()),
            args: vec!["--changed".to_owned()],
            ..Default::default()
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_restarts_a_running_server_whose_entry_changed() {
        // Given a running actor for an enabled server, its entry hash seeded
        // (a first Starting status from the baseline spawn, in the recorder).
        let (harness, services, _state, _session_id, actor, recorder, _notes) =
            spawned_server_fixture(&[("unrunnable", unrunnable_server())], "unrunnable").await;
        let baseline = recorder.drain();
        let baseline_starts = baseline
            .iter()
            .filter(|event| event.status == McpConnectionStatus::Starting)
            .count();
        assert!(
            baseline_starts >= 1,
            "the baseline spawn must have published a Starting status, got {baseline:?}"
        );

        // When the config edit changes that server's entry and the reload lands.
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(
                [("unrunnable".to_owned(), edited_unrunnable_server())].into(),
            ))
            .expect("write the edited entry");
        harness.publish(JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then a second spawn happened: the hash diff caught the edit and
        // restart_one ran — the respawned actor publishes its own Starting.
        // The baseline was drained above, so any Starting in this batch
        // arrived after the reload.
        let all = jinn_testutil::wait::await_settled_messages(
            &recorder,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;
        let starts = all
            .iter()
            .filter(|event| event.status == McpConnectionStatus::Starting)
            .count();
        assert!(
            starts >= 1,
            "the reload must have respawned the edited server; \
             expected a fresh Starting after the baseline drain, got {all:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_auto_restart_notes_once_with_the_error_tone() {
        // Given a running actor for the unrunnable server: the restart's
        // connect probe fails fast (the command does not exist), so the
        // auto-restart below takes the failure path.
        let (harness, services, _state, _session_id, actor, _recorder, notes) =
            spawned_server_fixture(&[("unrunnable", unrunnable_server())], "unrunnable").await;

        // When the entry is edited and the reload lands.
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(
                [("unrunnable".to_owned(), edited_unrunnable_server())].into(),
            ))
            .expect("write the edited entry");
        harness.publish(JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then the failure surfaced exactly once, Error-toned, naming the
        // server and saying the process was kept running.
        jinn_testutil::wait::await_settled(
            &notes,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(10),
        )
        .await;
        let all = notes.drain();
        let failures: Vec<&ServiceStatusUpdate> = all
            .iter()
            .filter(|note| {
                note.status_message
                    .as_deref()
                    .is_some_and(|m| m.contains("restart failed"))
            })
            .collect();
        assert_eq!(
            failures.len(),
            1,
            "a failed auto-restart notes exactly once; got {all:?}"
        );
        let note = failures[0];
        assert_eq!(
            note.note_tone,
            Some(jinn_slices::NoteTone::Error),
            "the failure note is Error-toned"
        );
        assert!(
            note.status_message
                .as_deref()
                .is_some_and(|m| m.contains("unrunnable") && m.contains("kept running")),
            "the note names the server and says it kept running: {:?}",
            note.status_message
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_does_not_restart_an_unchanged_server() {
        // Given a running actor for an enabled server, its entry hash seeded.
        let (harness, services, _state, _session_id, actor, _recorder, notes) =
            spawned_server_fixture(&[("unrunnable", unrunnable_server())], "unrunnable").await;

        // When a reload lands with the config byte-identical for that entry.
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(
                [("unrunnable".to_owned(), unrunnable_server())].into(),
            ))
            .expect("rewrite the same entry");
        harness.publish(JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then nothing is noted: the hash short-circuit held, and the
        // absence is observed over a settled window, not a race.
        jinn_testutil::wait::await_settled(
            &notes,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            notes.is_empty(),
            "an unchanged entry must not restart or note, got {:?}",
            notes.drain()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_leaves_a_running_server_whose_entry_was_deleted() {
        // Given a running actor for an enabled server, its entry hash seeded.
        let (harness, services, _state, _session_id, actor, _recorder, notes) =
            spawned_server_fixture(&[("unrunnable", unrunnable_server())], "unrunnable").await;

        // When the entry is deleted from the config and the reload lands.
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(Default::default()))
            .expect("delete the entry");
        harness.publish(JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then the connection is untouched: no restart note over a settled
        // window.
        jinn_testutil::wait::await_settled(
            &notes,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            notes.is_empty(),
            "a deleted entry must not kill the running connection, got {:?}",
            notes.drain()
        );

        // And the deleted-entry hash row was kept: re-adding the identical
        // entry and reloading must not count it as an edit (the row
        // describes what the running process was started with, and that
        // process never stopped).
        services
            .config
            .put::<McpServersConfig>(&McpServersConfig(
                [("unrunnable".to_owned(), unrunnable_server())].into(),
            ))
            .expect("re-add the entry");
        harness.publish(JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;
        jinn_testutil::wait::await_settled(
            &notes,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            notes.is_empty(),
            "a re-added identical entry must not count as an edit, got {:?}",
            notes.drain()
        );
    }
}

#[cfg(test)]
mod status_tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_core_types::SessionId;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_kernel::common::state::State;
    use jinn_mcp_msg::{McpConnectionStatus, McpRuntimeState, McpServerLog, McpServerStatus};
    use jinn_session_lifecycle_msg::SessionTeardownFinished;
    use jinn_session_msg::{SessionArchived, SessionClosed};
    use jinn_session_store_msg::SessionLoadCompleted;
    use jinn_slices::TypedCell;
    use jinn_testutil::bus_harness::TestHarness;

    use super::McpCoordinatorActor;
    use crate::coordinator::McpCoordinatorActorDeps;

    /// Spawns a coordinator with a dedicated MCP runtime cell.
    /// Spawns a coordinator with a seeded session, handing back the runtime
    /// cell, the session id, the actor path, and the services — the path and
    /// services so a test can drain the coordinator's inbox before asserting
    /// an absence.
    async fn spawn_with_session(
        harness: &TestHarness,
    ) -> (
        TypedCell<McpRuntimeState>,
        SessionId,
        trouper::actor::ActorPath,
        jinn_kernel::Services,
    ) {
        let services = harness.services().await;
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let runtime = crate::activate_runtime(&services.slices);
        let path = McpCoordinatorActor::spawn(
            &services.trouper_system,
            McpCoordinatorActorDeps {
                deps: ActorDeps {
                    services: services.clone(),
                },
                state,
                runtime: runtime.clone(),
            },
        )
        .await;
        (runtime, session_id, path, services)
    }

    fn status_of(
        runtime: &TypedCell<McpRuntimeState>,
        sid: &SessionId,
        server: &str,
    ) -> Option<McpConnectionStatus> {
        runtime.read().status(sid, server)
    }

    fn tail_of(
        runtime: &TypedCell<McpRuntimeState>,
        sid: &SessionId,
        server: &str,
    ) -> Option<String> {
        runtime.read().stderr(sid, server).map(str::to_owned)
    }

    fn seed_runtime(runtime: &TypedCell<McpRuntimeState>, session_id: &SessionId) {
        runtime.update(|runtime| {
            runtime.set_status(session_id, "excalimate", McpConnectionStatus::Running);
            runtime.set_stderr(session_id, "excalimate", "stale".to_owned());
        });
    }

    fn assert_runtime_is_empty(runtime: &TypedCell<McpRuntimeState>, session_id: &SessionId) {
        let runtime = runtime.read();
        assert_eq!(runtime.status(session_id, "excalimate"), None);
        assert_eq!(runtime.stderr(session_id, "excalimate"), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_closed_clears_runtime_data() {
        // Given stale runtime data for a session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, actor, services) = spawn_with_session(&harness).await;
        seed_runtime(&runtime, &session_id);

        // When the session closes.
        harness
            .publish(SessionClosed {
                session_id: session_id.clone(),
            })
            .await;
        // The coordinator has drained its inbox, so the event above has been
        // *processed*. Without this barrier the absence assertion below
        // could run before the coordinator was even scheduled, and would
        // pass whether or not the cleanup worked.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then its runtime status and stderr are absent.
        assert_runtime_is_empty(&runtime, &session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_archived_clears_runtime_data() {
        // Given stale runtime data for a session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, actor, services) = spawn_with_session(&harness).await;
        seed_runtime(&runtime, &session_id);

        // When the session is archived.
        harness
            .publish(SessionArchived {
                session_id: session_id.clone(),
            })
            .await;
        // The coordinator has drained its inbox, so the event above has been
        // *processed*. Without this barrier the absence assertion below
        // could run before the coordinator was even scheduled, and would
        // pass whether or not the cleanup worked.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then its runtime status and stderr are absent.
        assert_runtime_is_empty(&runtime, &session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_teardown_finished_clears_runtime_data() {
        // Given stale runtime data for a session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, actor, services) = spawn_with_session(&harness).await;
        seed_runtime(&runtime, &session_id);

        // When lifecycle teardown finishes.
        harness
            .publish(SessionTeardownFinished {
                session_id: session_id.clone(),
                error: None,
            })
            .await;
        // The coordinator has drained its inbox, so the event above has been
        // *processed*. Without this barrier the absence assertion below
        // could run before the coordinator was even scheduled, and would
        // pass whether or not the cleanup worked.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then its runtime status and stderr are absent.
        assert_runtime_is_empty(&runtime, &session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_load_completed_clears_stale_runtime_data() {
        // Given stale runtime data for a session being initialized.
        let harness = TestHarness::new().await;
        let (runtime, session_id, actor, services) = spawn_with_session(&harness).await;
        seed_runtime(&runtime, &session_id);

        // When load completion is published.
        harness
            .publish(SessionLoadCompleted {
                session_id: session_id.clone(),
            })
            .await;
        // The coordinator has drained its inbox, so the event above has been
        // *processed*. Without this barrier the absence assertion below
        // could run before the coordinator was even scheduled, and would
        // pass whether or not the cleanup worked.
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &actor,
            std::time::Duration::from_secs(10),
        )
        .await;

        // Then only the newly initialized session has no stale runtime data.
        assert_runtime_is_empty(&runtime, &session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dead_status_is_written_to_runtime_cell() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, _actor, _services) = spawn_with_session(&harness).await;

        // When publishing a Dead status for one server.
        harness
            .publish(McpServerStatus {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Dead,
            })
            .await;

        jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
            state.status(&session_id, "excalimate") == Some(McpConnectionStatus::Dead)
        })
        .await;

        // Then the runtime cell status shows Dead.
        assert_eq!(
            status_of(&runtime, &session_id, "excalimate"),
            Some(McpConnectionStatus::Dead)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn running_status_is_written_to_runtime_cell() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, _actor, _services) = spawn_with_session(&harness).await;

        // When publishing a Running status for one server.
        harness
            .publish(McpServerStatus {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Running,
            })
            .await;

        jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
            state.status(&session_id, "excalimate") == Some(McpConnectionStatus::Running)
        })
        .await;

        // Then the runtime cell status shows Running.
        assert_eq!(
            status_of(&runtime, &session_id, "excalimate"),
            Some(McpConnectionStatus::Running)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn status_for_one_session_is_not_visible_in_another() {
        // Given a coordinator with two seeded sessions.
        let harness = TestHarness::new().await;
        let (runtime, session_a, _actor, _services) = spawn_with_session(&harness).await;
        let session_b = SessionId::new();

        // When publishing a Running status for session A only.
        harness
            .publish(McpServerStatus {
                session_id: session_a.clone(),
                server: "excalimate".to_owned(),
                status: McpConnectionStatus::Running,
            })
            .await;

        jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
            state.status(&session_a, "excalimate") == Some(McpConnectionStatus::Running)
        })
        .await;

        // Then session A shows Running, but session B has no status for it.
        assert_eq!(
            status_of(&runtime, &session_a, "excalimate"),
            Some(McpConnectionStatus::Running)
        );
        assert_eq!(status_of(&runtime, &session_b, "excalimate"), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stderr_tail_is_written_to_runtime_cell() {
        // Given a coordinator with a seeded session.
        let harness = TestHarness::new().await;
        let (runtime, session_id, _actor, _services) = spawn_with_session(&harness).await;

        // When publishing a stderr tail for one server.
        harness
            .publish(McpServerLog {
                session_id: session_id.clone(),
                server: "excalimate".to_owned(),
                tail: "npm warn something".to_owned(),
            })
            .await;

        jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
            state.stderr(&session_id, "excalimate") == Some("npm warn something")
        })
        .await;

        // Then the runtime cell stderr shows the latest tail.
        assert_eq!(
            tail_of(&runtime, &session_id, "excalimate"),
            Some("npm warn something".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stderr_tail_for_one_session_is_not_visible_in_another() {
        // Given a coordinator with two seeded sessions.
        let harness = TestHarness::new().await;
        let (runtime, session_a, _actor, _services) = spawn_with_session(&harness).await;
        let session_b = SessionId::new();

        // When publishing a stderr tail for session A only.
        harness
            .publish(McpServerLog {
                session_id: session_a.clone(),
                server: "excalimate".to_owned(),
                tail: "only-in-a".to_owned(),
            })
            .await;

        jinn_testutil::wait::await_cell(&runtime, std::time::Duration::from_secs(10), |state| {
            state.stderr(&session_a, "excalimate") == Some("only-in-a")
        })
        .await;

        // Then session A shows the tail, but session B has none.
        assert_eq!(
            tail_of(&runtime, &session_a, "excalimate"),
            Some("only-in-a".to_owned())
        );
        assert_eq!(tail_of(&runtime, &session_b, "excalimate"), None);
    }
}
