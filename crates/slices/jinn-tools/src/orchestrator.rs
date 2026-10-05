//! Tool orchestrator actor - dispatches tool calls and aggregates batch results.
//!
//! This actor maintains a registry of available tools (built-in and actor-provided),
//! dispatches [`ExecuteToolBatch`] requests, and emits [`ToolBatchCompleted`] when
//! all calls in a batch finish.
//!
//! Built-in tools (`get_time`, `read`, `write`) are registered at
//! startup and executed via spawned tokio tasks. Actor-provided tools
//! are routed via [`ExecuteTool`] commands on the bus.
//!
//! Each tool execution receives a [`ToolContext`] containing the session's CWD
//! (for resolving relative paths) and an optional timeout. The orchestrator
//! reads CWD from shared [`State`] at dispatch time.

use jinn_preferences_config::schemas::ToolsConfig;
use jinn_preferences_config::schemas::WebSearchConfig;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::tool_types::ToolContext;
use jiff::Timestamp;
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
    ToolBatchCompleted, ToolExecutionCompleted, ToolsRegistered, ToolsUnregistered,
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
        /// by a dropped future (`task` would orphan its child session).
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
    /// Number of tool calls still awaiting results.
    remaining: usize,
    /// Collected results so far.
    results: Vec<ToolResult>,
    /// Join handles for spawned builtin tool tasks (for cancellation).
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
    async fn start(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `start_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("ToolOrchestratorActor spawns via start_with"))
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
            .start_with({
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
            // `bash` is the one builtin whose schema names a config value
            // (the default timeout), so it is the one that re-resolves.
            let live_definition = (name == "bash").then_some(
                (|config: &jinn_config::ConfigLayer| {
                    let tools = config.get::<ToolsConfig>().unwrap_or_default();
                    crate::bash::definition(tools.default_timeout_secs)
                }) as fn(&jinn_config::ConfigLayer) -> ToolDefinition,
            );
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
                        tool_call_id: String::new(),
                        name: "openrouter:web_search".to_owned(),
                        content: "server tool should not be dispatched".to_owned(),
                        success: false,
                        full_content: None,
                        truncation: None,
                        pin_position: None,
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
        self.handle_tool_execution_completed(msg.session_id.clone(), msg.result.clone())
            .await;
    }
}

impl MsgHandler<SessionClosed> for ToolOrchestratorActor {
    async fn handle(&mut self, msg: &SessionClosed, _ctx: &mut MsgCtx<'_>) {
        // Drop per-session routing and context definitions so neither map leaks.
        self.session_tools.remove(&msg.session_id);
        self.remove_cached_session_tools(&msg.session_id);
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
    async fn handle_execute_tool_batch(
        &mut self,
        session_id: SessionId,
        tool_calls: Vec<ToolCall>,
        dispatched_at: Timestamp,
    ) {
        tracing::info!(
            session_id = %session_id,
            tools = ?tool_calls.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            "handle_execute_tool_batch"
        );

        if tool_calls.is_empty() {
            self.publish(ToolBatchCompleted {
                session_id,
                results: vec![],
            })
            .await;
            return;
        }

        let remaining = tool_calls.len();
        let mut handles = Vec::new();
        for tc in tool_calls {
            if let Some(handle) = self
                .dispatch_tool_call(session_id.clone(), tc, dispatched_at)
                .await
            {
                handles.push(handle);
            }
        }
        self.pending.insert(
            session_id.clone(),
            PendingBatch {
                remaining,
                results: vec![],
                handles,
            },
        );
    }

    /// Cancels all pending tool executions for a session.
    ///
    /// Aborts spawned builtin tasks and removes the pending batch.
    /// Any `ToolExecutionCompleted` events that already arrived for this
    /// session will be ignored (the pending batch is gone).
    fn handle_cancel_tool_batch(&mut self, session_id: &SessionId) {
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
    fn build_tool_context(&self, session_id: &SessionId, dispatched_at: Timestamp) -> ToolContext {
        let cwd = {
            let guard = self.state.read();
            guard.session.get(session_id).map_or_else(
                || guard.session.default_cwd().clone(),
                |session| session.cwd().to_owned(),
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
            state: Some(self.state.clone()),
            config: self.services.config.clone(),
            session_id: Some(session_id.clone()),
            app_paths: self.services.paths.clone(),
            bus: Some(self.bus().clone()),
            max_output_lines,
            max_output_bytes,
            dispatched_at,
            mcp_coordinator: self.services.mcp_coordinator.get().cloned(),
            interactive_term: self.services.interactive_term.get().cloned(),
            task_spawns: Some(self.services.task_spawns.clone()),
            session_store: Some(self.services.session_store.clone()),
            trouper_system: Some(self.services.trouper_system.clone()),
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
            return self.reject_withheld_tool(session_id, tool_call).await;
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
                *execute,
                *self_managed_timeout,
            )),
            Some(ToolRegistration::Actor { provider, .. }) => {
                self.dispatch_actor(session_id, tool_call, provider, dispatched_at)
                    .await
            }
            None => self.reject_unknown_tool(session_id, tool_call).await,
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
        };

        self.publish(ToolExecutionCompleted { session_id, result })
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
        execute_fn: fn(ToolCall, ToolContext) -> BoxedToolFuture,
        self_managed_timeout: bool,
    ) -> tokio::task::JoinHandle<()> {
        let bus = self.bus().clone();
        let tool_ctx = self.build_tool_context(&session_id, dispatched_at);

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
            bus.publish(ToolExecutionCompleted { session_id, result })
                .await;
        })
    }

    /// Routes an actor-backed tool to its provider's command.
    ///
    /// MCP providers (`mcp__*`) are delivered via the generic
    /// [`ExecuteTool`] command to whichever MCP client actor owns the
    /// matching session-scoped connection.
    async fn dispatch_actor(
        &self,
        session_id: SessionId,
        tool_call: ToolCall,
        provider: &str,
        dispatched_at: Timestamp,
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
                    session_id,
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
    ) -> Option<tokio::task::JoinHandle<()>> {
        let result = ToolResult {
            tool_call_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            content: format!("unknown tool: {}", tool_call.name),
            success: false,
            full_content: None,
            truncation: None,
            pin_position: None,
        };

        self.publish(ToolExecutionCompleted { session_id, result })
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
        let runtime = self
            .services
            .slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot());
        let status = runtime
            .as_ref()
            .and_then(|runtime| runtime.read().status(session_id, server));
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

    /// Aggregates a tool result into the pending batch.
    ///
    /// When all calls in a batch have completed, emits [`ToolBatchCompleted`]
    async fn handle_tool_execution_completed(&mut self, session_id: SessionId, result: ToolResult) {
        let Some(batch) = self.pending.get_mut(&session_id) else {
            tracing::warn!(
                session_id = ?session_id,
                tool = %result.name,
                tool_call_id = %result.tool_call_id,
                "handle_tool_execution_completed — no pending batch, dropping result"
            );
            return;
        };

        batch.remaining -= 1;
        batch.results.push(result);

        tracing::info!(
            session_id = ?session_id,
            tool = %batch.results.last().map_or("?", |r| r.name.as_str()),
            remaining = batch.remaining,
            "handle_tool_execution_completed"
        );

        if batch.remaining == 0 {
            let results = self
                .pending
                .remove(&session_id)
                .map(|b| b.results)
                .unwrap_or_default();

            tracing::info!(
                session_id = ?session_id,
                result_count = results.len(),
                "emitting ToolBatchCompleted"
            );

            self.publish(ToolBatchCompleted {
                session_id,
                results,
            })
            .await;
        }
    }
}

/// Constructs a failed [`ToolResult`] for a tool that panicked.
///
/// Ensures a panicking tool still publishes a completion so the batch always finishes.
fn panicked_tool_result(tool_call_id: &str, name: &str) -> ToolResult {
    tracing::error!(tool = %name, tool_call_id = %tool_call_id, "tool execution panicked");
    ToolResult {
        tool_call_id: tool_call_id.to_owned(),
        name: name.to_owned(),
        content: "tool execution panicked".to_owned(),
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
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
            },
        },
        None => execute_fn(tool_call, tool_ctx).await,
    }
}

#[cfg(test)]
mod timeout_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{BoxedToolFuture, ToolContext, run_builtin_with_timeout};
    use jinn_core_types::tool_types::{ToolCall, ToolResult};
    use jinn_kernel::common::app_paths::AppPaths;
    use jinn_preferences_config::schemas::ToolsConfig;

    fn make_call() -> ToolCall {
        ToolCall {
            id: "call_1".to_owned(),
            name: "slow_tool".to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    fn empty_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            config: jinn_config::testutil::config_layer(""),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
            trouper_system: None,
        }
    }

    /// A tool execute_fn that sleeps 500ms before succeeding.
    fn slow_execute(_call: ToolCall, _ctx: ToolContext) -> BoxedToolFuture {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            ToolResult {
                tool_call_id: "call_1".to_owned(),
                name: "slow_tool".to_owned(),
                content: "done".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }
        })
    }

    /// A tool execute_fn that completes immediately.
    fn fast_execute(_call: ToolCall, _ctx: ToolContext) -> BoxedToolFuture {
        Box::pin(async {
            ToolResult {
                tool_call_id: "call_1".to_owned(),
                name: "slow_tool".to_owned(),
                content: "done".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }
        })
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_exceeding_timeout_returns_failed_result() {
        // Given a tool timeout of 50ms and a tool that sleeps 500ms.
        // When running with the timeout.
        let result = run_builtin_with_timeout(
            make_call(),
            {
                let mut c = empty_ctx();
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
        // When running with the timeout.
        let result = run_builtin_with_timeout(
            make_call(),
            {
                let mut c = empty_ctx();
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
        let mut call = make_call();
        call.arguments = r#"{"max_duration_secs":1}"#.to_owned();
        let mut ctx = empty_ctx();
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
        let mut call = make_call();
        call.arguments = r#"{"max_duration_secs":0}"#.to_owned();

        // When running a tool that sleeps 500ms with timeout disabled.
        let result = run_builtin_with_timeout(call, empty_ctx(), slow_execute).await;

        // Then it succeeds (no timeout enforced).
        assert!(result.success);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn no_override_uses_global_timeout() {
        // Given no override in args and a 50ms global timeout.
        let call = make_call(); // arguments = "{}"
        let mut ctx = empty_ctx();
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

        let tool_call = ToolCall {
            id: "call_panic".to_owned(),
            name: "boom".to_owned(),
            arguments: "{}".to_owned(),
        };
        // When the builtin future panics and is caught.
        let result: ToolResult = match AssertUnwindSafe(panicking_execute(
            tool_call.clone(),
            super::ToolContext {
                cwd: std::path::PathBuf::from("/tmp"),
                config: jinn_config::testutil::config_layer(""),
                timeout: None,
                state: None,
                session_id: None,
                app_paths: jinn_kernel::common::app_paths::AppPaths::new_in(std::path::Path::new(
                    "/tmp",
                )),
                bus: None,
                max_output_lines: None,
                max_output_bytes: None,
                dispatched_at: jiff::Timestamp::now(),
                mcp_coordinator: None,
                interactive_term: None,
                task_spawns: None,
                session_store: None,
                trouper_system: None,
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
        assert_eq!(result.tool_call_id, "call_panic");
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
            Err(_) => panicked_tool_result("call_future", "future_tool"),
        };

        // Then a failed ToolResult is produced (not a silent hang).
        assert!(!result.success, "panicking future must report failure");
        assert_eq!(result.content, "tool execution panicked");
        assert_eq!(result.tool_call_id, "call_future");
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
    use jinn_tools_msg::{ExecuteTool, ExecuteToolBatch, RegisterTools};
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
            id: "tc_1".to_owned(),
            name: "mcp__stub__echo".to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    async fn register_stub_tools(harness: &TestHarness, session_id: &SessionId) {
        harness
            .publish(RegisterTools {
                provider: PROVIDER.to_owned(),
                definitions: vec![mcp_tool_def()],
                session_id: Some(session_id.clone()),
            })
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
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

    async fn publish_batch(harness: &TestHarness, session_id: &SessionId) {
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![echo_call()],
                dispatched_at: jiff::Timestamp::now(),
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
        register_stub_tools(&harness, &session_id).await;

        // When the provider unregisters its tools.
        harness
            .publish(ToolsUnregistered {
                provider: PROVIDER.to_owned(),
                session_id,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Then the definition is no longer available to context assembly.
        let registry = tools_registry(&services);
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
        register_stub_tools(&harness, &session_id).await;

        // When the session closes.
        harness
            .publish(SessionClosed {
                session_id: session_id.clone(),
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Then its context definitions are removed.
        let registry = tools_registry(&services);
        assert!(!registry.read().session.contains_key(&session_id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tools_unregistered_removes_the_providers_tools_from_the_routing_map() {
        // Given a state with a seeded session, the stub tools registered for it.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, _services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &session_id).await;

        // When the provider unregisters its tools for that session.
        harness
            .publish(ToolsUnregistered {
                provider: PROVIDER.to_owned(),
                session_id: session_id.clone(),
            })
            .await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;
        publish_batch(&harness, &session_id).await;

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
        let (harness, _services) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &session_id).await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call to the disabled server's tool.
        publish_batch(&harness, &session_id).await;

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
        register_stub_tools(&harness, &session_id).await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call to the dead server's tool.
        publish_batch(&harness, &session_id).await;

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
        register_stub_tools(&harness, &session_id).await;
        let dispatched = harness.spawn_recorder::<ExecuteTool>().await;

        // When dispatching a call to the running server's tool.
        publish_batch(&harness, &session_id).await;

        // Then an ExecuteTool is published on the bus as before (no gate
        // rejection short-circuits the dispatch).
        let messages = await_recorded(&dispatched, 1, Duration::from_secs(3)).await;
        assert_eq!(messages[0].tool_call.name, "mcp__stub__echo");
    }

    /// The stream-rules cell, which the catalog registers.
    fn rules_cell(
        services: &jinn_kernel::common::services::Services,
    ) -> jinn_slices::TypedCell<jinn_slices::StreamRules> {
        services
            .slices
            .reader::<jinn_slices::StreamRules>(&jinn_slices::stream_rules_slot())
            .expect("the catalog registers the stream-rules cell")
    }

    /// A `fail_tool` rule denying `condition` on `tool`.
    fn denial_rule(
        name: &str,
        condition: &str,
        tool: &str,
        project: Option<&str>,
    ) -> jinn_preferences_config::schemas::StreamRuleConfig {
        jinn_preferences_config::schemas::StreamRuleConfig {
            name: name.to_owned(),
            description: format!("denies {condition}"),
            conditions: vec![condition.to_owned()],
            scopes: vec![format!("tool:{tool}")],
            body: "Use a narrower delete.".to_owned(),
            on_trigger: Some(jinn_preferences_config::schemas::FAIL_TOOL_TRIGGER.to_owned()),
            project: project.map(str::to_owned),
        }
    }

    /// Installs `rules` as the session's rule set.
    fn install_rules(
        services: &jinn_kernel::common::services::Services,
        rules: &[jinn_preferences_config::schemas::StreamRuleConfig],
    ) {
        rules_cell(services).update(|payload| {
            payload.install(std::sync::Arc::new(
                jinn_stream_rules::matcher::CompiledSet::build(rules),
            ));
        });
    }

    /// Installs a `fail_tool` rule denying `condition` on the `bash` tool.
    fn install_denial_rule(services: &jinn_kernel::common::services::Services, condition: &str) {
        install_rules(services, &[denial_rule("no-rm", condition, "bash", None)]);
    }

    /// A session rooted at `cwd`, with the orchestrator spawned against it.
    async fn session_at(
        cwd: &std::path::Path,
    ) -> (
        State,
        SessionId,
        TestHarness,
        jinn_kernel::common::services::Services,
    ) {
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state
            .write()
            .session
            .get_or_create(&session_id)
            .set_cwd(cwd.to_path_buf());
        let (harness, services) = spawn_orchestrator(&state).await;
        (state, session_id, harness, services)
    }

    /// Dispatches a bash call running `command`.
    async fn dispatch_bash(harness: &TestHarness, session_id: &SessionId, command: &str) {
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![ToolCall {
                    id: "tc_1".to_owned(),
                    name: "bash".to_owned(),
                    arguments: serde_json::json!({ "command": command }).to_string(),
                }],
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_denied_call_returns_a_failed_result() {
        // Given a rule denying a command, and a session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, services) = spawn_orchestrator(&state).await;
        install_denial_rule(&services, "rm -rf");
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call the rule denies.
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![ToolCall {
                    id: "tc_deny".to_owned(),
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"rm -rf /"}"#.to_owned(),
                }],
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;

        // Then the result is a failure naming the rule.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        let first = messages.first().expect("a result is published");
        assert!(!first.result.success);
        assert!(
            first.result.content.contains("no-rm"),
            "the failure must name the rule, got: {}",
            first.result.content
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_denied_call_never_reaches_the_tool() {
        // Given a rule denying a command, and a session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, services) = spawn_orchestrator(&state).await;
        install_denial_rule(&services, "rm -rf");
        let started = harness
            .spawn_recorder::<jinn_tools_msg::ToolExecutionStarted>()
            .await;
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When dispatching a call the rule denies.
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![ToolCall {
                    id: "tc_deny".to_owned(),
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"rm -rf /"}"#.to_owned(),
                }],
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;

        // Then nothing was ever started for it: no execution event, which is
        // the observable that a denial happened before any child could spawn.
        await_recorded(&results, 1, Duration::from_secs(3)).await;
        assert!(
            started.is_empty(),
            "a denied call must emit no ToolExecutionStarted, so no child process can exist"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_call_the_rule_does_not_match_is_dispatched_normally() {
        // Given a rule denying a command, and a session.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write().session.get_or_create(&session_id);
        let (harness, services) = spawn_orchestrator(&state).await;
        install_denial_rule(&services, "rm -rf");

        // When dispatching a builtin call the rule does not match.
        let started = harness
            .spawn_recorder::<jinn_tools_msg::ToolExecutionStarted>()
            .await;
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![ToolCall {
                    id: "tc_ok".to_owned(),
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"ls -la"}"#.to_owned(),
                }],
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;

        // Then it actually starts -- the gate let it through, which is the
        // whole point: a rule must not deny what it does not match.
        let messages = await_recorded(&started, 1, Duration::from_secs(3)).await;
        assert_eq!(
            messages.first().map(|m| m.tool_call_id.as_str()),
            Some("tc_ok")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_project_scoped_rule_denies_a_call_in_that_project() {
        // Given a rule scoped to one project, and a session inside it.
        let (_state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/code/myapp")).await;
        install_rules(
            &services,
            &[denial_rule("scoped", "rm -rf", "bash", Some("myapp"))],
        );
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When a matching call is dispatched there.
        dispatch_bash(&harness, &session_id, "rm -rf /").await;

        // Then it is denied.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        let first = messages.first().expect("a result is published");
        assert!(
            !first.result.success,
            "the call must be denied in its project"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_project_scoped_rule_does_not_deny_a_call_elsewhere() {
        // Given a rule scoped to one project, and a session inside another.
        let (_state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/code/other")).await;
        install_rules(
            &services,
            &[denial_rule("scoped", "rm -rf", "bash", Some("myapp"))],
        );
        let started = harness
            .spawn_recorder::<jinn_tools_msg::ToolExecutionStarted>()
            .await;

        // When the same call is dispatched there.
        dispatch_bash(&harness, &session_id, "rm -rf /").await;

        // Then it runs, because the rule does not apply to this project.
        let messages = await_recorded(&started, 1, Duration::from_secs(3)).await;
        assert_eq!(
            messages.len(),
            1,
            "the call must start outside the rule's project"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_global_rule_denies_a_call_in_a_project_that_scopes_another_rule() {
        // Given a global rule beside one scoped to a single project.
        let (_state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/elsewhere")).await;
        install_rules(
            &services,
            &[
                denial_rule("global", "rm -rf", "bash", None),
                denial_rule("scoped", "curl", "bash", Some("myapp")),
            ],
        );
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When the global rule's command runs there.
        dispatch_bash(&harness, &session_id, "rm -rf /").await;

        // Then it is denied: scoping the other rule cannot lift this one.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        let first = messages.first().expect("a result is published");
        assert!(!first.result.success, "a global rule is a floor");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_project_rule_adds_a_block_a_global_rule_does_not_carry() {
        // Given a global rule about one command, beside a project rule about another.
        let (_state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/code/myapp")).await;
        install_rules(
            &services,
            &[
                denial_rule("global", "rm -rf", "bash", None),
                denial_rule("scoped", "curl", "bash", Some("myapp")),
            ],
        );
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When the project rule's command runs there.
        dispatch_bash(&harness, &session_id, "curl http://x").await;

        // Then it is denied too, so a project rule can add blocks.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        let first = messages.first().expect("a result is published");
        assert!(!first.result.success, "the project rule adds its own block");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_project_rules_set_is_resolved_once_and_reused() {
        // Given a rule scoped to a project, and a session inside it.
        let (state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/code/myapp")).await;
        install_rules(
            &services,
            &[denial_rule("scoped", "rm -rf", "bash", Some("myapp"))],
        );
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When two calls are dispatched in a row.
        dispatch_bash(&harness, &session_id, "rm -rf /a").await;
        dispatch_bash(&harness, &session_id, "rm -rf /b").await;

        // Then both are denied, so the cached set is the same one the second
        // call consults and denying does not depend on resolving twice.
        let messages = await_recorded(&results, 2, Duration::from_secs(3)).await;
        assert_eq!(messages.len(), 2, "both calls must be denied");
        assert!(
            messages.iter().all(|m| !m.result.success),
            "every denied call reports failure"
        );
        drop(state);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn two_sessions_in_different_projects_resolve_independent_sets() {
        // Given one rule scoped to a project, and two sessions in different ones.
        let state = State::new(AppState::default());
        let inside = SessionId::new();
        let outside = SessionId::new();
        {
            let mut guard = state.write();
            guard
                .session
                .get_or_create(&inside)
                .set_cwd("/home/dev/code/myapp".into());
            guard
                .session
                .get_or_create(&outside)
                .set_cwd("/home/dev/code/other".into());
        }
        let (harness, services) = spawn_orchestrator(&state).await;
        install_rules(
            &services,
            &[denial_rule("scoped", "rm -rf", "bash", Some("myapp"))],
        );
        let results = harness.spawn_recorder::<ToolExecutionCompleted>().await;

        // When both dispatch the same command.
        dispatch_bash(&harness, &inside, "rm -rf /a").await;
        dispatch_bash(&harness, &outside, "rm -rf /b").await;

        // Then only the session inside the rule's project is denied.
        let messages = await_recorded(&results, 1, Duration::from_secs(3)).await;
        let denied = messages.iter().find(|m| !m.result.success);
        assert!(denied.is_some(), "the in-project call must be denied");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn no_rules_installed_leaves_every_call_alone() {
        // Given a session and an empty rule set.
        let (_state, session_id, harness, services) =
            session_at(std::path::Path::new("/home/dev/code/myapp")).await;
        install_rules(&services, &[]);
        let started = harness
            .spawn_recorder::<jinn_tools_msg::ToolExecutionStarted>()
            .await;

        // When a call is dispatched.
        dispatch_bash(&harness, &session_id, "rm -rf /").await;

        // Then it starts: an absent rule set must not read as "deny everything".
        let messages = await_recorded(&started, 1, Duration::from_secs(3)).await;
        assert_eq!(messages.len(), 1, "an empty set must deny nothing");
    }
}
