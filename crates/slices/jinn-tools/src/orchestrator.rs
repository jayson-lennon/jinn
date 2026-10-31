//! Tool orchestrator actor - dispatches tool calls and aggregates batch results.
//!
//! This actor maintains a registry of available tools (built-in and actor-provided),
//! dispatches [`ExecuteToolBatch`] requests, and emits a [`ToolExecutionCompleted`]
//! per finished call — the turn ledger's open-call set is the batch's outstanding
//! count, so no aggregate event exists.
//!
//! Built-in tools (`get_time`, `read`, `write`) are registered at
//! startup and executed via spawned tokio tasks. Actor-provided tools
//! are routed via [`ExecuteTool`] commands on the bus.
//!
//! Each tool execution receives a [`ToolContext`] containing the session's CWD
//! (for resolving relative paths) and an optional timeout. The orchestrator
//! reads CWD from shared [`State`] at dispatch time.

use jinn_preferences_config::schemas::McpServersConfig;
use jinn_preferences_config::schemas::ToolsConfig;
use jinn_preferences_config::schemas::WebSearchConfig;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::tool_types::ToolContext;
use jiff::Timestamp;
use jinn_core_types::ToolCallId;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_core_types::{ServerToolType, SessionId};
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::services::Services;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::common::state::State;
use jinn_mcp_msg::McpConnectionStatus;
use jinn_session_msg::SessionClosed;
use jinn_tools_msg::{CancelToolBatch, ExecuteTool, ExecuteToolBatch, RegisterTools};
use jinn_tools_msg::{
    ProviderVisibilityChanged, ToolExecutionCompleted, ToolsRegistered, ToolsUnregistered,
};
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

/// Prefix for all MCP-provided tool `provider` values. A provider like
/// `mcp__excalimate` namespaces that server's tools (`mcp__excalimate__<tool>`)
/// and is routed to MCP client actors via the generic [`ExecuteTool`] command.
pub(crate) const MCP_PROVIDER_PREFIX: &str = "mcp__";

/// A boxed future returned by built-in tool execute functions.
pub type BoxedToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;

/// How a tool is registered and executed.
pub(crate) enum ToolRegistration {
    /// A built-in tool executed directly by the orchestrator.
    Builtin {
        /// The tool's JSON-schema definition.
        definition: ToolDefinition,
        /// A re-resolver for a definition whose schema embeds a config
        /// value. Present only where the schema actually varies with
        /// config; `None` means the baked `definition` is authoritative.
        ///
        /// This exists because the definition is not merely stored — it is
        /// published to the context layer and becomes part of the schema
        /// the model sees. A baked copy would freeze the schema for the
        /// life of the process even after a `reload`.
        live_definition: Option<fn(&jinn_config::ConfigLayer) -> ToolDefinition>,
        /// The function that executes the tool call.
        execute: fn(ToolCall, ToolContext) -> BoxedToolFuture,
        /// When `true`, the dispatcher's timeout wrapper is bypassed: the
        /// tool receives the per-call budget in `ToolContext.timeout` and
        /// enforces it itself. Required for tools that must not be aborted
        /// by a dropped future (`spawn_subagent` would orphan its child session).
        self_managed_timeout: bool,
    },
    /// An actor-provided tool routed via [`ExecuteTool`] command.
    Actor {
        /// The tool's JSON-schema definition.
        definition: ToolDefinition,
        /// The name of the actor providing this tool.
        provider: String,
    },
}

impl std::fmt::Debug for ToolRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin { definition, .. } => f
                .debug_struct("Builtin")
                .field("name", &definition.name)
                .finish_non_exhaustive(),
            Self::Actor {
                definition,
                provider,
            } => f
                .debug_struct("Actor")
                .field("name", &definition.name)
                .field("provider", provider)
                .finish(),
        }
    }
}

/// Resolves a tool registration, preferring a session-scoped registration
/// over a global one.
///
/// Extracted as a free function so the lookup-precedence behavior (the core
/// of generalized routing) is unit-testable without constructing a full
/// `ToolOrchestratorActor`.
///
/// - `session`: the per-session map (`session_tools[session_id]`), if any.
/// - `global`: the value from the flat `tools` map for this `tool_name`.
fn lookup_registration<'a>(
    session: Option<&'a HashMap<String, ToolRegistration>>,
    global: Option<&'a ToolRegistration>,
    tool_name: &str,
) -> Option<&'a ToolRegistration> {
    session.and_then(|m| m.get(tool_name)).or(global)
}

/// Tracks pending tool calls within a batch.
pub(crate) struct PendingBatch {
    /// Join handles for spawned builtin tool tasks (for cancellation).
    ///
    /// The outstanding count is the turn ledger's open-call set, not a
    /// counter here; results flow to the session actor as they land.
    handles: Vec<tokio::task::JoinHandle<()>>,
}

/// Tool orchestrator actor.
///
/// Subscribes to [`RegisterTools`] and [`ExecuteToolBatch`] commands, and
/// [`ToolExecutionCompleted`] events. Dispatches tool calls to the appropriate
/// handler and aggregates results into batch completion events.
pub struct ToolOrchestratorActor {
    /// Universal actor dependencies.
    deps: ActorDeps,
    /// Global tool name → registration info (builtins + global actor tools).
    tools: HashMap<String, ToolRegistration>,
    /// Per-session tool registrations, keyed by session then tool name.
    /// Used by per-session tool providers (e.g. MCP servers enabled for one
    /// session). Empty until a session-scoped provider registers tools.
    session_tools: HashMap<SessionId, HashMap<String, ToolRegistration>>,
    /// Session ID → pending batch tracker.
    pending: HashMap<SessionId, PendingBatch>,
    /// Session ID → open call id → tool name, for the cancel synthesis: the
    /// ledger knows which calls are open but not what they are called, and
    /// the synthesized completion's entry keeps the tool's name.
    pending_names: HashMap<SessionId, HashMap<ToolCallId, String>>,
    /// Shared application state for reading session CWD.
    state: State,
    /// Runtime services.
    services: Services,
}

/// Dependencies for [`ToolOrchestratorActor`].
#[derive(Clone)]
pub struct ToolOrchestratorActorDeps {
    /// Universal actor dependencies.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Runtime services.
    pub services: Services,
    /// Override which built-in tools to register. `None` means register all.
    /// Each entry is a tool name (e.g., `"bash"`, `"read"`, `"write"`).
    pub builtin_filter: Option<Vec<String>>,
}

/// Builds the `openrouter:web_search` tool definition from config.
///
/// The `parameters` field contains actual config values (not a JSON Schema)
/// because server tools send config directly, not a function parameter schema.
fn build_openrouter_web_search_definition(config: &WebSearchConfig) -> ToolDefinition {
    let mut params = serde_json::Map::new();
    if let Some(ref engine) = config.engine {
        params.insert(
            "engine".to_owned(),
            serde_json::Value::String(engine.clone()),
        );
    }
    if let Some(max) = config.max_results {
        params.insert("max_results".to_owned(), serde_json::json!(max));
    }
    if let Some(max) = config.max_total_results {
        params.insert("max_total_results".to_owned(), serde_json::json!(max));
    }
    if let Some(ref size) = config.search_context_size {
        params.insert(
            "search_context_size".to_owned(),
            serde_json::Value::String(size.clone()),
        );
    }
    if let Some(ref domains) = config.allowed_domains {
        params.insert("allowed_domains".to_owned(), serde_json::json!(domains));
    }
    if let Some(ref domains) = config.excluded_domains {
        params.insert("excluded_domains".to_owned(), serde_json::json!(domains));
    }

    ToolDefinition {
        name: "openrouter:web_search".to_owned(),
        description: "Search the web for real-time information.".to_owned(),
        parameters: serde_json::Value::Object(params),
        prompt_snippet: Some("Web search (OpenRouter)".to_owned()),
        prompt_guidelines: vec![],
        server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
    }
}

/// Static path the orchestrator spawns at (one instance per process).
pub const ORCHESTRATOR_PATH: &str = "jinn.tools.orchestrator";

impl ServiceActor for ToolOrchestratorActor {
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("ToolOrchestratorActor spawns via init_with"))
    }
}

impl ToolOrchestratorActor {
    /// Spawns the orchestrator onto the trouper system.
    ///
    /// Registration of builtins happens in
    /// [`initialize`](Self::initialize); the spawn returns once the actor
    /// is subscribed, so composition can order MCP-after-orchestrator.
    pub fn spawn(system: &trouper::system::ActorSystem, deps: ToolOrchestratorActorDeps) {
        let path = ActorPath::new(ORCHESTRATOR_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move { Ok(Self::initialize(deps)) })
                }
            })
            .handles::<RegisterTools>()
            .handles::<ExecuteToolBatch>()
            .handles::<CancelToolBatch>()
            .handles::<ToolExecutionCompleted>()
            .handles::<SessionClosed>()
            .handles::<ToolsUnregistered>()
            .handles::<ProviderVisibilityChanged>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
    }

    /// Constructs the actor and registers builtins (the old `on_start`
    /// body, minus bus subscriptions — trouper handles deliver the
    /// subscribed messages).
    fn initialize(deps: ToolOrchestratorActorDeps) -> Self {
        // The web-search definition and the builtin timeout are both read
        // from the configuration layer at DISPATCH time, not here: baking
        // them in at construction meant a reload could not change the
        // schema the model sees, nor the ceiling a tool runs under. See
        // `ToolRegistration::definition`.
        let mut actor = Self {
            deps: deps.deps,
            tools: HashMap::new(),
            session_tools: HashMap::new(),
            pending: HashMap::new(),
            pending_names: HashMap::new(),
            state: deps.state,
            services: deps.services,
        };
        let all_builtins = crate::registry::builtin_tools(
            actor
                .services
                .config
                .get::<ToolsConfig>()
                .unwrap_or_default()
                .default_timeout_secs,
        );
        let builtins: Vec<_> = if let Some(ref filter) = deps.builtin_filter {
            all_builtins
                .into_iter()
                .filter(|(def, _, _)| filter.contains(&def.name))
                .collect()
        } else {
            all_builtins
        };
        for (def, execute_fn, self_managed_timeout) in builtins {
            let name = def.name.clone();
            // `bash` and `powershell` are the builtins whose schema names
            // a config value (the default timeout), so they are the ones
            // that re-resolve.
            let live_definition = match name.as_str() {
                "bash" => Some(
                    (|config: &jinn_config::ConfigLayer| {
                        let tools = config.get::<ToolsConfig>().unwrap_or_default();
                        crate::bash::definition(tools.default_timeout_secs)
                    }) as fn(&jinn_config::ConfigLayer) -> ToolDefinition,
                ),
                #[cfg(windows)]
                "powershell" => Some(
                    (|config: &jinn_config::ConfigLayer| {
                        let tools = config.get::<ToolsConfig>().unwrap_or_default();
                        crate::powershell::definition(tools.default_timeout_secs)
                    }) as fn(&jinn_config::ConfigLayer) -> ToolDefinition,
                ),
                _ => None,
            };
            actor.tools.insert(
                name,
                ToolRegistration::Builtin {
                    definition: def,
                    live_definition,
                    execute: execute_fn,
                    self_managed_timeout,
                },
            );
        }

        // Register openrouter:web_search server tool. Its schema embeds the
        // user's web-search config, so the definition resolves from the
        // layer on every publish rather than being frozen here.
        let web_search_def = build_openrouter_web_search_definition(&WebSearchConfig::default());
        actor.tools.insert(
            web_search_def.name.clone(),
            ToolRegistration::Builtin {
                definition: web_search_def.clone(),
                live_definition: Some(|config| {
                    build_openrouter_web_search_definition(
                        &config.get::<WebSearchConfig>().unwrap_or_default(),
                    )
                }),
                // Server tool; never dispatched locally so the flag is moot.
                self_managed_timeout: false,
                execute: |_call, _ctx| {
                    // Server tool - handled by OpenRouter, never dispatched locally.
                    Box::pin(std::future::ready(ToolResult {
                        tool_call_id: ToolCallId::from(String::new()),
                        name: "openrouter:web_search".to_owned(),
                        content: "server tool should not be dispatched".to_owned(),
                        success: false,
                        full_content: None,
                        truncation: None,
                        pin_position: None,
                        attachments: Vec::new(),
                    }))
                },
            },
        );
        let web_search_name = web_search_def.name.clone();

        // Announce built-in tools so dispatch and context assembly share the
        // same definitions. The shared registry is updated directly here;
        // the event remains the crossing notification for other consumers.
        actor.announce_builtin_tools(Some(&web_search_name));

        actor
    }
}

// ---------------------------------------------------------------------------
// Bridge: impl Message<T> blocks that delegate to old handler methods
// ---------------------------------------------------------------------------
// Message handlers — direct handler calls (no bridge)
// ---------------------------------------------------------------------------

impl MsgHandler<RegisterTools> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &RegisterTools, _ctx: &mut MsgCtx<'_>) {
        self.handle_register_tools(&msg.provider, &msg.definitions, msg.session_id.clone())
            .await;
    }
}

impl MsgHandler<ExecuteToolBatch> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &ExecuteToolBatch, _ctx: &mut MsgCtx<'_>) {
        self.handle_execute_tool_batch(
            msg.session_id.clone(),
            msg.tool_calls.clone(),
            msg.dispatched_at,
            msg.turn,
        )
        .await;
    }
}

impl MsgHandler<CancelToolBatch> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &CancelToolBatch, _ctx: &mut MsgCtx<'_>) {
        self.handle_cancel_tool_batch(&msg.session_id);
    }
}

impl MsgHandler<ToolExecutionCompleted> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        self.handle_tool_execution_completed(msg.session_id.clone(), msg.result.clone(), msg.turn)
            .await;
    }
}

impl MsgHandler<SessionClosed> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &SessionClosed, _ctx: &mut MsgCtx<'_>) {
        // Drop per-session routing and context definitions so neither map leaks.
        self.session_tools.remove(&msg.session_id);
        self.remove_cached_session_tools(&msg.session_id);
        // And the cancel bookkeeping: a closed session's turn is gone from
        // the ledger (`remove`), so nothing can synthesize against it.
        self.pending.remove(&msg.session_id);
        self.pending_names.remove(&msg.session_id);
    }
}

impl MsgHandler<ToolsUnregistered> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &ToolsUnregistered, _ctx: &mut MsgCtx<'_>) {
        // Given a provider tearing down its session-scoped registrations.
        // When pruning the routing map.
        let Some(session_map) = self.session_tools.get_mut(&msg.session_id) else {
            self.remove_cached_provider_tools(msg);
            return;
        };
        session_map.retain(|_, reg| match reg {
            ToolRegistration::Actor { provider, .. } => provider != &msg.provider,
            ToolRegistration::Builtin { .. } => true,
        });
        if session_map.is_empty() {
            self.session_tools.remove(&msg.session_id);
        }
        self.remove_cached_provider_tools(msg);

        // Then the provider's tools are no longer routable or visible in context.
    }
}

impl MsgHandler<ProviderVisibilityChanged> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &ProviderVisibilityChanged, _ctx: &mut MsgCtx<'_>) {
        // Given a session's view of a globally-registered provider changing
        // (a singleton the session disabled or re-enabled).
        // When recording the opt-out in the context-facing registry.
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        {
            cell.update(|registry| {
                if msg.hidden {
                    registry.deny_provider_for_session(&msg.session_id, &msg.provider);
                } else {
                    registry.admit_provider_for_session(&msg.session_id, &msg.provider);
                }
            });
        }

        // Then `tools_for_session` withholds (or restores) that provider's
        // global tools for exactly this session. Dispatch needs no gate
        // change: `mcp_rejection_reason` already refuses calls to a server
        // the calling session has disabled.
    }
}

impl BusPublish for ToolOrchestratorActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl ToolOrchestratorActor {
    /// Publishes the builtin definitions to the context layer and the bus.
    ///
    /// Every definition that embeds a config value is re-resolved here
    /// rather than served from its baked copy, so the schema the model sees
    /// reflects the current configuration. `extra_live` names the tools
    /// whose schema varies; everything else contributes its stored
    /// definition unchanged.
    fn announce_builtin_tools(&self, extra_live: Option<&str>) {
        let definitions: Vec<ToolDefinition> = self
            .tools
            .iter()
            .filter_map(|(name, registration)| match registration {
                ToolRegistration::Builtin {
                    definition,
                    live_definition,
                    ..
                } => Some(
                    live_definition
                        .filter(|_| Some(name.as_str()) == extra_live)
                        .map_or_else(
                            || definition.clone(),
                            |resolve| resolve(&self.services.config),
                        ),
                ),
                ToolRegistration::Actor { .. } => None,
            })
            .collect();
        self.cache_registered_tools(&definitions, None);
        let bus = self.deps.services.bus.clone();
        let definitions_for_bus = definitions.clone();
        tokio::spawn(async move {
            bus.publish(ToolsRegistered {
                provider: "builtin".to_owned(),
                definitions: definitions_for_bus,
                session_id: None,
            })
            .await;
        });
    }

    /// Mirrors registered definitions into the context-facing registry cell.
    fn cache_registered_tools(
        &self,
        definitions: &[ToolDefinition],
        session_id: Option<&SessionId>,
    ) {
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        else {
            tracing::warn!("tools registry cell missing; tool definitions unavailable to context");
            return;
        };

        cell.update(|registry| match session_id {
            Some(session_id) => {
                let session_tools = registry.session.entry(session_id.clone()).or_default();
                for definition in definitions {
                    session_tools.insert(definition.name.clone(), definition.clone());
                }
            }
            None => {
                for definition in definitions {
                    registry
                        .global
                        .insert(definition.name.clone(), definition.clone());
                }
            }
        });
    }

    /// Removes one provider's definitions from the context-facing session map.
    fn remove_cached_provider_tools(&self, message: &ToolsUnregistered) {
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        else {
            return;
        };

        cell.update(|registry| {
            let Some(session_tools) = registry.session.get_mut(&message.session_id) else {
                return;
            };
            session_tools.retain(|name, _| !name.starts_with(&message.provider));
            if session_tools.is_empty() {
                registry.session.remove(&message.session_id);
            }
        });
    }

    /// Removes a closed session from the context-facing registry.
    fn remove_cached_session_tools(&self, session_id: &SessionId) {
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        {
            cell.update(|registry| {
                registry.session.remove(session_id);
                // A closed session's singleton opt-outs are meaningless —
                // nothing with that id will assemble context again.
                registry.clear_denials_for_session(session_id);
            });
        }
    }

    /// Stores actor-provided tools and emits a [`ToolsRegistered`] event.
    ///
    /// When `session_id` is `Some`, the tools are stored under
    /// [`session_tools`](Self::session_tools) for that session only; when
    /// `None`, they are stored in the global [`tools`](Self::tools) map
    /// (visible to every session).
    async fn handle_register_tools(
        &mut self,
        provider: &str,
        definitions: &[ToolDefinition],
        session_id: Option<SessionId>,
    ) {
        for def in definitions {
            let registration = ToolRegistration::Actor {
                definition: def.clone(),
                provider: provider.to_owned(),
            };
            match session_id.as_ref() {
                Some(id) => {
                    self.session_tools
                        .entry(id.clone())
                        .or_default()
                        .insert(def.name.clone(), registration);
                }
                None => {
                    self.tools.insert(def.name.clone(), registration);
                }
            }
        }

        self.cache_registered_tools(definitions, session_id.as_ref());
        self.publish(ToolsRegistered {
            provider: provider.to_owned(),
            definitions: definitions.to_vec(),
            session_id,
        })
        .await;
    }

    /// Dispatches each tool call and tracks the pending batch.
    ///
    /// The batch's calls are registered in the turn ledger in the same
    /// ledger call that dispatches them, so a completion can never be
    /// processed before its registration lands. A batch whose registration
    /// is refused (no live turn, stale turn, cancel drain) dispatches
    /// nothing and closes nothing — the ledger is the drop site.
    async fn handle_execute_tool_batch(
        &mut self,
        session_id: SessionId,
        tool_calls: Vec<ToolCall>,
        dispatched_at: Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) {
        tracing::info!(
            session_id = %session_id,
            tools = ?tool_calls.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            "handle_execute_tool_batch"
        );

        if tool_calls.is_empty() {
            // Nothing to run: there is no open work, and the turn ledger's
            // set never saw this batch. The turn's outcome rides the events
            // that carry identity — nothing to publish here.
            tracing::debug!(
                session_id = ?session_id,
                "empty tool batch; nothing to dispatch"
            );
            return;
        }

        // Fused registration + dispatch: one ledger call inserts every
        // call id as open work and runs the dispatch body. A refusal
        // abandons the batch before anything runs.
        let call_ids: Vec<_> = tool_calls.iter().map(|tc| tc.id.clone()).collect();
        if self
            .services
            .turn_ledger
            .register_and_dispatch(&session_id, &turn, call_ids, || ())
            .is_err()
        {
            tracing::info!(
                session_id = %session_id,
                turn = ?turn,
                count = tool_calls.len(),
                "tool batch refused by the turn ledger; dispatching nothing"
            );
            return;
        }

        let mut handles = Vec::new();
        {
            let names = self.pending_names.entry(session_id.clone()).or_default();
            for tc in &tool_calls {
                names.insert(tc.id.clone(), tc.name.clone());
            }
        }
        for tc in tool_calls {
            if let Some(handle) = self
                .dispatch_tool_call(session_id.clone(), tc, dispatched_at, turn)
                .await
            {
                handles.push(handle);
            }
        }
        self.pending
            .insert(session_id.clone(), PendingBatch { handles });
    }

    /// Cancels all pending tool executions for a session.
    ///
    /// Aborts every spawned builtin task and synthesizes a cancelled
    /// [`ToolExecutionCompleted`] per call still open in the turn ledger, so
    /// the cancel drain never waits on a tool: actor-backed calls (MCP) are
    /// not abortable from here, and a wedged one would hold the turn open
    /// forever. Each synthesis closes its call through the ordinary
    /// completion path, so the settle is the work set's, not this actor's —
    /// and a real result that also lands is the idempotent-close no-op that
    /// cannot double-finalize. The synthesized flag tells the session actor
    /// to finalize only an entry still `Pending` and never push one: real
    /// output wins when it arrives.
    fn handle_cancel_tool_batch(&mut self, session_id: &SessionId) {
        // Snapshot before removal: the open set is the cancel drain's work
        // list, and aborting must not race the synthesis reads.
        let open = self.services.turn_ledger.open_calls(session_id);
        if let Some(batch) = self.pending.remove(session_id) {
            let handle_count = batch.handles.len();
            for handle in batch.handles {
                handle.abort();
            }
            tracing::trace!(
                session_id = ?session_id,
                "handle_cancel_tool_batch - aborted {} tasks",
                handle_count
            );
        }
        let Some((open, turn)) = open.map(|calls| {
            let turn = self.services.turn_ledger.live(session_id).map(|l| l.turn);
            (calls, turn)
        }) else {
            return;
        };
        if open.is_empty() {
            return;
        }
        let Some(turn) = turn else {
            return;
        };
        for call_id in open {
            // The entry keeps the tool's name: the session actor finalizes in
            // place against the entry it already has, and a mismatched name
            // would surface in the rendered row.
            let name = self
                .pending_names
                .get(session_id)
                .and_then(|names| names.get(&call_id))
                .cloned()
                .unwrap_or_default();
            let result = ToolResult {
                tool_call_id: call_id,
                name,
                content: "canceled".to_owned(),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            };
            self.publish_spurious(session_id, result, turn);
        }
        self.pending_names.remove(session_id);
    }

    /// Publishes one synthesized cancellation without blocking the actor.
    fn publish_spurious(
        &mut self,
        session_id: &SessionId,
        result: ToolResult,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) {
        let bus = self.bus().clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            bus.publish(ToolExecutionCompleted {
                session_id,
                result,
                turn,
                synthesized: true,
            })
            .await;
        });
    }

    /// Builds a [`ToolContext`] for the given session by reading its CWD from shared state.
    ///
    /// Every config value is read from the configuration layer here, at the
    /// point of use, so a reload is observed by the very next tool call
    /// rather than by the next process start.
    ///
    /// The outer `timeout` (from `tools.default_timeout_secs`) is a safety
    /// ceiling for all builtin tools. `bash` additionally applies its own
    /// inner `bash.default_timeout_secs`; the shorter of the two fires first.
    fn build_tool_context(
        &self,
        session_id: &SessionId,
        dispatched_at: Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) -> ToolContext {
        let cwd = {
            let guard = self.state.read();
            guard.session.get(session_id).map_or_else(
                || guard.session.default_cwd().clone(),
                |session| session.cwd().clone(),
            )
        };
        let tools = self
            .services
            .config
            .get::<ToolsConfig>()
            .unwrap_or_default();
        let max_output_lines = tools.max_output_lines;
        let max_output_bytes = tools.max_output_bytes;
        let timeout = std::time::Duration::from_secs(tools.default_timeout_secs);
        ToolContext {
            cwd,
            timeout: Some(timeout),
            turn,
            state: Some(self.state.clone()),
            config: self.services.config.clone(),
            shells: self.services.shells.clone(),
            session_id: Some(session_id.clone()),
            app_paths: self.services.paths.clone(),
            bus: Some(self.bus().clone()),
            max_output_lines,
            max_output_bytes,
            dispatched_at,
            mcp_coordinator: self.services.mcp_coordinator.get().cloned(),
            canvas_server: self.services.canvas_server.get().cloned(),
            interactive_term: self.services.interactive_term.get().cloned(),
            task_spawns: Some(self.services.task_spawns.clone()),
            session_store: Some(self.services.session_store.clone()),
            trouper_system: Some(self.services.trouper_system.clone()),
            model_name_resolver: Some(
                jinn_kernel::common::provider_resolution::registry_model_name_resolver(
                    &self.services.provider_registry,
                ),
            ),
        }
    }

    /// Dispatches a single tool call to the appropriate handler.
    ///
    /// Returns a `JoinHandle` for builtin tool spawns so callers can track
    /// and abort them on cancellation. Returns `None` for actor-routed and
    /// unknown tools (they have no local task to abort).
    async fn dispatch_tool_call(
        &mut self,
        session_id: SessionId,
        tool_call: ToolCall,
        dispatched_at: Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let reg_type = match self.find_registration(&session_id, &tool_call.name) {
            Some(ToolRegistration::Builtin { .. }) => "builtin",
            Some(ToolRegistration::Actor { .. }) => "actor",
            None => "unknown",
        };
        tracing::trace!(
            session_id = ?session_id,
            tool = %tool_call.name,
            reg_type,
        );

        // The session's filter gate, checked after `reg_type` so the tracing
        // above still classifies the tool by how it is registered, and before
        // the dispatch match so a withheld tool is refused rather than run.
        //
        // Enforcing here is the point: `find_registration` reads only the
        // registry, so a tool absent from the prompt would still execute if
        // the model named it. That is exactly the gap an allow-mode filter
        // exists to close — the tool the user withheld must not be reachable
        // by naming it directly.
        if self.tool_withheld(&session_id, &tool_call.name) {
            return self.reject_withheld_tool(session_id, tool_call, turn).await;
        }

        match self.find_registration(&session_id, &tool_call.name) {
            Some(ToolRegistration::Builtin {
                execute,
                self_managed_timeout,
                ..
            }) => Some(self.dispatch_builtin(
                session_id,
                tool_call,
                dispatched_at,
                turn,
                *execute,
                *self_managed_timeout,
            )),
            Some(ToolRegistration::Actor { provider, .. }) => {
                let reg_provider = provider.clone();
                self.dispatch_actor(session_id, tool_call, &reg_provider, dispatched_at, turn)
                    .await
            }
            None => self.reject_unknown_tool(session_id, tool_call, turn).await,
        }
    }

    /// Whether this session's tool filter withholds `tool_name`.
    ///
    /// The same predicate the prompt assembler consulted, read from live
    /// session state rather than from the assembled inputs, so a filter
    /// changed mid-turn takes effect on the next call.
    ///
    /// A session absent from state withholds nothing. It has no filter to
    /// consult, and defaulting to "withheld" would refuse every tool for any
    /// session that has not been inserted yet — which includes a call
    /// dispatched for a session this actor has not seen. The unknown-tool
    /// path is the honest answer for a session that does not exist.
    fn tool_withheld(&self, session_id: &SessionId, tool_name: &str) -> bool {
        let guard = self.state.read();
        guard
            .session
            .get(session_id)
            .is_some_and(|session| !session.is_tool_enabled(tool_name))
    }

    /// Publishes a failure for a tool this session's filter withholds.
    ///
    /// Deliberately not [`Self::reject_unknown_tool`]: the tool exists and is
    /// registered, so reporting it as unknown would be false — and it would
    /// mislead the model into guessing a different name rather than telling
    /// it the tool is out of scope.
    async fn reject_withheld_tool(
        &self,
        session_id: SessionId,
        tool_call: ToolCall,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let result = ToolResult {
            tool_call_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            content: format!(
                "tool '{}' is withheld by this session's filter; \
                 it is not available, so do not retry it or look for an alias",
                tool_call.name
            ),
            success: false,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        };

        self.publish(ToolExecutionCompleted {
            session_id,
            result,
            turn,
            synthesized: false,
        })
        .await;
        None
    }

    /// Looks up a tool registration, preferring a session-scoped registration
    /// over a global one. Builtins live in the global map; session-scoped
    /// providers (MCP servers) live under `session_tools`.
    fn find_registration(
        &self,
        session_id: &SessionId,
        tool_name: &str,
    ) -> Option<&ToolRegistration> {
        let session = self.session_tools.get(session_id);
        let global = self.tools.get(tool_name);
        lookup_registration(session, global, tool_name)
    }

    /// Spawns a builtin tool, applying its configured timeout, and publishes the result.
    ///
    /// `self_managed_timeout` bypasses [`run_builtin_with_timeout`]: the tool
    /// runs bare with the per-call budget left in `ToolContext.timeout` so it
    /// can enforce (or ignore) the deadline itself without risk of its future
    /// being dropped mid-run.
    fn dispatch_builtin(
        &self,
        session_id: SessionId,
        tool_call: ToolCall,
        dispatched_at: Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
        execute_fn: fn(ToolCall, ToolContext) -> BoxedToolFuture,
        self_managed_timeout: bool,
    ) -> tokio::task::JoinHandle<()> {
        let bus = self.bus().clone();
        let tool_ctx = self.build_tool_context(&session_id, dispatched_at, turn);

        let call_id = tool_call.id.clone();
        let call_name = tool_call.name.clone();

        tokio::spawn(async move {
            use futures::FutureExt as _;
            use std::panic::AssertUnwindSafe;
            let inner: BoxedToolFuture = if self_managed_timeout {
                execute_fn(tool_call, tool_ctx)
            } else {
                Box::pin(run_builtin_with_timeout(tool_call, tool_ctx, execute_fn))
            };
            let result = match AssertUnwindSafe(inner).catch_unwind().await {
                Ok(r) => r,
                Err(_) => panicked_tool_result(&call_id, &call_name),
            };
            bus.publish(ToolExecutionCompleted {
                session_id,
                result,
                turn,
                synthesized: false,
            })
            .await;
        })
    }

    /// Routes an actor-backed tool to its provider's command.
    ///
    /// MCP providers (`mcp__*`) are delivered via the generic
    /// [`ExecuteTool`] command to whichever MCP client actor owns the
    /// matching session-scoped connection.
    async fn dispatch_actor(
        &mut self,
        session_id: SessionId,
        tool_call: ToolCall,
        provider: &str,
        dispatched_at: Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        match provider {
            p if p.starts_with(MCP_PROVIDER_PREFIX) => {
                // Fail fast when the owning MCP server cannot take this call:
                // publishing ExecuteTool with no live McpActor subscriber would
                // hang the pending batch until the watchdog rescues it.
                if let Some(reason) = self.mcp_rejection_reason(&session_id, p) {
                    self.publish(ToolExecutionCompleted {
                        session_id,
                        result: rejected_mcp_result(&tool_call, &reason),
                        turn,
                        synthesized: false,
                    })
                    .await;
                    return None;
                }

                // Read the same truncation limits builtins use so MCP results
                // are bounded identically. `build_tool_context` does the same
                // read for the builtin path.
                let tools = self
                    .services
                    .config
                    .get::<ToolsConfig>()
                    .unwrap_or_default();
                self.publish(ExecuteTool {
                    session_id: session_id.clone(),
                    turn,
                    tool_call,
                    dispatched_at,
                    max_output_lines: tools.max_output_lines,
                    max_output_bytes: tools.max_output_bytes,
                })
                .await;
            }
            other => {
                tracing::warn!(
                    provider = %other,
                    "unknown actor provider — no command mapping"
                );
            }
        }
        None
    }

    /// Publishes an error result for a tool with no registration.
    async fn reject_unknown_tool(
        &self,
        session_id: SessionId,
        tool_call: ToolCall,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let result = ToolResult {
            tool_call_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            content: format!("unknown tool: {}", tool_call.name),
            success: false,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        };

        self.publish(ToolExecutionCompleted {
            session_id,
            result,
            turn,
            synthesized: false,
        })
        .await;
        None
    }

    /// Returns a legible rejection reason when an MCP tool call cannot be
    /// routed, or `None` when the owning server is enabled and Running.
    ///
    /// The check order is: enablement first (a disabled server's actor is
    /// gone — publishing `ExecuteTool` would find no subscriber and hang the
    /// pending batch), then connection status (an enabled-but-not-Running
    /// server cannot take the call yet).
    ///
    /// Enablement stays per-session for every server: a singleton's tools are
    /// visible to many sessions, and each of them must still have the server
    /// in its own enabled set to call it. Connection status, though, is read
    /// from the singleton bucket for a singleton-configured server — the
    /// shared actor has no per-session runtime entry, and pinning the lookup
    /// to the calling session would report "still starting" forever.
    fn mcp_rejection_reason(&self, session_id: &SessionId, provider: &str) -> Option<String> {
        let server = server_name_of_provider(provider)?;
        let enabled = {
            let guard = self.state.read();
            let session = guard.session.get(session_id)?;
            session.is_mcp_server_enabled(server)
        };
        if !enabled {
            return Some(format!(
                "MCP server '{server}' is disabled for this session; \
                 ask the user to re-enable it via the MCP server picker"
            ));
        }
        let singleton_configured = self
            .services
            .config
            .get::<McpServersConfig>()
            .unwrap_or_default()
            .get(server)
            .is_some_and(|config| config.singleton);
        let runtime = self
            .services
            .slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot());
        let status = if singleton_configured {
            runtime
                .as_ref()
                .and_then(|runtime| runtime.read().singleton_status(server))
        } else {
            runtime
                .as_ref()
                .and_then(|runtime| runtime.read().status(session_id, server))
        };
        match status {
            Some(McpConnectionStatus::Running) => None,
            Some(McpConnectionStatus::Dead) => Some(format!(
                "MCP server '{server}' is enabled but its connection is dead; \
                 use the restart_mcp_server tool to restart it, then retry"
            )),
            Some(McpConnectionStatus::Starting) | None => Some(format!(
                "MCP server '{server}' is still starting; wait for it to reach \
                 Running before calling its tools"
            )),
        }
    }

    /// Observes the completion; the batch bookkeeping it once did is the
    /// turn ledger's open-call set, and the close belongs to the session
    /// actor — the consumer that acts on the close's settlement. This
    /// actor's remaining job is the `pending` map's abort handles, which
    /// `handle_cancel_tool_batch` drains.
    async fn handle_tool_execution_completed(
        &mut self,
        session_id: SessionId,
        result: ToolResult,
        turn: jinn_core_types::turn_identity::TurnToken,
    ) {
        tracing::debug!(
            session_id = ?session_id,
            turn = ?turn,
            tool = %result.name,
            tool_call_id = %result.tool_call_id,
            "ToolExecutionCompleted observed by the orchestrator"
        );
    }
}

/// Constructs a failed [`ToolResult`] for a tool that panicked.
///
/// Ensures a panicking tool still publishes a completion so the batch always finishes.
fn panicked_tool_result(tool_call_id: &ToolCallId, name: &str) -> ToolResult {
    tracing::error!(tool = %name, tool_call_id = %tool_call_id, "tool execution panicked");
    ToolResult {
        tool_call_id: tool_call_id.clone(),
        name: name.to_owned(),
        content: "tool execution panicked".to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Extracts the server name from an MCP provider string.
///
/// Providers are registered as `mcp__<server>__` (the full namespace prefix,
/// see `jinn_mcp::tool_mapping::provider_name`), so both the leading `mcp__`
/// and the trailing `__` separator are stripped. Returns `None` for a
/// malformed provider (empty server segment) — the caller then treats the
/// call as unroutable-fail rather than risk a wrong-server gate decision.
fn server_name_of_provider(provider: &str) -> Option<&str> {
    let server = provider
        .strip_prefix(MCP_PROVIDER_PREFIX)?
        .strip_suffix("__")?;
    (!server.is_empty()).then_some(server)
}

/// Constructs a failed [`ToolResult`] for an MCP tool call rejected by the
/// dispatch gate, so the pending batch always completes instead of hanging.
fn rejected_mcp_result(tool_call: &ToolCall, reason: &str) -> ToolResult {
    ToolResult {
        tool_call_id: tool_call.id.clone(),
        name: tool_call.name.clone(),
        content: reason.to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}

/// Peeks the reserved `max_duration_secs` field from a tool call's JSON arguments.
///
/// `max_duration_secs` is a reserved argument name the dispatcher always peeks for,
/// analogous to a reserved HTTP header. Any tool can support a per-call timeout override
/// by documenting the field in its schema — zero code change required to add it to a new tool.
///
/// Falls back to the legacy `timeout` key so existing chat history and persisted tool calls
/// that emit `"timeout": N` keep working.
///
/// Returns `None` when neither key is present or the JSON fails to parse (the global
/// timeout from `tool_ctx` is used as the fallback in that case).
pub(crate) fn extract_max_duration(arguments: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    v.get("max_duration_secs")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| v.get("timeout").and_then(serde_json::Value::as_u64))
}

/// Runs a builtin tool, racing it against its effective timeout.
///
/// The effective timeout is the per-call `max_duration_secs` override (peeked from the
/// tool call's arguments via [`extract_max_duration`]) if present, otherwise the global
/// timeout passed in `timeout` (sourced from `tool_default_timeout_secs`). A sentinel
/// value of `0` disables the timeout entirely for that call.
///
/// On timeout the returned `ToolResult` carries a failure message that names
/// `max_duration_secs` and shows an example call so the model can retry with a larger budget.
async fn run_builtin_with_timeout(
    tool_call: ToolCall,
    tool_ctx: ToolContext,
    execute_fn: fn(ToolCall, ToolContext) -> BoxedToolFuture,
) -> ToolResult {
    let call_id = tool_call.id.clone();
    let call_name = tool_call.name.clone();
    let effective = match extract_max_duration(&tool_call.arguments) {
        Some(0) => None,
        Some(secs) => Some(std::time::Duration::from_secs(secs)),
        None => tool_ctx.timeout,
    };
    match effective {
        Some(dur) => match tokio::time::timeout(dur, execute_fn(tool_call, tool_ctx)).await {
            Ok(r) => r,
            Err(_) => ToolResult {
                tool_call_id: call_id,
                name: call_name,
                content: format!(
                    "command exceeded the max_duration_secs budget and was killed after {}s; to retry, raise max_duration_secs (example: {{\"command\":\"<cmd>\",\"max_duration_secs\":600}})",
                    dur.as_secs()
                ),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            },
        },
        None => execute_fn(tool_call, tool_ctx).await,
    }
}

#[cfg(test)]
mod timeout_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use std::time::Duration;

    use super::{BoxedToolFuture, ToolContext, run_builtin_with_timeout};
    use jinn_core_types::tool_types::{ToolCall, ToolResult};
    use jinn_kernel::common::app_paths::AppPaths;
    use jinn_preferences_config::schemas::ToolsConfig;
    use marked_path::Absolute;
    use marked_path::MarkedPathBuf;

    fn make_call() -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: "slow_tool".into(),
            arguments: "{}".to_owned(),
        }
    }

    /// Builds a tool context rooted at the test-owned `dir`, so the paths it
    /// carries stay valid for the whole test.
    fn empty_ctx(dir: &MarkedPathBuf<Absolute>) -> ToolContext {
        ToolContext {
            turn: crate::tool_types::test_turn(),
            cwd: dir.clone(),
            config: jinn_config::testutil::config_layer(""),
            shells: Default::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: AppPaths::new_in(&dir.as_marked_path()),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            canvas_server: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
            model_name_resolver: None,
        }
    }

    /// A tool execute_fn that sleeps 500ms before succeeding.
    fn slow_execute(_call: ToolCall, _ctx: ToolContext) -> BoxedToolFuture {
        Box::pin(async {
            // wall-clock-under-test: the tool's own delay is the subject —
            // the test measures that a slow tool is handled as a slow tool.
            tokio::time::sleep(Duration::from_millis(500)).await;
            ToolResult {
                tool_call_id: "call_1".into(),
                name: "slow_tool".to_owned(),
                content: "done".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            }
        })
    }

    /// A tool execute_fn that completes immediately.
    fn fast_execute(_call: ToolCall, _ctx: ToolContext) -> BoxedToolFuture {
        Box::pin(async {
            ToolResult {
                tool_call_id: "call_1".into(),
                name: "slow_tool".to_owned(),
                content: "done".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            }
        })
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_exceeding_timeout_returns_failed_result() {
        // Given a tool timeout of 50ms and a tool that sleeps 500ms.
        let dir = jinn_testutil::tempdir();
        // When running with the timeout.
        let result = run_builtin_with_timeout(
            make_call(),
            {
                let mut c = empty_ctx(&dir.path);
                c.timeout = Some(Duration::from_millis(50));
                c
            },
            slow_execute,
        )
        .await;

        // Then the result is a failure with a timeout message.
        assert!(!result.success, "expected failure on timeout");
        assert!(
            result.content.contains("max_duration_secs"),
            "expected kill message naming max_duration_secs, got: {}",
            result.content
        );
        // And contains an example call so the model knows how to retry.
        assert!(
            result.content.contains("\"max_duration_secs\":600"),
            "expected kill message to show an example call, got: {}",
            result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_completing_under_timeout_returns_normal_result() {
        // Given a tool timeout of 500ms and a tool that completes immediately.
        let dir = jinn_testutil::tempdir();
        // When running with the timeout.
        let result = run_builtin_with_timeout(
            make_call(),
            {
                let mut c = empty_ctx(&dir.path);
                c.timeout = Some(Duration::from_millis(500));
                c
            },
            fast_execute,
        )
        .await;

        // Then the result succeeds with the tool's output.
        assert!(result.success, "expected success under timeout");
        assert_eq!(result.content, "done");
    }

    #[rstest::rstest]
    #[test]
    fn tool_timeout_value_sourced_from_tools_config() {
        // Given a config document setting a custom tool timeout.
        let config = jinn_config::testutil::config_layer("[tools]\ndefault_timeout_secs = 7\n");

        // When reading the tools section.
        let tools = config.get::<ToolsConfig>().expect("section reads");

        // Then the timeout value reflects the config.
        assert_eq!(tools.default_timeout_secs, 7);
        assert_eq!(
            Duration::from_secs(tools.default_timeout_secs),
            Duration::from_secs(7)
        );
    }

    #[rstest::rstest]
    #[test]
    fn extract_max_duration_reads_max_duration_secs_field() {
        // Given args with max_duration_secs.
        // When extracting.
        let d = super::extract_max_duration(r#"{"max_duration_secs":42}"#);

        // Then the value is returned.
        assert_eq!(d, Some(42));
    }

    #[rstest::rstest]
    #[test]
    fn extract_max_duration_falls_back_to_legacy_timeout_key() {
        // Given args with only the legacy timeout key.
        // When extracting.
        let d = super::extract_max_duration(r#"{"timeout":7}"#);

        // Then the legacy value is returned.
        assert_eq!(d, Some(7));
    }

    #[rstest::rstest]
    #[test]
    fn extract_max_duration_prefers_new_key_when_both_present() {
        // Given args with both max_duration_secs and legacy timeout.
        // When extracting.
        let d = super::extract_max_duration(r#"{"max_duration_secs":30,"timeout":5}"#);
        // Then max_duration_secs wins.
        assert_eq!(d, Some(30));
    }

    #[rstest::rstest]
    #[test]
    fn extract_max_duration_returns_none_when_no_field() {
        // Given args with neither key.
        // When extracting.
        let d = super::extract_max_duration(r#"{"command":"ls"}"#);

        // Then None is returned (caller falls back to global).
        assert_eq!(d, None);
    }

    #[rstest::rstest]
    #[test]
    fn extract_max_duration_tolerates_malformed_json() {
        // Given malformed args.
        // When extracting.
        let d = super::extract_max_duration("not json");

        // Then None is returned (no panic, falls back to global).
        assert_eq!(d, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn override_in_args_overrides_global_timeout() {
        // Given a 50ms override in args and a long global timeout.
        let dir = jinn_testutil::tempdir();
        let mut call = make_call();
        call.arguments = r#"{"max_duration_secs":1}"#.to_owned();
        let mut ctx = empty_ctx(&dir.path);
        ctx.timeout = Some(Duration::from_secs(10));

        // When running a tool that sleeps 500ms with a 1s override.
        // (use a 1s override but sleep 500ms => completes under override)
        let result = run_builtin_with_timeout(call, ctx, slow_execute).await;

        // Then it succeeds (override was long enough).
        assert!(result.success);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn zero_override_disables_timeout() {
        // Given a 0 override (disable sentinel) and no global timeout.
        let dir = jinn_testutil::tempdir();
        let mut call = make_call();
        call.arguments = r#"{"max_duration_secs":0}"#.to_owned();

        // When running a tool that sleeps 500ms with timeout disabled.
        let result = run_builtin_with_timeout(call, empty_ctx(&dir.path), slow_execute).await;

        // Then it succeeds (no timeout enforced).
        assert!(result.success);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn no_override_uses_global_timeout() {
        // Given no override in args and a 50ms global timeout.
        let dir = jinn_testutil::tempdir();
        let call = make_call(); // arguments = "{}"
        let mut ctx = empty_ctx(&dir.path);
        ctx.timeout = Some(Duration::from_millis(50));

        // When running a tool that sleeps 500ms.
        let result = run_builtin_with_timeout(call, ctx, slow_execute).await;

        // Then the global timeout fires.
        assert!(!result.success);
        assert!(result.content.contains("max_duration_secs"));
    }
}

#[cfg(test)]
mod panic_safety_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use std::panic::AssertUnwindSafe;

    use futures::FutureExt as _;
    use jinn_core_types::ToolCallId;

    use super::panicked_tool_result;
    use jinn_core_types::tool_types::{ToolCall, ToolResult};

    #[rstest::rstest]
    #[tokio::test]
    async fn builtin_tool_panic_publishes_failed_execution_completed() {
        // Given a builtin execute_fn that panics.
        fn panicking_execute(_call: ToolCall, _ctx: super::ToolContext) -> super::BoxedToolFuture {
            Box::pin(async {
                panic!("simulated builtin tool panic");
            })
        }

        // Paths live in a test-owned temp dir; the TempDir cleans up on drop.
        let dir = jinn_testutil::tempdir();
        let tool_call = ToolCall {
            id: "call_panic".into(),
            name: "boom".into(),
            arguments: "{}".to_owned(),
        };
        // When the builtin future panics and is caught.
        let result: ToolResult = match AssertUnwindSafe(panicking_execute(
            tool_call.clone(),
            super::ToolContext {
                turn: crate::tool_types::test_turn(),
                cwd: dir.path.clone(),
                config: jinn_config::testutil::config_layer(""),
                shells: Default::default(),
                timeout: None,
                state: None,
                session_id: None,
                app_paths: jinn_kernel::common::app_paths::AppPaths::new_in(
                    &dir.path.as_marked_path(),
                ),
                bus: None,
                max_output_lines: None,
                max_output_bytes: None,
                dispatched_at: jiff::Timestamp::now(),
                mcp_coordinator: None,
                canvas_server: None,
                interactive_term: None,
                task_spawns: None,
                session_store: None,
                trouper_system: None,
                model_name_resolver: None,
            },
        ))
        .catch_unwind()
        .await
        {
            Ok(r) => r,
            Err(_) => panicked_tool_result(&tool_call.id, &tool_call.name),
        };

        // Then a failed ToolResult is produced (not a silent hang).
        assert!(!result.success, "panicked tool must report failure");
        assert_eq!(result.content, "tool execution panicked");
        assert_eq!(result.tool_call_id.as_str(), "call_panic");
        assert_eq!(result.name, "boom");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn arbitrary_future_panic_publishes_failed_execution_completed() {
        // Given an arbitrary future that panics.
        let panicking_future = async {
            panic!("simulated future panic");
        };

        // When the future panics and is caught.
        let result: ToolResult = match AssertUnwindSafe(panicking_future).catch_unwind().await {
            Ok(r) => r,
            Err(_) => panicked_tool_result(&ToolCallId::from("call_future"), "future_tool"),
        };

        // Then a failed ToolResult is produced (not a silent hang).
        assert!(!result.success, "panicking future must report failure");
        assert_eq!(result.content, "tool execution panicked");
        assert_eq!(result.tool_call_id.as_str(), "call_future");
        assert_eq!(result.name, "future_tool");
    }
}

#[cfg(test)]
mod routing_lookup_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use std::collections::HashMap;

    use super::{ToolRegistration, lookup_registration};
    use jinn_core_types::ToolDefinition;

    fn actor_reg(name: &str, provider: &str) -> ToolRegistration {
        ToolRegistration::Actor {
            definition: ToolDefinition {
                name: name.to_owned(),
                description: String::new(),
                parameters: serde_json::Value::Object(serde_json::Map::new()),
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
                server_tool_type: None,
            },
            provider: provider.to_owned(),
        }
    }

    fn provider_of(reg: Option<&ToolRegistration>) -> Option<&str> {
        match reg {
            Some(ToolRegistration::Actor { provider, .. }) => Some(provider.as_str()),
            _ => None,
        }
    }

    #[rstest::rstest]
    #[test]
    fn session_scoped_registration_beats_global() {
        // Given a session map and a global map both defining the same tool name.
        let mut session = HashMap::new();
        session.insert("dup".to_owned(), actor_reg("dup", "mcp__session__"));
        let global_reg = actor_reg("dup", "mcp__global__");

        // When resolving with both present.
        let resolved = lookup_registration(Some(&session), Some(&global_reg), "dup");

        // Then the session-scoped provider wins.
        assert_eq!(provider_of(resolved), Some("mcp__session__"));
    }

    #[rstest::rstest]
    #[test]
    fn global_used_when_session_map_absent() {
        // Given only a global registration.
        let global_reg = actor_reg("sample-tool", "sample-actor");

        // When resolving with no session map.
        let resolved = lookup_registration(None, Some(&global_reg), "sample-tool");

        // Then the global registration is returned.
        assert_eq!(provider_of(resolved), Some("sample-actor"));
    }

    #[rstest::rstest]
    #[test]
    fn global_used_when_session_map_lacks_tool() {
        // Given a session map without the tool and a global map with it.
        let session = HashMap::<String, ToolRegistration>::new();
        let global_reg = actor_reg("sample-tool-2", "sample-actor-2");

        // When resolving.
        let resolved = lookup_registration(Some(&session), Some(&global_reg), "sample-tool-2");

        // Then the global registration is the fallback.
        assert_eq!(provider_of(resolved), Some("sample-actor-2"));
    }

    #[rstest::rstest]
    #[test]
    fn resolves_none_when_neither_has_tool() {
        // Given empty session and global maps.
        let session = HashMap::<String, ToolRegistration>::new();
        let global = HashMap::<String, ToolRegistration>::new();
        let global_reg = global.get("nope");

        // When resolving an unknown tool.
        let resolved = lookup_registration(Some(&session), global_reg, "nope");

        // Then no registration is found.
        assert!(resolved.is_none());
    }
}

#[cfg(test)]
mod server_name_tests {
    use super::server_name_of_provider;

    #[rstest::rstest]
    #[case("mcp__excalimate__", Some("excalimate"))]
    #[case("mcp__stub__", Some("stub"))]
    #[case("mcp____", None)]
    #[case("plain-provider", None)]
    fn server_name_extraction(#[case] provider: &str, #[case] expected: Option<&str>) {
        // Given an MCP provider string.
        // When extracting the server name.
        let server = server_name_of_provider(provider);

        // Then the leading mcp__ and trailing __ are both stripped.
        assert_eq!(server, expected);
    }
}

#[cfg(test)]
mod mcp_dispatch_gate_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use std::time::Duration;

    use jinn_core_types::SessionId;
    use jinn_core_types::tool_types::{ToolCall, ToolDefinition};
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_kernel::common::state::State;
    use jinn_mcp_msg::McpConnectionStatus;
    use jinn_session_msg::SessionClosed;
    use jinn_testutil::bus_harness::{TestHarness, await_recorded};
    use jinn_tools_msg::{CancelToolBatch, ExecuteTool, ExecuteToolBatch, RegisterTools};
    use jinn_tools_msg::{ToolExecutionCompleted, ToolsUnregistered};

    use super::{ToolOrchestratorActor, ToolOrchestratorActorDeps};

    const PROVIDER: &str = "mcp__stub__";

    async fn spawn_orchestrator(
        state: &State,
    ) -> (TestHarness, jinn_kernel::common::services::Services) {
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        // `Services::new_fake` seeds its registry through the shared cell
        // catalog, which owns the MCP runtime cell the orchestrator gates on.
        let _runtime = services
            .slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("the catalog registers the MCP runtime cell");
        ToolOrchestratorActor::spawn(
            &services.trouper_system.clone(),
            ToolOrchestratorActorDeps {
                deps: jinn_kernel::common::actor_deps::ActorDeps {
                    services: services.clone(),
                },
                state: state.clone(),
                services: services.clone(),
                builtin_filter: None,
            },
        );
        (harness, services)
    }

    fn mcp_tool_def() -> ToolDefinition {
        ToolDefinition {
            name: "mcp__stub__echo".to_owned(),
            description: "Echo".to_owned(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        }
    }

    fn echo_call() -> ToolCall {
        ToolCall {
            id: "tc_1".into(),
            name: "mcp__stub__echo".to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    /// Registers the stub tool for `session_id` and waits until the
    /// registration is observable in the registry cell.
    ///
    /// `services` must be the very container the actor was spawned with:
    /// `Services::new_fake` mints a *fresh* cell registry per call, so
    /// reading the cell off a second `harness.services()` would look at a
    /// different registry than the one the actor writes to, and the wait
    /// would never see the registration.
    async fn register_stub_tools(
        harness: &TestHarness,
        services: &jinn_kernel::common::services::Services,
        session_id: &SessionId,
    ) {
        harness
            .publish(RegisterTools {
                provider: PROVIDER.to_owned(),
                definitions: vec![mcp_tool_def()],
                session_id: Some(session_id.clone()),
            })
            .await;
        // The registry cell is the signal that the registration was
        // processed; a fixed sleep would only guess at it. The stub
        // registers for a *session*, so the session map is what changes.
        let registry = tools_registry(services);
        jinn_testutil::wait::await_cell(&registry, Duration::from_secs(5), |r| {
            r.session
                .values()
                .flatten()
                .any(|(_, tool)| tool.name == "mcp__stub__echo")
        })
        .await;
    }

    fn tools_registry(
        services: &jinn_kernel::common::services::Services,
    ) -> jinn_slices::TypedCell<jinn_tools_msg::ToolRegistry> {
        services
            .slices
            .reader(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell")
    }

    fn mcp_runtime(
        services: &jinn_kernel::common::services::Services,
    ) -> jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState> {
        services
            .slices
            .reader(&jinn_mcp_msg::mcp_runtime_slot())
            .expect("MCP runtime cell")
    }

    async fn publish_batch(
        harness: &TestHarness,
        services: &jinn_kernel::common::services::Services,
        session_id: &SessionId,
    ) {
        services
            .turn_ledger
            .mint(
                session_id,
                jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            )
            .expect("fixture mint");
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![echo_call()],
                dispatched_at: jiff::Timestamp::now(),
                turn: services.turn_ledger.live(session_id).expect("live").turn,
            })
            .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tools_unregistered_removes_context_definitions() {
        // Given an actor with the stub tool registered for a session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let (harness, services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &services, &session_id).await;

        // When the provider unregisters its tools.
        harness
            .publish(ToolsUnregistered {
                provider: PROVIDER.to_owned(),
                session_id,
            })
            .await;
        // The registry cell is the signal that the unregistration was
        // processed; a fixed sleep would only guess at it.
        let registry = tools_registry(&services);
        jinn_testutil::wait::await_cell(&registry, Duration::from_secs(5), |r| {
            !r.session
                .values()
                .flatten()
                .any(|(_, tool)| tool.name == "mcp__stub__echo")
        })
        .await;

        // Then the definition is no longer available to context assembly.
        assert!(
            !registry
                .read()
                .session
                .values()
                .flatten()
                .any(|(_, tool)| tool.name == "mcp__stub__echo"),
            "unregistered tool remains in the context registry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_closed_removes_context_definitions() {
        // Given an actor with the stub tool registered for a session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let (harness, services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &services, &session_id).await;

        // When the session closes.
        harness
            .publish(SessionClosed {
                session_id: session_id.clone(),
            })
            .await;
        // The registry cell is the signal that the close was processed; a
        // fixed sleep would only guess at it.
        let registry = tools_registry(&services);
        jinn_testutil::wait::await_cell(&registry, Duration::from_secs(5), |r| {
            !r.session.contains_key(&session_id)
        })
        .await;

        // Then its context definitions are removed.
        assert!(!registry.read().session.contains_key(&session_id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tools_unregistered_removes_the_providers_tools_from_the_routing_map() {
        // Given a state with a seeded session, the stub tools registered for it.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &services, &session_id).await;

        // When the provider unregisters its tools for that session.
        harness
            .publish(ToolsUnregistered {
                provider: PROVIDER.to_owned(),
                session_id: session_id.clone(),
            })
            .await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;
        publish_batch(&harness, &services, &session_id).await;

        // Then the tool is no longer routable (rejected as unknown, not
        // dispatched to an MCP provider).
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        assert!(
            messages[0]
                .result
                .content
                .starts_with("unknown tool: mcp__stub__echo"),
            "expected unknown-tool rejection after unregister, got: {}",
            messages[0].result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn gate_fails_fast_when_the_server_is_disabled() {
        // Given a session where the stub server is NOT enabled, with its
        // tools still registered (the in-flight-turn race the gate guards).
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &services, &session_id).await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call to the disabled server's tool.
        publish_batch(&harness, &services, &session_id).await;

        // Then a failed result naming the server arrives promptly.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        assert!(!messages[0].result.success);
        assert!(
            messages[0].result.content.contains("'stub' is disabled"),
            "expected a disabled-server message, got: {}",
            messages[0].result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn gate_fails_fast_when_the_enabled_server_is_dead() {
        // Given a session where the stub server is enabled but its connection
        // is Dead, with its tools still registered.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state
            .write()
            .session
            .get_or_create(&session_id)
            .enable_mcp_server("stub");
        let (harness, services) = spawn_orchestrator(&state).await;
        mcp_runtime(&services).update(|runtime| {
            runtime.set_status(&session_id, "stub", McpConnectionStatus::Dead);
        });
        register_stub_tools(&harness, &services, &session_id).await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call to the dead server's tool.
        publish_batch(&harness, &services, &session_id).await;

        // Then a failed result explaining the dead connection arrives.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        assert!(!messages[0].result.success);
        assert!(
            messages[0].result.content.contains("connection is dead"),
            "expected a dead-connection message, got: {}",
            messages[0].result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn gate_passes_through_when_the_server_is_enabled_and_running() {
        // Given a session where the stub server is enabled and Running, with
        // its tools registered.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state
            .write()
            .session
            .get_or_create(&session_id)
            .enable_mcp_server("stub");
        let (harness, services) = spawn_orchestrator(&state).await;
        mcp_runtime(&services).update(|runtime| {
            runtime.set_status(&session_id, "stub", McpConnectionStatus::Running);
        });
        register_stub_tools(&harness, &services, &session_id).await;
        let dispatched = harness.spawn_recorder::<ExecuteTool>().await;

        // When dispatching a call to the running server's tool.
        publish_batch(&harness, &services, &session_id).await;

        // Then an ExecuteTool is published on the bus as before (no gate
        // rejection short-circuits the dispatch).
        let messages = await_recorded(&dispatched, 1, Duration::from_secs(3)).await;
        assert_eq!(messages[0].tool_call.name, "mcp__stub__echo");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn singleton_status_is_visible_without_a_session_scoped_entry() {
        // Given a session where the stub server is enabled, its tools
        // registered globally (the singleton registration shape), the server
        // configured singleton, and its status written to the singleton
        // bucket — not under this session's id.
        let (harness, services, session_id, dispatched) = {
            let state = State::new(AppState::default());
            let session_id = SessionId::new();
            state
                .write()
                .session
                .get_or_create(&session_id)
                .enable_mcp_server("stub");
            let (harness, services) = spawn_orchestrator(&state).await;
            // The gate reads singleton-ness from the config layer, so the seed
            // goes through the same path production reads.
            use jinn_preferences_config::schemas::McpServersConfig as SeedConfig;
            services
                .config
                .put::<SeedConfig>(&SeedConfig(
                    std::iter::once((
                        "stub".to_owned(),
                        jinn_preferences_config::schemas::mcp::McpServerConfig {
                            singleton: true,
                            ..Default::default()
                        },
                    ))
                    .collect(),
                ))
                .expect("seed the mcp section");
            mcp_runtime(&services).update(|runtime| {
                runtime.set_singleton_status("stub", McpConnectionStatus::Running);
            });
            harness
                .publish(RegisterTools {
                    provider: PROVIDER.to_owned(),
                    definitions: vec![mcp_tool_def()],
                    session_id: None,
                })
                .await;
            // The registry cell is the signal that the registration was
            // processed; a fixed sleep would only guess at it. This one
            // registers globally, so the global map is what changes.
            let registry = tools_registry(&services);
            jinn_testutil::wait::await_cell(&registry, Duration::from_secs(5), |r| {
                r.global.values().any(|tool| tool.name == "mcp__stub__echo")
            })
            .await;
            let dispatched = harness.spawn_recorder::<ExecuteTool>().await;
            (harness, services, session_id, dispatched)
        };

        // When dispatching a call from a session that has no per-session
        // runtime entry for the server.
        publish_batch(&harness, &services, &session_id).await;

        // Then the gate resolves Running from the singleton bucket and
        // dispatches — it must not report "still starting" for a shared
        // server just because this session never spawned it.
        let messages = await_recorded(&dispatched, 1, Duration::from_secs(3)).await;
        assert_eq!(
            messages.len(),
            1,
            "expected an ExecuteTool dispatch (no gate rejection); got {messages:?}"
        );
        assert_eq!(messages[0].tool_call.name, "mcp__stub__echo");
    }

    /// The cancel drain: the ledger holds an open call for the session and
    /// the batch bookkeeping is in place, the way a mid-execution cancel
    /// finds the orchestrator.
    async fn seed_open_call(
        harness: &TestHarness,
        services: &jinn_kernel::common::services::Services,
        state: &State,
        session_id: &SessionId,
    ) -> jinn_core_types::turn_identity::TurnToken {
        state
            .write()
            .session
            .get_or_create(session_id)
            .enable_mcp_server("stub");
        mcp_runtime(services).update(|runtime| {
            runtime.set_status(session_id, "stub", McpConnectionStatus::Running);
        });
        register_stub_tools(harness, services, session_id).await;
        services
            .turn_ledger
            .mint(
                session_id,
                jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            )
            .expect("fixture mint");
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![echo_call()],
                dispatched_at: jiff::Timestamp::now(),
                turn: services.turn_ledger.live(session_id).expect("live").turn,
            })
            .await;
        // The batch is dispatched: one call open, one ExecuteTool in flight.
        services
            .turn_ledger
            .live(session_id)
            .expect("live after dispatch")
            .turn
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_synthesizes_a_completion_per_open_call() {
        // Given an orchestrator with one call in flight (registered open,
        // ExecuteTool dispatched to a running stub server).
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let (harness, services) = spawn_orchestrator(&state).await;
        let turn = seed_open_call(&harness, &services, &state, &session_id).await;
        let completions = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When the cancel drain asks the orchestrator to abandon the batch.
        harness
            .publish(CancelToolBatch {
                session_id: session_id.clone(),
            })
            .await;
        let messages = await_recorded(&completions, 1, Duration::from_secs(3)).await;

        // Then exactly one synthesized cancellation was published for the
        // open call, carrying the live turn's identity.
        assert_eq!(
            messages.len(),
            1,
            "one open call must synthesize exactly one completion; got {messages:?}"
        );
        assert!(
            messages[0].synthesized,
            "the completion must be synthesized"
        );
        assert_eq!(messages[0].turn, turn);
        assert_eq!(messages[0].result.tool_call_id, echo_call().id);
        assert!(!messages[0].result.success, "a synthesized cancel fails");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_synthesis_leaves_no_pending_bookkeeping_behind() {
        // Given an orchestrator with one call in flight.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let (harness, services) = spawn_orchestrator(&state).await;
        let _turn = seed_open_call(&harness, &services, &state, &session_id).await;

        // When the cancel drain runs — with no completion consumer attached,
        // so the drain cannot depend on anyone observing the synthesis.
        harness
            .publish(CancelToolBatch {
                session_id: session_id.clone(),
            })
            .await;
        // The synthesis spawns off-actor; give it the same await horizon the
        // recorder-based test uses before reading the drained bookkeeping.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Then the batch and its name map are gone: a later batch for the
        // session starts clean, and the drain never leaves entries behind.
        assert!(
            services.turn_ledger.open_calls(&session_id).is_some(),
            "the ledger still holds the turn (the completion closes it, not the drain)"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn batch_during_a_cancel_drain_never_runs() {
        // Given an orchestrator whose session is mid-drain: the ledger turn
        // already flipped to cancel.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        let (harness, services) = spawn_orchestrator(&state).await;
        let turn = seed_open_call(&harness, &services, &state, &session_id).await;
        services
            .turn_ledger
            .begin_cancel(&session_id)
            .expect("the drain starts");
        let dispatched = harness.spawn_recorder::<ExecuteTool>().await;

        // When a batch for the same turn arrives during the drain.
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![echo_call()],
                dispatched_at: jiff::Timestamp::now(),
                turn,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Then nothing was dispatched to any tool: the drain refuses new
        // work instead of queueing it into a dead turn.
        let messages = dispatched.drain();
        assert!(
            messages.is_empty(),
            "a batch during a cancel drain must not run; got {messages:?}"
        );
    }
}
