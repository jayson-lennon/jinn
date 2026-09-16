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

use serde::{Deserialize, Serialize};

/// OpenRouter web search server tool configuration.
///
/// Serialized as `[openrouter_web_search]` in `jinn.toml`.
/// Controls parameters sent to the `openrouter:web_search` server tool.
/// All fields are optional - when `None`, the parameter is omitted from
/// the request and OpenRouter uses its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenrouterWebSearchConfig {
    /// Search engine: "auto", "native", "exa", "firecrawl", or "parallel".
    /// Default: "exa".
    #[serde(default)]
    pub engine: Option<String>,

    /// Maximum results per search call (1–25). `None` = OpenRouter default (5).
    #[serde(default)]
    pub max_results: Option<u32>,

    /// Maximum total results across all searches in one request.
    #[serde(default)]
    pub max_total_results: Option<u32>,

    /// How much context to retrieve: "low", "medium", or "high".
    /// `None` = OpenRouter picks adaptively.
    #[serde(default)]
    pub search_context_size: Option<String>,

    /// Only return results from these domains.
    #[serde(default)]
    pub allowed_domains: Option<Vec<String>>,

    /// Exclude results from these domains.
    #[serde(default)]
    pub excluded_domains: Option<Vec<String>>,
}

impl Default for OpenrouterWebSearchConfig {
    fn default() -> Self {
        Self {
            engine: Some("exa".to_owned()),
            max_results: None,
            max_total_results: None,
            search_context_size: None,
            allowed_domains: None,
            excluded_domains: None,
        }
    }
}
pub mod bash;
pub mod command_policy;
pub mod edit;
pub mod get_time;
pub mod grep;
pub(crate) mod input_bounds;
pub mod interactive_term;
pub mod interactive_term_kill;
pub mod interactive_term_send;
#[cfg(test)]
mod interactive_term_tests;
pub mod protocol;
pub mod read;
pub mod registry;
pub mod restart_mcp;
#[cfg(test)]
mod restart_mcp_tests;
pub mod save_plan;
pub mod session_fetch;
pub mod session_search;
pub mod skill;
pub mod task;
pub mod task_phase_listener_actor;
pub mod task_registry;
pub mod task_settle_listener_actor;
#[cfg(test)]
mod task_tests;
pub mod tool_entry;
pub mod tool_types;
pub(crate) mod truncation;
pub(crate) mod visible_lines;
pub mod write;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::Services;
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::feat::mcp_actor::protocol::McpConnectionStatus;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::protocol::SessionClosed;
use crate::feat::tools_actor::protocol::command::{
    CancelToolBatch, ExecuteTool, ExecuteToolBatch, RegisterTools,
};
use crate::feat::tools_actor::protocol::event::{
    ToolBatchCompleted, ToolExecutionCompleted, ToolsRegistered, ToolsUnregistered,
};
use crate::feat::tools_actor::tool_types::{ToolCall, ToolContext, ToolDefinition, ToolResult};
use crate::protocol::SessionId;
use jiff::Timestamp;
use jinn_provider::ServerToolType;
use kameo::prelude::{Actor, ActorRef, Context, Message};

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
    /// Session write capability for todo-list and skill tools.
    session_cap: crate::common::tcaps::session::SessionCap,
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
    /// Session write capability for todo-list and skill tools.
    pub session_cap: crate::common::tcaps::session::SessionCap,
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
fn build_openrouter_web_search_definition(config: &OpenrouterWebSearchConfig) -> ToolDefinition {
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

impl Actor for ToolOrchestratorActor {
    type Args = ToolOrchestratorActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let bus = &args.deps.services.bus;
        bus.subscribe::<RegisterTools, _>(&actor_ref).await;
        bus.subscribe::<ExecuteToolBatch, _>(&actor_ref).await;
        bus.subscribe::<CancelToolBatch, _>(&actor_ref).await;
        bus.subscribe::<ToolExecutionCompleted, _>(&actor_ref).await;
        bus.subscribe::<SessionClosed, _>(&actor_ref).await;
        bus.subscribe::<ToolsUnregistered, _>(&actor_ref).await;

        // Read web search config from preferences storage.
        let web_search_config = args
            .deps
            .services
            .user_preferences_storage
            .read()
            .openrouter_web_search
            .clone();

        let default_timeout_secs = args
            .deps
            .services
            .user_preferences_storage
            .read()
            .tool_default_timeout_secs;

        let mut actor = Self {
            deps: args.deps,
            tools: HashMap::new(),
            session_tools: HashMap::new(),
            pending: HashMap::new(),
            state: args.state,
            session_cap: args.session_cap,
            services: args.services,
        };
        let all_builtins = registry::builtin_tools(default_timeout_secs);
        let builtins: Vec<_> = if let Some(ref filter) = args.builtin_filter {
            all_builtins
                .into_iter()
                .filter(|(def, _, _)| filter.contains(&def.name))
                .collect()
        } else {
            all_builtins
        };
        let mut builtin_definitions: Vec<ToolDefinition> =
            builtins.iter().map(|(d, _, _)| d.clone()).collect();

        for (def, execute_fn, self_managed_timeout) in builtins {
            let name = def.name.clone();
            actor.tools.insert(
                name,
                ToolRegistration::Builtin {
                    definition: def,
                    execute: execute_fn,
                    self_managed_timeout,
                },
            );
        }

        // Register openrouter:web_search server tool.
        let web_search_def = build_openrouter_web_search_definition(&web_search_config);
        actor.tools.insert(
            web_search_def.name.clone(),
            ToolRegistration::Builtin {
                definition: web_search_def.clone(),
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
        builtin_definitions.push(web_search_def);

        // Announce built-in tools so downstream actors can cache them.
        actor
            .publish(ToolsRegistered {
                provider: "builtin".to_owned(),
                definitions: builtin_definitions,
                session_id: None,
            })
            .await;

        Ok(actor)
    }
}

// ---------------------------------------------------------------------------
// Bridge: impl Message<T> blocks that delegate to old handler methods
// ---------------------------------------------------------------------------
// Message handlers — direct handler calls (no bridge)
// ---------------------------------------------------------------------------

impl Message<RegisterTools> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: RegisterTools, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_register_tools(&msg.provider, &msg.definitions, msg.session_id)
            .await;
    }
}

impl Message<ExecuteToolBatch> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: ExecuteToolBatch, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_execute_tool_batch(msg.session_id, msg.tool_calls, msg.dispatched_at)
            .await;
    }
}

impl Message<CancelToolBatch> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: CancelToolBatch, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_cancel_tool_batch(&msg.session_id);
    }
}

impl Message<ToolExecutionCompleted> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: ToolExecutionCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_tool_execution_completed(msg.session_id, msg.result)
            .await;
    }
}

impl Message<SessionClosed> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut Context<Self, Self::Reply>) {
        // Drop per-session tool registrations so the map does not leak.
        if self.session_tools.remove(&msg.session_id).is_some() {
            tracing::debug!(
                session_id = ?msg.session_id,
                "removed session-scoped tool registrations on SessionClosed"
            );
        }
    }
}

impl Message<ToolsUnregistered> for ToolOrchestratorActor {
    type Reply = ();

    async fn handle(&mut self, msg: ToolsUnregistered, _ctx: &mut Context<Self, Self::Reply>) {
        // Given a provider tearing down its session-scoped registrations.
        // When pruning the routing map.
        let Some(session_map) = self.session_tools.get_mut(&msg.session_id) else {
            return;
        };
        session_map.retain(|_, reg| match reg {
            ToolRegistration::Actor { provider, .. } => provider != &msg.provider,
            ToolRegistration::Builtin { .. } => true,
        });
        if session_map.is_empty() {
            self.session_tools.remove(&msg.session_id);
        }

        // Then the provider's tools are no longer routable for that session.
    }
}

impl BusPublish for ToolOrchestratorActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl ToolOrchestratorActor {
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
    /// The outer `timeout` (from `tool_default_timeout_secs`) is a safety ceiling for
    /// all builtin tools. `bash` additionally applies its own inner
    /// `bash.default_timeout_secs`; the shorter of the two fires first.
    fn build_tool_context(&self, session_id: &SessionId, dispatched_at: Timestamp) -> ToolContext {
        let prefs = self.services.user_preferences_storage.read();
        let cwd = {
            let guard = self.state.read();
            guard.session.get(session_id).map_or_else(
                || guard.session.default_cwd().clone(),
                |s: &ChatSessionState| s.cwd().to_owned(),
            )
        };
        let max_output_lines = prefs.max_tool_output_lines;
        let max_output_bytes = prefs.max_tool_output_bytes;
        let timeout = std::time::Duration::from_secs(prefs.tool_default_timeout_secs);
        let command_policy = {
            use crate::feat::tools_actor::command_policy::{self, CompiledCommandPolicy};
            let rules = command_policy::resolve_project_rules(
                &prefs.projects,
                &cwd,
                self.services.paths.home_dir(),
            );
            CompiledCommandPolicy::compile(&rules)
        };
        ToolContext {
            cwd,
            command_policy,
            timeout: Some(timeout),
            state: Some(self.state.clone()),
            session_id: Some(session_id.clone()),
            app_paths: self.services.paths.clone(),
            bus: Some(self.bus().clone()),
            max_output_lines,
            max_output_bytes,
            dispatched_at,
            session_cap: Some(self.session_cap),
            mcp_coordinator: self.services.mcp_coordinator.get().cloned(),
            interactive_term: self.services.interactive_term.get().cloned(),
            task_spawns: Some(self.services.task_spawns.clone()),
            session_store: Some(self.services.session_store.clone()),
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
                let prefs = self.services.user_preferences_storage.read();
                self.publish(ExecuteTool {
                    session_id,
                    tool_call,
                    dispatched_at,
                    max_output_lines: prefs.max_tool_output_lines,
                    max_output_bytes: prefs.max_tool_output_bytes,
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
        let guard = self.state.read();
        let session = guard.session.get(session_id)?;
        if !session.is_mcp_server_enabled(server) {
            return Some(format!(
                "MCP server '{server}' is disabled for this session; \
                 ask the user to re-enable it via the MCP server picker"
            ));
        }
        match session.mcp_server_status().get(server) {
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
fn extract_max_duration(arguments: &str) -> Option<u64> {
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
mod openrouter_web_search_config_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use tempfile::TempDir;

    use super::OpenrouterWebSearchConfig;
    use crate::common::app_info::PREFS_FILE_NAME;
    use crate::feat::preferences_actor::user_preferences::load_preferences_from;

    #[rstest::rstest]
    fn load_parses_openrouter_web_search_config() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[openrouter_web_search]
engine = "parallel"
max_results = 5
max_total_results = 20
search_context_size = "medium"
allowed_domains = ["nature.com", "arxiv.org"]
excluded_domains = ["spam.com"]
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");

        assert_eq!(
            prefs.openrouter_web_search.engine.as_deref(),
            Some("parallel")
        );
        assert_eq!(prefs.openrouter_web_search.max_results, Some(5));
        assert_eq!(prefs.openrouter_web_search.max_total_results, Some(20));
        assert_eq!(
            prefs.openrouter_web_search.search_context_size.as_deref(),
            Some("medium")
        );
        assert_eq!(
            prefs.openrouter_web_search.allowed_domains,
            Some(vec!["nature.com".to_owned(), "arxiv.org".to_owned()])
        );
        assert_eq!(
            prefs.openrouter_web_search.excluded_domains,
            Some(vec!["spam.com".to_owned()])
        );
    }

    #[rstest::rstest]
    fn load_without_openrouter_web_search_section_uses_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"last_model = "ollama/llama3"
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");

        let defaults = OpenrouterWebSearchConfig::default();
        assert_eq!(prefs.openrouter_web_search.engine, defaults.engine);
        assert_eq!(
            prefs.openrouter_web_search.max_results,
            defaults.max_results
        );
        assert_eq!(
            prefs.openrouter_web_search.max_total_results,
            defaults.max_total_results
        );
        assert_eq!(
            prefs.openrouter_web_search.search_context_size,
            defaults.search_context_size
        );
        assert_eq!(
            prefs.openrouter_web_search.allowed_domains,
            defaults.allowed_domains
        );
        assert_eq!(
            prefs.openrouter_web_search.excluded_domains,
            defaults.excluded_domains
        );
    }
}

#[cfg(test)]
mod timeout_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{BoxedToolFuture, ToolContext, run_builtin_with_timeout};
    use crate::common::app_paths::AppPaths;
    use crate::feat::preferences_actor::user_preferences::UserPreferences;
    use jinn_provider::tool_types::{ToolCall, ToolResult};

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
            command_policy:
                crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: AppPaths::new_in(std::path::Path::new("/tmp")),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,
            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
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
    fn tool_timeout_value_sourced_from_preferences() {
        // Given preferences with a custom tool timeout.
        let prefs = UserPreferences {
            tool_default_timeout_secs: 7,
            ..UserPreferences::default()
        };

        // Then the timeout value reflects the preference.
        assert_eq!(prefs.tool_default_timeout_secs, 7);
        assert_eq!(
            Duration::from_secs(prefs.tool_default_timeout_secs),
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
    use jinn_provider::tool_types::{ToolCall, ToolResult};

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
                command_policy:
                    crate::feat::tools_actor::command_policy::CompiledCommandPolicy::default(),
                timeout: None,
                state: None,
                session_id: None,
                app_paths: crate::common::app_paths::AppPaths::new_in(std::path::Path::new("/tmp")),
                bus: None,
                max_output_lines: None,
                max_output_bytes: None,
                dispatched_at: jiff::Timestamp::now(),
                session_cap: None,
                mcp_coordinator: None,
                interactive_term: None,
                task_spawns: None,
                session_store: None,
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
    use jinn_provider::ToolDefinition;

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

    use kameo::actor::Spawn;

    use crate::common::app_state::AppState;
    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::common::state::State;
    use crate::feat::mcp_actor::protocol::McpConnectionStatus;
    use crate::feat::tools_actor::protocol::command::{
        ExecuteTool, ExecuteToolBatch, RegisterTools,
    };
    use crate::feat::tools_actor::protocol::event::{ToolExecutionCompleted, ToolsUnregistered};
    use crate::feat::tools_actor::tool_types::{ToolCall, ToolDefinition};
    use crate::protocol::SessionId;

    use super::{ToolOrchestratorActor, ToolOrchestratorActorDeps};

    const PROVIDER: &str = "mcp__stub__";

    async fn spawn_orchestrator(
        state: &State,
    ) -> (TestHarness, kameo::actor::ActorRef<ToolOrchestratorActor>) {
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        let actor = ToolOrchestratorActor::spawn(ToolOrchestratorActorDeps {
            deps: crate::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            services,
            session_cap: crate::common::tcaps::mint::mint_session_cap(),
            builtin_filter: None,
        });
        actor.wait_for_startup().await;
        (harness, actor)
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
    async fn tools_unregistered_removes_the_providers_tools_from_the_routing_map() {
        // Given a state with a seeded session, the stub tools registered for it.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        state.write_test_no_cap().session.get_or_create(&session_id);
        let (harness, _actor) = spawn_orchestrator(&state).await;
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
        state.write_test_no_cap().session.get_or_create(&session_id);
        let (harness, _actor) = spawn_orchestrator(&state).await;
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
            .write_test_no_cap()
            .session
            .get_or_create(&session_id)
            .enable_mcp_server("stub");
        state
            .write_test_no_cap()
            .session
            .get_mut(&session_id)
            .expect("session")
            .set_mcp_server_status("stub", McpConnectionStatus::Dead);
        let (harness, _actor) = spawn_orchestrator(&state).await;
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
            .write_test_no_cap()
            .session
            .get_or_create(&session_id)
            .enable_mcp_server("stub");
        state
            .write_test_no_cap()
            .session
            .get_mut(&session_id)
            .expect("session")
            .set_mcp_server_status("stub", McpConnectionStatus::Running);
        let (harness, _actor) = spawn_orchestrator(&state).await;
        register_stub_tools(&harness, &session_id).await;
        let dispatched = harness.spawn_recorder::<ExecuteTool>().await;

        // When dispatching a call to the running server's tool.
        publish_batch(&harness, &session_id).await;

        // Then an ExecuteTool is published on the bus as before (no gate
        // rejection short-circuits the dispatch).
        let messages = await_recorded(&dispatched, 1, Duration::from_secs(3)).await;
        assert_eq!(messages[0].tool_call.name, "mcp__stub__echo");
    }
}
