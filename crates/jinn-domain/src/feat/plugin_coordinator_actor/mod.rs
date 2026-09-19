//! Plugin coordinator — spawns plugin actors, owns the trust boundary and
//! the contribution cache.
//!
//! One instance lives for the whole app. At `on_start` it reads the enabled
//! `[[plugin]]` entries from `jinn.toml` and spawns one
//! [`PluginActor`](crate::feat::plugin_actor::PluginActor) per entry
//! (supervised, `RestartPolicy::Never` — a dead plugin stays dead until
//! the next app start).
//!
//! Every inbound plugin message flows through the coordinator's private
//! channel ([`PluginInbound`]). The coordinator validates and authorizes:
//! unknown variants and malformed payloads are dropped, unrequested
//! commands are ignored. Accepted contributions are validated and
//! forwarded onto the bus; citation contributions flow to the session
//! actor's consumer.
//!
//! `PluginStatus` events published by the plugin actors are also received
//! here (bus subscription) and mirrored into the state cache so the UI can
//! read plugin health synchronously.
//!
//! A dead plugin means a stale cache, never a blocked or failed consumer:
//! the theme picker falls back to the built-in default theme when the
//! cache is empty (see the theme extraction phase).

use std::collections::HashMap;

use kameo::actor::{ActorRef, Spawn};
use kameo::prelude::{Context, Message};
use kameo::supervision::RestartPolicy;
use parking_lot::Mutex;
use tokio::sync::mpsc;

pub mod protocol;

#[cfg(test)]
mod tests;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::root_supervisor::RootSupervisorRef;
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::feat::plugin::PluginConfig;
use crate::feat::plugin_actor::{DeliverHostEvent, PluginActor, PluginActorDeps, PluginInbound};
use crate::feat::plugin_coordinator_actor::protocol::{
    PluginPhase, PluginStatus, PluginSubscriptions, Tick,
};
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::retry_stalled_session::RetryStalledSession;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use jinn_inference_msg::SendToLlmProvider;
use jinn_inference_msg::{StreamCompleted, StreamCompletedReason, StreamToken};
use jinn_tools_msg::{ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted, ToolUseStarted};

/// Wall-clock pulse interval pushed to guests subscribed to `"tick"`.
///
/// Bounds the guest-side stall-detection granularity: any time-based guest
/// logic (e.g. the stall-watchdog plugin's restart timeout) fires within
/// this interval of its threshold.
const GUEST_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Channel capacity for plugin→coordinator inbound events. Small: events are
/// rare (handshake + contributions); a full channel means a flooding plugin,
/// and senders should feel backpressure.
const INBOUND_CAPACITY: usize = 64;

/// The plugin coordinator actor.
pub struct PluginCoordinatorActor {
    deps: ActorDeps,
    root: RootSupervisorRef,
    state: State,
    cap: crate::common::tcaps::PluginsCap,
    /// Config dir / data dir context for grant resolution and wasm paths.
    dirs: PluginDirs,
    /// Live plugin actors by name.
    spawned: Mutex<HashMap<String, ActorRef<PluginActor>>>,
    /// Validated event-subscription kinds per running plugin (from each
    /// guest's `Hello`). Drives the host→guest event forwarder.
    subscriptions: Mutex<HashMap<String, Vec<String>>>,
    /// Test seam: when set, spawned plugin actors use a scripted fake
    /// guest instead of a real wasm module (see
    /// [`jinn_plugin::FakeGuestScript`]). Shared through deps so tests
    /// can arm it before spawning the coordinator.
    #[cfg(test)]
    fake_guest: std::sync::Arc<std::sync::Mutex<Option<jinn_plugin::FakeGuestScript>>>,
}

/// Directory context the coordinator resolves plugin paths against.
#[derive(Debug, Clone)]
pub struct PluginDirs {
    /// User config dir (e.g. `~/.config/jinn`).
    pub config_dir: std::path::PathBuf,
    /// User data dir (e.g. `~/.local/share/jinn`).
    pub data_dir: std::path::PathBuf,
    /// The shared wasmtime engine, reused across all plugin guests.
    pub engine: std::sync::Arc<jinn_plugin::PluginEngine>,
}

/// Dependencies for [`PluginCoordinatorActor`].
#[derive(Clone)]
pub struct PluginCoordinatorActorDeps {
    /// Common actor dependencies (services + bus).
    pub deps: ActorDeps,
    /// Test seam: scripted fake guest for spawned plugin actors.
    /// Production constructs this as `None`.
    #[cfg(test)]
    pub fake_guest: std::sync::Arc<std::sync::Mutex<Option<jinn_plugin::FakeGuestScript>>>,
    /// Root supervisor — plugin actors are supervised children of it.
    pub root: RootSupervisorRef,
    /// Shared application state — the contribution cache lives here.
    pub state: State,
    /// Authority to write the plugin contribution cache.
    pub cap: crate::common::tcaps::PluginsCap,
    /// Directory context for grant resolution and wasm paths.
    pub dirs: PluginDirs,
    /// Test seam: overrides [`GUEST_TICK_INTERVAL`] so tick forwarding is
    /// observable in tests without real-time waits. Production passes
    /// `None`.
    pub tick_override: Option<std::time::Duration>,
}

impl kameo::Actor for PluginCoordinatorActor {
    type Args = PluginCoordinatorActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .services
            .bus
            .subscribe::<PluginStatus, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<PluginSubscriptions, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<ToolCallReceived, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<ToolExecutionCompleted, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<SessionPhaseChanged, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<SendToLlmProvider, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<StreamToken, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<ToolUseStarted, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<ToolCallStreaming, _>(&actor_ref)
            .await;
        args.deps
            .services
            .bus
            .subscribe::<StreamCompleted, _>(&actor_ref)
            .await;

        // Push a periodic tick to guests subscribed to `"tick"`: guests are
        // blocking stdin readers and only wake when the host writes a line,
        // so without this pulse they could never act on elapsed time. The
        // task holds a clone of the actor ref and dies with it; the first
        // (immediate) tick is skipped.
        let timer_ref = actor_ref.clone();
        tokio::spawn(async move {
            let interval = args.tick_override.unwrap_or(GUEST_TICK_INTERVAL);
            let mut interval = tokio::time::interval(interval);
            interval.tick().await;
            loop {
                interval.tick().await;
                let _ = timer_ref.tell(Tick).await;
            }
        });

        let actor = Self {
            deps: args.deps,
            root: args.root,
            state: args.state,
            cap: args.cap,
            dirs: args.dirs,
            spawned: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::new()),
            #[cfg(test)]
            fake_guest: args.fake_guest.clone(),
        };

        // Spawn every enabled plugin from jinn.toml.
        actor.spawn_all().await;

        Ok(actor)
    }
}

impl BusPublish for PluginCoordinatorActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl PluginCoordinatorActor {
    /// Spawns a plugin actor for every enabled `[plugin.<name>]` entry.
    async fn spawn_all(&self) {
        let configs = enabled_plugins(&self.deps.services);
        match configs.len() {
            0 => tracing::info!("plugin coordinator: no plugins configured"),
            n => tracing::info!(
                count = n,
                names = %configs
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                "plugin coordinator: loading plugins"
            ),
        }
        for (name, config) in configs {
            self.spawn_one(&name, &config).await;
        }
    }

    /// Spawns one plugin actor (idempotent per name) with its inbound
    /// channel and grant set resolved from the manifest entry.
    async fn spawn_one(&self, name: &str, config: &PluginConfig) {
        if self.spawned.lock().contains_key(name) {
            return;
        }

        let (inbound_tx, mut inbound_rx) = mpsc::channel::<PluginInbound>(INBOUND_CAPACITY);

        let grants = match self.resolve_grants(name, config) {
            Ok(grants) => grants,
            Err(report) => {
                tracing::warn!(plugin = %name, "{report:#}");
                self.deps
                    .publish(PluginStatus {
                        name: name.to_owned(),
                        phase: PluginPhase::Dead,
                    })
                    .await;
                return;
            }
        };

        let wasm_path = self.wasm_path(config);
        // Snapshot grant facts for logging; `grants` itself moves into the
        // actor deps below.
        let (read_dirs, write_dirs, http) =
            (grants.read_dirs.len(), grants.write_dirs.len(), grants.http);
        let actor_ref = PluginActor::supervise(
            &self.root,
            PluginActorDeps {
                deps: self.deps.clone(),
                name: name.to_owned(),
                config: config.clone(),
                grants,
                wasm_path,
                engine: self.dirs.engine.clone(),
                inbound_tx: inbound_tx.clone(),
                #[cfg(test)]
                fake_guest: self.fake_guest.clone(),
            },
        )
        .restart_policy(RestartPolicy::Never)
        .spawn()
        .await;
        self.spawned.lock().insert(name.to_owned(), actor_ref);
        tracing::info!(
            plugin = %name,
            read_grants = read_dirs,
            write_grants = write_dirs,
            http,
            "plugin coordinator: spawned PluginActor"
        );

        // The coordinator's inbound pump: every message from this plugin is
        // validated here before it may touch state.
        let bus = self.deps.services.bus.clone();
        let pump_name = name.to_owned();
        tokio::spawn(async move {
            while let Some(inbound) = inbound_rx.recv().await {
                handle_inbound(&bus, &pump_name, inbound).await;
            }
        });
    }

    /// Resolves manifest grants into runner grants against the dir context.
    fn resolve_grants(
        &self,
        name: &str,
        config: &PluginConfig,
    ) -> Result<jinn_plugin::Grants, error_stack::Report<jinn_plugin::GrantsError>> {
        let ctx = jinn_plugin::DirContext {
            config_dir: self.dirs.config_dir.clone(),
            data_dir: self.dirs.data_dir.clone(),
            plugin_name: name.to_owned(),
        };
        let path_grants: Vec<jinn_plugin::PathGrant> = config
            .grants
            .iter()
            .map(|g| jinn_plugin::PathGrant {
                path: g.path.clone(),
                writable: g.writable,
            })
            .collect();
        let config_value = config.config.clone().map_or(serde_json::Value::Null, |v| {
            serde_json::to_value(&v).unwrap_or(serde_json::Value::Null)
        });
        jinn_plugin::resolve_grants(&path_grants, config.http, config_value, &ctx)
    }

    /// Resolves a plugin's wasm path: absolute, or relative to the user
    /// plugins dir (`<data_dir>/plugins/`) where `plugin install` places
    /// payloads.
    fn wasm_path(&self, config: &PluginConfig) -> std::path::PathBuf {
        let candidate = std::path::PathBuf::from(&config.wasm);
        if candidate.is_absolute() {
            return candidate;
        }
        self.dirs.data_dir.join("plugins").join(candidate)
    }
}

/// Reads the enabled plugin entries from user preferences, keyed by name.
fn enabled_plugins(services: &crate::Services) -> Vec<(String, PluginConfig)> {
    services
        .user_preferences_storage
        .read()
        .plugin
        .iter()
        .filter(|(_, config)| config.enabled)
        .map(|(name, config)| (name.clone(), config.clone()))
        .collect()
}

/// Validates and applies one inbound plugin message.
///
/// The trust boundary: nothing from a plugin reaches `AppState` except
/// through this function's match. Unknown variants are silently dropped
/// payloads (forward compatibility). `Hello` outside the handshake is
/// ignored.
async fn handle_inbound(bus: &BusService, name: &str, inbound: PluginInbound) {
    match inbound.event {
        jinn_plugin_api::PluginToHost::Hello(_) => {
            // Handshake was already completed by the actor; a second Hello
            // is protocol noise — ignore it.
        }
        jinn_plugin_api::PluginToHost::PushCitations(entries) => {
            // Turn-scoped contribution — no identical-payload debounce: two
            // identical search turns are different turns and both must land.
            let Some(session_id) = crate::protocol::SessionId::try_from_string(&entries.session_id)
            else {
                tracing::warn!(
                    plugin = %name,
                    session_id = %entries.session_id,
                    "plugin citations dropped: unparseable session id"
                );
                return;
            };
            let citations: Vec<jinn_provider::UrlCitation> = entries
                .citations
                .iter()
                .filter_map(|citation| validate_citation(citation, name))
                .collect();
            if citations.is_empty() {
                return;
            }
            tracing::info!(
                plugin = %name,
                session_id = %session_id,
                count = citations.len(),
                "plugin contributed citations"
            );
            bus.publish(
                crate::feat::session::protocol::citations_received::CitationsReceived {
                    session_id,
                    citations,
                },
            )
            .await;
        }
        jinn_plugin_api::PluginToHost::CancelStream(msg) => {
            apply_plugin_cancel_stream(bus, name, msg).await;
        }
        jinn_plugin_api::PluginToHost::InsertSystemEntry(msg) => {
            apply_plugin_insert_system_entry(bus, name, msg).await;
        }
        jinn_plugin_api::PluginToHost::RestartStalledStream(msg) => {
            apply_plugin_restart_stalled_stream(bus, name, msg).await;
        }
    }
}

/// Applies a mirrored plugin cancel request: validates the session id and
/// publishes the internal provider `CancelStream` command.
///
/// A cancel for an idle session is a host-side no-op (the provider actor
/// drops it), so duplicate sends are harmless — no dedup or debounce.
async fn apply_plugin_cancel_stream(
    bus: &BusService,
    plugin: &str,
    msg: jinn_plugin_api::CancelStream,
) {
    let Some(session_id) = crate::protocol::SessionId::try_from_string(&msg.session_id) else {
        tracing::warn!(
            plugin = %plugin,
            session_id = %msg.session_id,
            "plugin cancel_stream dropped: unparseable session id"
        );
        return;
    };
    tracing::info!(
        plugin = %plugin,
        session_id = %session_id,
        "plugin requested stream cancel"
    );
    bus.publish(jinn_inference_msg::CancelStream { session_id })
        .await;
}

/// Applies a mirrored plugin restart request: validates the session id and
/// publishes the internal session `RetryStalledSession` command.
///
/// The session actor's retry handler is the real guard — it restarts only
/// when the phase is active *and* an LLM stream is genuinely in flight, and
/// no-ops otherwise — so a stale or bogus request is harmless by
/// construction (mirrors the cancel path's "canceling an idle session is a
/// no-op" reasoning).
async fn apply_plugin_restart_stalled_stream(
    bus: &BusService,
    plugin: &str,
    msg: jinn_plugin_api::RestartStalledStream,
) {
    let Some(session_id) = crate::protocol::SessionId::try_from_string(&msg.session_id) else {
        tracing::warn!(
            plugin = %plugin,
            session_id = %msg.session_id,
            "plugin restart_stalled_stream dropped: unparseable session id"
        );
        return;
    };
    tracing::info!(
        plugin = %plugin,
        session_id = %session_id,
        "plugin requested stalled-stream restart"
    );
    bus.publish(RetryStalledSession {
        session_id,
        attempt: msg.attempt,
        max_restarts: msg.max_restarts,
    })
    .await;
}

/// Applies a mirrored plugin system entry: validates the session id and
/// pushes the entry directly via `PushChatEntry`.
async fn apply_plugin_insert_system_entry(
    bus: &BusService,
    plugin: &str,
    msg: jinn_plugin_api::InsertSystemEntry,
) {
    let Some(session_id) = crate::protocol::SessionId::try_from_string(&msg.session_id) else {
        tracing::warn!(
            plugin = %plugin,
            session_id = %msg.session_id,
            "plugin insert_system_entry dropped: unparseable session id"
        );
        return;
    };
    tracing::info!(
        plugin = %plugin,
        session_id = %session_id,
        "plugin contributed system entry"
    );
    // Pushed, not queued: `SubmitHistoryMutations` defers non-idle sessions
    // to the next stream completion, which would hide watchdog entries for
    // the entire stall they exist to report. `PushChatEntry` appends
    // immediately regardless of phase, so the marker lands while the stream
    // is still (stuck) in flight.
    bus.publish(jinn_session_history_msg::PushChatEntry {
        session_id,
        entry: crate::protocol::ChatEntry::system(msg.text),
    })
    .await;
}

/// Validates one plugin citation, applying the title fallback.
///
/// Invalid entries (non-http(s) URL, empty everything) are dropped with a
/// warn — a malformed guest payload never blocks the valid remainder.
fn validate_citation(
    citation: &jinn_plugin_api::PluginCitation,
    plugin: &str,
) -> Option<jinn_provider::UrlCitation> {
    let is_http = citation.url.starts_with("http://") || citation.url.starts_with("https://");
    if !is_http || citation.url.is_empty() {
        tracing::warn!(plugin = %plugin, url = %citation.url, "citation dropped: not an http(s) URL");
        return None;
    }
    let title = if citation.title.trim().is_empty() {
        citation.url.clone()
    } else {
        citation.title.clone()
    };
    Some(jinn_provider::UrlCitation {
        url: citation.url.clone(),
        title,
        content: citation.content.clone(),
        start_index: None,
        end_index: None,
    })
}

impl Message<PluginStatus> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: PluginStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let PluginStatus { name, phase } = msg;
        self.state
            .with_plugins(&self.cap, |p| p.set_phase(name.clone(), phase));
        // A terminal phase (dead or cleanly done) drops the spawned map
        // entry — the actor is stopping either way. The phase itself stays
        // in the cache: `Done` keeps the plugin's contributions readable,
        // `Dead` records the failure until the next app start.
        if matches!(phase, PluginPhase::Dead | PluginPhase::Done) {
            self.spawned.lock().remove(&name);
            self.subscriptions.lock().remove(&name);
        }
    }
}

impl Message<PluginSubscriptions> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: PluginSubscriptions,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let PluginSubscriptions { name, kinds } = msg;
        tracing::info!(
            plugin = %name,
            kinds = %kinds.join(", "),
            "plugin coordinator: subscriptions registered"
        );
        self.subscriptions.lock().insert(name, kinds);
    }
}

impl Message<ToolCallReceived> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ToolCallReceived,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let tool_call = msg.tool_call;
        let event = jinn_plugin_api::ToolCallEvent {
            session_id: msg.session_id.to_string(),
            tool_call_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
        };
        self.forward_event("tool_call", || {
            jinn_plugin_api::HostToPlugin::ToolCallEvent(event.clone())
        })
        .await;
        // An assembled tool call is also proof the stream is alive.
        stream_ping(self, &msg.session_id).await;
    }
}

impl Message<ToolExecutionCompleted> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ToolExecutionCompleted,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Plugins always receive the complete tool output — truncation is
        // an LLM-context protection, never a plugin-facing limit.
        let result = msg.result;
        let content = full_output_for_plugin(&result);
        let event = jinn_plugin_api::ToolResultEvent {
            session_id: msg.session_id.to_string(),
            tool_call_id: result.tool_call_id.clone(),
            name: result.name.clone(),
            content,
            success: result.success,
        };
        self.forward_event("tool_result", || {
            jinn_plugin_api::HostToPlugin::ToolResultEvent(event.clone())
        })
        .await;
        // A finished execution is also proof the turn is alive — a long
        // tool run must not read as stream silence to liveness watchers.
        stream_ping(self, &msg.session_id).await;
    }
}

impl Message<SessionPhaseChanged> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SessionPhaseChanged,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !(msg.old_phase == PhaseKind::Streaming && msg.new_phase == PhaseKind::Idle) {
            return;
        }
        let event = jinn_plugin_api::TurnEndEvent {
            session_id: msg.session_id.to_string(),
            final_answer: last_entry_is_assistant(&self.state, &msg.session_id),
        };
        self.forward_event("turn_end", || {
            jinn_plugin_api::HostToPlugin::TurnEndEvent(event.clone())
        })
        .await;
    }
}

/// Whether the session's last history entry is an assistant message — the
/// host-computed "the turn reached a genuine final answer" signal.
///
/// Error/cancel mid-turn leaves a non-assistant last entry; guests retain
/// their turn-scoped state for the next successful turn.
fn last_entry_is_assistant(state: &State, session_id: &crate::protocol::SessionId) -> bool {
    state
        .read()
        .try_session(session_id)
        .and_then(|session| session.history().last())
        .is_some_and(|entry| {
            matches!(
                entry.kind,
                crate::protocol::ChatEntryKind::Assistant(_)
            )
        })
}

/// The tool output a plugin must see: always the complete original.
///
/// `ToolResult::content` may be truncated to protect the LLM context
/// window; every truncating producer preserves the uncut output in
/// `full_content` when that happens. Plugins need full access to operate
/// correctly (a clipped JSON payload cannot be parsed by shape detection),
/// so the untruncated original wins whenever it exists and `content`
/// stands only for results that were never truncated.
fn full_output_for_plugin(result: &jinn_core_types::tool_types::ToolResult) -> String {
    result
        .full_content
        .clone()
        .unwrap_or_else(|| result.content.clone())
}

impl Message<SendToLlmProvider> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SendToLlmProvider,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // A generation start: the dispatch-to-first-token gap counts as
        // stall time, so this arms the guest-side stall timer. Note the
        // phase machine reuses `Sending` for tool-batch execution — that is
        // *not* a stream start, and no `SendToLlmProvider` flows then.
        let event = jinn_plugin_api::StreamStartEvent {
            session_id: msg.session_id.to_string(),
        };
        self.forward_event("stream_start", || {
            jinn_plugin_api::HostToPlugin::StreamStartEvent(event.clone())
        })
        .await;
    }
}

impl Message<StreamToken> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: StreamToken,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        stream_ping(self, &msg.session_id).await;
    }
}

impl Message<ToolUseStarted> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ToolUseStarted,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        stream_ping(self, &msg.session_id).await;
    }
}

impl Message<ToolCallStreaming> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ToolCallStreaming,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        stream_ping(self, &msg.session_id).await;
    }
}

impl Message<StreamCompleted> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: StreamCompleted,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Identity reason mapping — the wire enum mirrors the internal one.
        let reason = match msg.reason {
            StreamCompletedReason::Finished => jinn_plugin_api::StreamEndReason::Finished,
            StreamCompletedReason::Canceled => jinn_plugin_api::StreamEndReason::Canceled,
            StreamCompletedReason::ToolUse => jinn_plugin_api::StreamEndReason::ToolUse,
            StreamCompletedReason::Error => jinn_plugin_api::StreamEndReason::Error,
        };
        let event = jinn_plugin_api::StreamEndEvent {
            session_id: msg.session_id.to_string(),
            reason,
        };
        self.forward_event("stream_end", || {
            jinn_plugin_api::HostToPlugin::StreamEndEvent(event.clone())
        })
        .await;
    }
}

/// Forwards one liveness ping for the session's in-flight stream.
///
/// The four stream-activity messages (`StreamToken`, `ToolUseStarted`,
/// `ToolCallStreaming`, `ToolCallReceived`) all mean the same thing to a
/// guest — the stream produced output — so they collapse into one wire
/// event, uncoalesced: the watchdog's entire operating model is "receive
/// things to reset the timer", and the raw feed also serves future
/// liveness-tracking guests.
async fn stream_ping(
    coordinator: &PluginCoordinatorActor,
    session_id: &crate::protocol::SessionId,
) {
    let event = jinn_plugin_api::StreamEventPing {
        session_id: session_id.to_string(),
    };
    coordinator
        .forward_event("stream_event", || {
            jinn_plugin_api::HostToPlugin::StreamEventPing(event.clone())
        })
        .await;
}

impl Message<Tick> for PluginCoordinatorActor {
    type Reply = ();

    async fn handle(&mut self, _msg: Tick, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        let event = jinn_plugin_api::TickEvent { now_ms: now_ms() };
        self.forward_event("tick", || {
            jinn_plugin_api::HostToPlugin::Tick(event.clone())
        })
        .await;
    }
}

/// Unix epoch milliseconds — the guest tick's clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl PluginCoordinatorActor {
    /// Delivers a host event to every plugin subscribed to `kind`.
    ///
    /// Fire-and-forget: `tell` never blocks the bus on a slow or dead
    /// plugin actor; a dead plugin's map entry is cleared by its own
    /// lifecycle events.
    async fn forward_event<F>(&self, kind: &str, build: F)
    where
        F: Fn() -> jinn_plugin_api::HostToPlugin,
    {
        let targets: Vec<ActorRef<PluginActor>> = {
            let subscriptions = self.subscriptions.lock();
            let spawned = self.spawned.lock();
            subscriptions
                .iter()
                .filter(|(_, kinds)| kinds.iter().any(|k| k == kind))
                .filter_map(|(name, _)| spawned.get(name).cloned())
                .collect()
        };
        for actor_ref in targets {
            let _ = actor_ref.tell(DeliverHostEvent(build())).await;
        }
    }
}
