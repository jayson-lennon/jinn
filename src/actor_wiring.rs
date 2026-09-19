//! Actor wiring — spawns all actors as kameo actors.
//!
//! This module encapsulates the one-time startup wiring: creating shared state,
//! spawning each actor via kameo's `Spawn::spawn()`, building the bus and bridge,
//! and waiting for the actor system to become ready. Called once from `App::dispatch`.
//!
//! # Spawn order
//!
//! 1. Infrastructure actors (system-ready, env-init).
//! 2. Init actors (provider-init, preferences, scan actors).
//! 3. Domain actors (session, tools, history workers, etc.).
//!
//! EnvInitActor is spawned first with `wait_for_startup()` so dependent actors
//! can look it up in the kameo registry and pull config via `ask()`.

use jinn_domain::ApiKeysService;
use jinn_domain::AppState;
use jinn_domain::ConfigStorageService;
use jinn_domain::LlmServiceFactoryService;
use jinn_domain::ProviderRegistryService;
use jinn_domain::Services;
use jinn_domain::SessionStoreService;
use jinn_preferences_config::UserPreferencesStorageService;
use jinn_quake_bar;
use jinn_slices;

use jinn_domain::common::actor_deps::ActorDeps;
use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;

use jinn_domain::init::env_init_actor::{EnvInitActor, EnvInitActorDeps};
use jinn_domain::init::provider_init_actor::{ProviderInitActor, ProviderInitActorDeps};
use jinn_domain::init::system_ready_actor::{SystemReadyActor, SystemReadyActorDeps};

use jinn_domain::{AppCore, State};

use kameo::actor::Spawn;

/// Spawn a kameo actor and announce its lifecycle on the bus.
///
/// Publishes `ActorStarting` before the spawn future resolves and
/// `ActorStarted` after, so the dashboard can display the lifecycle.
macro_rules! spawn_tracked {
    ($bus:expr, $name:expr, $desc:expr, $spawn:expr) => {{
        let __bus = $bus.actor_ref();
        let __name: &str = $name;
        let __desc: &str = $desc;
        let _ = __bus
            .tell(kameo_actors::message_bus::Publish(
                jinn_domain::common::actor::protocol::event::ActorStarting {
                    name: __name.to_string(),
                    description: Some(__desc.to_string()),
                },
            ))
            .await;
        let __actor = $spawn;
        let _ = __bus
            .tell(kameo_actors::message_bus::Publish(
                jinn_domain::common::actor::protocol::event::ActorStarted {
                    name: __name.to_string(),
                    description: Some(__desc.to_string()),
                },
            ))
            .await;
        __actor
    }};
}

/// The fixed (required) inputs to actor-system construction.
#[derive(Clone)]
pub struct ActorSystemBuilderArgs {
    /// Tokio runtime handle actors are spawned onto.
    pub handle: tokio::runtime::Handle,
    /// LLM service factory.
    pub llm_service: LlmServiceFactoryService,
    /// Provider registry service.
    pub provider_registry: ProviderRegistryService,
    /// Resolved API keys.
    pub api_keys: ApiKeysService,
    /// Config storage service.
    pub config_storage: ConfigStorageService,
    /// Session store service. Caller-built (e.g. `SqliteSessionStore`).
    pub session_store: SessionStoreService,
    /// User preferences storage service.
    pub user_preferences_storage: UserPreferencesStorageService,
    /// App state storage service.
    pub app_state_storage: jinn_preferences_config::AppStateStorageService,
    /// Application paths.
    pub paths: jinn_domain::AppPaths,
    /// Dump directory for provider request debugging. `None` disables.
    pub dump_requests: Option<std::path::PathBuf>,
    /// The compaction system prompt loaded from the prompts directory at
    /// startup (the compaction worker consumes it on each compaction).
    pub compaction_prompt: String,
}

/// Builds the actor system: spawns all actors via kameo.
///
/// Construct with [`ActorSystemBuilder::new`], then call
/// [`ActorSystemBuilder::build`]. After spawning all actors, `build` blocks
/// the calling thread until the actor system signals readiness (3s timeout).
pub struct ActorSystemBuilder {
    args: ActorSystemBuilderArgs,
}

impl ActorSystemBuilder {
    #[must_use]
    pub fn new(args: ActorSystemBuilderArgs) -> Self {
        Self { args }
    }
    /// Spawn all actors via kameo, build the bus and bridge, and wait for readiness.
    pub async fn build(self) -> (AppCore, Services, jinn_discord::ActivatedDiscord) {
        let ActorSystemBuilderArgs {
            handle,
            llm_service,
            provider_registry,
            api_keys,
            config_storage,
            session_store,
            user_preferences_storage,
            app_state_storage,
            paths,
            dump_requests,
            compaction_prompt,
        } = self.args;

        // Create shared State FIRST — injected into multiple actors.
        let state = State::new(AppState::default());
        let intent_handler_cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();

        // Set preferences
        {
            let mut guard = state.write(&intent_handler_cap);
            guard.frontend.preferences = user_preferences_storage.read();
        }

        // Set app state (last_model, theme_name, persona_name, sidebar_width)
        {
            let app_state = app_state_storage.read();
            let mut guard = state.write(&intent_handler_cap);
            guard.frontend.app_state.last_model = app_state.last_model.clone();
            guard.frontend.app_state.theme_name = app_state.theme_name.clone();
            guard.frontend.app_state.persona_name = app_state.persona_name.clone();
            guard.frontend.app_state.sidebar_width = app_state.sidebar_width;
        }

        // Set default CWD for sessions (inherited from shell).
        let (initial_session_id, initial_cwd) = {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
            let mut guard = state.write(&intent_handler_cap);
            guard.session.set_default_cwd(cwd.clone());
            guard.active_session_mut().set_cwd(cwd.clone());
            (guard.active_session().session_id().clone(), cwd)
        };

        // Create the message fabric: the trouper actor system (primary) and
        // the transitional kameo bus leg, plus the closure bridge.
        let (bus, trouper_system) = {
            let system = trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            );
            let bus_actor = kameo_actors::message_bus::MessageBus::new(
                kameo_actors::DeliveryStrategy::BestEffort,
            );
            let bus_ref = kameo_actors::message_bus::MessageBus::spawn(bus_actor);
            (
                jinn_domain::common::services::bus_service::BusService::new_trouper(
                    system.clone(),
                    Some(bus_ref.clone()),
                ),
                system,
            )
        };
        let bridge = jinn_domain::common::bridge::Bridge::with_system(&bus, &handle);

        let root = jinn_domain::common::root_supervisor::RootSupervisor::spawn_root().await;

        let mut services = Services {
            paths: paths.clone(),
            handle: handle.clone(),
            llm_service: llm_service.clone(),
            provider_registry: provider_registry.clone(),
            api_keys: api_keys.clone(),
            config_storage: config_storage.clone(),
            session_store: session_store.clone(),
            user_preferences_storage: user_preferences_storage.clone(),
            app_state_storage: app_state_storage.clone(),
            tempdir: None,
            bus,
            bridge: bridge.clone(),
            trouper_system,
            root_supervisor: root.clone(),
            mcp_coordinator: std::sync::Arc::new(std::sync::OnceLock::new()),
            interactive_term: std::sync::Arc::new(std::sync::OnceLock::new()),
            request_dump: jinn_domain::common::request_dump::RequestDumpService::new(dump_requests),
            task_spawns: jinn_tools_msg::TaskSpawnRegistry::default(),
            slices: jinn_domain::common::slices::Slices::new(),
            key_routes: jinn_domain::common::slices::key_routes::KeyRoutes::new(),
            viewport: jinn_domain::common::slices::view::Viewport::new(),
            overlay_views: jinn_domain::common::overlay_views::OverlayViews::new(),
            picker_registry: jinn_domain::feat::picker::registry::build_picker_registry(),
        };

        let actor_deps = ActorDeps {
            services: services.clone(),
        };

        // ── Forward-bridge route drains ───────────────────────────────
        // One relay per crossing message, registered on the bus in its
        // own on_start: publishes after the drains cannot be missed, so
        // the ordering constraint against slice activation is gone.
        jinn_dashboard::bridge::drain_routes(&services).await;
        jinn_quake_bar_drain(&services).await;
        jinn_discord_drain(&services).await;

        // ── Dashboard slice ───────────────────────────────────────────
        // Activation mints the cell, spawns the canvas actor FIRST
        // (subscribe is the readiness point, so no lifecycle event from
        // subsequently spawned actors is missed), attaches rows,
        // registers the view + tab. Slice integration is exactly this
        // call.
        #[expect(
            clippy::panic,
            reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
        )]
        if let Err(error) = jinn_dashboard::activate(&mut jinn_dashboard::SliceCtx {
            slices: &services.slices,
            key_routes: &services.key_routes,
            viewport: &mut services.viewport,
            trouper_system: &services.trouper_system,
        }) {
            panic!("dashboard slice activation failed: {error}");
        }

        // ── Discord slice ─────────────────────────────────────────────
        // Activation mints the connection cell, spawns the status
        // actor (the connection authority — after the dashboard so its
        // publications are not missed), resolves the `[discord]`
        // section (fail-fast), creates the gateway kanal channels
        // unconditionally, config-gates the bridge actor, and attaches
        // the `gdc` route row. Slice integration is exactly this call.
        let discord_activated = jinn_discord_activate(&mut services, state.clone()).await;

        // Scope-focus slice: activation mints the interaction cell
        // (focus stack, TUI signals, quit latch). Attaches the handle
        // the FrontendState facade resolves through — before any intent
        // can fire.
        state
            .write(&intent_handler_cap)
            .frontend
            .attach_slices(services.slices.clone());
        jinn_scope_focus_activate(&mut services);
        jinn_chat_log_view_activate(&mut services, &state);
        jinn_chat_input_activate(&mut services);
        jinn_cwd_activate(&mut services);
        jinn_preferences_activate(&mut services, state.clone()).await;
        jinn_preferences::bridge::drain_routes(&services).await;
        jinn_sidebar_activate(&mut services, state.clone());
        jinn_sidebar::bridge::drain_routes(&services).await;
        jinn_theme_activate(&mut services);

        // Persona slice: activation scans the persona directories and
        // mints the personas cell; the returned set is published as
        // `PersonasLoaded` below, after the session actor (its sole
        // subscriber) has spawned — the plugin's push-once contract.
        let persona_entries = jinn_persona_activate(&mut services);

        // Term slice: registers the terminal tab mirrors cell (written
        // by the coordinator actor, read by the TUI overlay + tools),
        // attaches the keybind rows, the capture key hook, and the
        // overlay geometry + renderer. Composition owns exactly this
        // call. Does NOT mint the shared control registry — wiring owns
        // that set-once static so it can hand the same registry to the
        // coordinator actor spawned below.
        jinn_term::activate(&mut services, &state);

        // Tools slice: activation mints the tools/registry cell (idempotent);
        // the orchestrator actor is spawned below (it needs the announce
        // supervisor and explicit ordering vs. the MCP coordinator).
        jinn_tools::activate(&mut services, &state);

        // Quake bar slice: activation mints the cell, spawns the actor
        // (submit-log writer), attaches rows, and registers the input
        // hook + overlay geometry. Composition owns exactly this call.
        jinn_status_bar_activate(&mut services);
        jinn_quake_bar_activate(&mut services);

        // ── Session-init slice ────────────────────────────────────────
        // Activation installs the discovery partition set, spawns the
        // supervisor + notifier on trouper, and stages the crossing
        // routes. Must precede the readiness publish at the tail of
        // this function: the supervisor's subscriptions must exist
        // before the first `EnvironmentLoaded` trigger.
        jinn_session_init_activate(&mut services, state.clone());
        jinn_session_init::bridge::drain_routes(&services).await;

        // ── Infrastructure actors ──────────────────────────────────────────

        // System-ready actor: signals main thread when all actors started.
        let (ready_tx, ready_rx) = kanal::unbounded::<()>();
        let _system_ready = spawn_tracked!(
            &services.bus,
            "system-ready",
            "SystemReadyActor",
            SystemReadyActor::supervise(
                &root,
                SystemReadyActorDeps {
                    deps: actor_deps.clone(),
                    ready_tx,
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // ── Init actors ────────────────────────────────────────────────────

        // Env init: registers in actor registry, defers config loading to GetEnvironmentConfig ask.
        let env_init = spawn_tracked!(
            &services.bus,
            "env-init",
            "EnvInitActor",
            EnvInitActor::supervise(
                &root,
                EnvInitActorDeps {
                    deps: actor_deps.clone(),
                    registry_name: Some("env-init"),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );
        env_init.wait_for_startup().await;
        // Provider init: on EnvironmentLoaded, builds registry, merges cache, resolves last_model.
        let _provider_init = spawn_tracked!(
            &services.bus,
            "provider-init",
            "ProviderInitActor",
            ProviderInitActor::supervise(
                &root,
                ProviderInitActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    provider_cap: jinn_domain::common::tcaps::mint::mint_provider_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Preferences + app-state actors: trouper, installed with the
        // preferences slice's activation wrapper below.
        // ── Domain actors ──────────────────────────────────────────────────

        // Model discovery actor.
        let _discover = spawn_tracked!(
            &services.bus,
            "discover",
            "DiscoverActor",
            jinn_domain::feat::provider::discover_actor::DiscoverActor::supervise(
                &root,
                jinn_domain::feat::provider::discover_actor::DiscoverActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Session persistence actor — must spawn before ToolOrchestratorActor so
        // ToolsRegistered subscription is ready when tools register builtins in on_start.
        // Unbounded mailbox: the session actor is the single sink for every streaming
        // event (StreamToken, StreamCompleted, ToolBatchCompleted, …) from a provider
        // burst. The default bounded(64) mailbox can momentarily fill at the [DONE]
        // peak of a large reasoning turn, and because the bus uses BestEffort
        // (try_send) delivery, the terminal `StreamCompleted(ToolUse)` gets silently
        // dropped on `MailboxFull` — permanently wedging the session (the phase never
        // advances out of Streaming). An unbounded mailbox means try_send always
        // succeeds, so the critical control message can never be dropped. There is no
        // deadlock risk: nothing downstream awaits the session actor's mailbox
        // capacity (publishers use fire-and-forget tell under BestEffort).
        let token_counter = TiktokenCounter::o200k_base();
        // Token-count slice: activation registers the shared entry-token
        // cache cell and spawns the slice's trouper actors; the returned
        // cache is handed to the session actor (accumulation gate) and
        // the prune workers.
        let entry_token_cache = jinn_token_count_activate(&mut services, state.clone());
        jinn_token_count::bridge::drain_routes(&services).await;

        // ── Turn-dispatch slice ─────────────────────────────────────
        // Activation spawns the queue actor (trouper ServiceActor, the
        // turn-queue consumer) and stages its crossing routes. Must
        // precede the env-init tail and the session-actor spawn: the
        // queue actor's subscriptions must exist before any dispatch
        // trigger (an `Idle` phase event or a `DispatchTurn` command)
        // is published.
        jinn_turn_dispatch_activate(&mut services, state.clone());
        jinn_turn_dispatch::bridge::drain_routes(&services).await;

        // ── Inference slice ─────────────────────────────────────────
        // Activation spawns the inference actor (trouper ServiceActor,
        // the LLM stream driver) and stages its crossing routes. Must
        // precede the env-init tail and the session-actor spawn: the
        // forward relays for `SendToLlmProvider`/`CancelStream` must
        // exist before the first dispatch is published.
        jinn_inference_activate(&mut services);
        jinn_inference::bridge::drain_routes(&services).await;

        // ── Context-assembly slice ─────────────────────────────────────
        // Install the slice's actors on trouper (the stateless assembly
        // service + the context-size actor) and stage the crossing
        // routes; the drain below spawns the relays. The size actor
        // holds `Services` for the assembly ask.
        jinn_context_assembly::install_actors(&services.trouper_system, state.clone(), &services);
        {
            let mut host = jinn_slices::SliceHost::new(
                &services.slices,
                &mut services.viewport,
                &services.overlay_views,
                &services.key_routes,
                &services.trouper_system,
            );
            jinn_context_assembly::stage_routes(&mut host);
            if let Err(error) = host.finalize(&|_key| None) {
                panic!("context-assembly slice finalize failed: {error}");
            }
        }
        jinn_context_assembly::bridge::drain_routes(&services).await;
        let _session =
            jinn_domain::feat::session::session_actor::SessionPersistenceActor::supervise(
                &root,
                jinn_domain::feat::session::session_actor::SessionPersistenceActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    counter: token_counter,
                    token_cache: entry_token_cache.clone(),
                    builtin_registry:
                        jinn_domain::feat::session_lifecycle::builtin::BuiltinRegistry::new(),
                    shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()),
                    image_converter:
                        jinn_domain::feat::image_convert::ImageConverterService::system(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn_with_mailbox(kameo::mailbox::unbounded())
            .await;
        _session.wait_for_startup().await;

        // Tool orchestrator actor: dispatched batches emit per-call
        // execution events and a final ToolBatchCompleted. Spawned after
        // the session actor (consumes SessionClosed) and before the MCP
        // coordinator (so MCP tool registrations land in a running
        // orchestrator).
        // Tool orchestrator actor: dispatched batches emit per-call
        // execution events and a final ToolBatchCompleted. Spawned after
        // the session actor (consumes SessionClosed) and before the MCP
        // coordinator (so MCP tool registrations land in a running
        // orchestrator).
        let _tools = spawn_tracked!(
            &services.bus,
            "tool-orchestrator",
            "ToolOrchestratorActor",
            jinn_tools::ToolOrchestratorActor::supervise(
                &root,
                jinn_tools::ToolOrchestratorActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    services: services.clone(),
                    session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                    builtin_filter: None,
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );
        _tools.wait_for_startup().await;

        // MCP lifecycle actor: subscribes to session lifecycle events +
        // McpEnablementChanged, spawning/killing one McpActor per
        // (session × enabled server). Spawned after the tool orchestrator so
        // tool registrations from McpActor land in an already-running
        // orchestrator. Restored sessions are picked up via SessionLoadCompleted;
        // no startup scan is needed here.
        let _mcp_coordinator = spawn_tracked!(
            &services.bus,
            "mcp-coordinator",
            "McpCoordinatorActor",
            jinn_mcp_slice::coordinator::McpCoordinatorActor::supervise(
                &root,
                jinn_mcp_slice::coordinator::McpCoordinatorActorDeps {
                    deps: actor_deps.clone(),
                    root: root.clone(),
                    state: state.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );
        _mcp_coordinator.wait_for_startup().await;
        // Expose a handle to the tool layer (restart_mcp_server). Minted from
        // the actor ref by the slice; `OnceLock::set` returns Err if already
        // set — ignore (e.g. test re-seed).
        let _ = services
            .mcp_coordinator
            .set(jinn_mcp_slice::mcp_coordinator_handle(
                _mcp_coordinator.clone(),
            ));

        // Interactive-term coordinator: owns PTY sessions across tool calls
        // (the `interactive_term*` tools ask it directly). Spawned with the
        // same lifecycle shape as the MCP coordinator; the per-session
        // control registry goes to the terminal tab (takeover UI) wiring.
        let term_controls = jinn_term_msg::TermControls::default();
        let (term_coordinator, _controls) =
            jinn_term::interactive_term_actor::spawn_interactive_term_actor(
                jinn_term::interactive_term_actor::InteractiveTermActorDeps {
                    bus: services.bus.clone(),
                    controls: term_controls.clone(),
                    state: state.clone(),
                    settle_quiet: std::time::Duration::from_millis(
                        state
                            .read()
                            .frontend
                            .preferences
                            .interactive_term
                            .settle_quiet_ms,
                    ),
                    settle_cap: std::time::Duration::from_millis(
                        state
                            .read()
                            .frontend
                            .preferences
                            .interactive_term
                            .settle_max_wait_ms,
                    ),
                },
                &root,
            )
            .await;
        let _ = services
            .interactive_term
            .set(std::sync::Arc::new(ActorTermHandle::new(term_coordinator)));
        // Install the shared registry for the IntentHandler's takeover
        // intents (synchronous flips that in-flight tool calls observe
        // mid-drain).
        let _ = jinn_term_msg::TERM_CONTROLS.set(term_controls);

        // Directory lister actor (`@path` file popup).
        let _directory_lister = spawn_tracked!(
            &services.bus,
            "directory-lister",
            "DirectoryListerActor",
            jinn_domain::feat::file_lister::DirectoryListerActor::supervise(
                &root,
                jinn_domain::feat::file_lister::DirectoryListerActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Provider actor.
        let _provider = spawn_tracked!(
            &services.bus,
            "provider",
            "ProviderActor",
            jinn_domain::feat::provider::provider_actor::ProviderActor::supervise(
                &root,
                jinn_domain::feat::provider::provider_actor::ProviderActorDeps {
                    state: state.clone(),
                    deps: actor_deps.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_provider_cap(),
                    session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Search index maintenance: message-driven reindex state machine —
        // refreshes its in-memory dirty-session queue when idle and
        // reindexes at most REINDEX_BATCH sessions per heartbeat,
        // publishing the remaining count after every session.
        let _search_index = spawn_tracked!(
            &services.bus,
            jinn_domain::feat::session_search::search_index_actor::SEARCH_INDEX_ROW_NAME,
            "SearchIndexActor",
            jinn_domain::feat::session_search::search_index_actor::spawn_search_index_actor(
                jinn_domain::feat::session_search::search_index_actor::SearchIndexActorDeps {
                    deps: actor_deps.clone(),
                    interval:
                        jinn_domain::feat::session_search::search_index_actor::REINDEX_INTERVAL,
                    batch: jinn_domain::feat::session_search::search_index_actor::REINDEX_BATCH,
                },
                &root,
            )
            .await
        );

        // Context size actor: trouper, installed with the context-assembly
        // slice's install_actors call above.

        // ── Context-curation slice ──────────────────────────────────
        // Activation spawns the two curation troupers (the prune actor
        // and the compaction actor) and stages their crossing routes.
        // Must precede the session-actor spawn: the actors' subscriptions
        // must exist before any `HistoryAppended` / `TriggerCompaction`
        // publish. Strategy enablement is a construction-time gate — the
        // disabled strategies never reach the actor.
        jinn_context_curation_activate(
            &mut services,
            state.clone(),
            &user_preferences_storage,
            handle.clone(),
            compaction_prompt,
        );

        // Signal system readiness and trigger init chain.
        {
            let bus_ref = services.bus.actor_ref();
            let env_init = env_init.clone();

            // INVARIANT: the MCP coordinator was spawned and fully awaited
            // (`wait_for_startup`) above, so it has already subscribed to
            // `SessionCreated` and `McpEnablementChanged`. Publishing
            // `EnvironmentLoaded` here triggers the welcome-session seeding,
            // which may publish `McpEnablementChanged` immediately — the
            // subscription must already exist. Do not move this publish ahead
            // of the coordinator spawn.

            // Personas: the persona slice scanned at activation; publish
            // now that every actor (the session actor subscribes to
            // `PersonasLoaded`) is spawned.
            if !persona_entries.entries.is_empty() {
                let _ = bus_ref
                    .tell(kameo_actors::message_bus::Publish(
                        jinn_domain::feat::context::protocol::event::PersonasLoaded {
                            personas: persona_entries.entries.clone(),
                            error: None,
                        },
                    ))
                    .await;
            }

            // Signal all actors spawned.
            let _ = bus_ref
                .tell(kameo_actors::message_bus::Publish(
                    jinn_domain::common::actor::protocol::event::AllActorsSpawned,
                ))
                .await;

            // Ask EnvInitActor for config and publish EnvironmentLoaded to trigger init chain.
            use jinn_domain::init::env_init_actor::GetEnvironmentConfig;
            match env_init.ask(GetEnvironmentConfig).await {
                Ok(Some(config)) => {
                    let _ = bus_ref
                        .tell(kameo_actors::message_bus::Publish(
                            jinn_domain::init::env_init_actor::EnvironmentLoaded { config },
                        ))
                        .await;
                }
                Ok(None) => {
                    tracing::warn!("no provider config found — skipping EnvironmentLoaded");
                }
                Err(e) => {
                    tracing::error!(err = ?e, "failed to get environment config from EnvInitActor");
                }
            }

            // The boot session's cwd is known here; the session-init
            // supervisor routes from payloads, not shared state. This publish
            // triggers the initial session's discovery through the same
            // payload path as every other session.
            let _ = bus_ref
                .tell(kameo_actors::message_bus::Publish(
                    jinn_domain::feat::session_lifecycle::protocol::event::SessionCwdChanged {
                        session_id: initial_session_id,
                        cwd: initial_cwd,
                    },
                ))
                .await;
        }

        // Wait for SystemReadyActor to confirm readiness.
        let _ = ready_rx.to_async().recv().await;

        // Build AppCore with shared state and the bridge.
        let core = AppCore {
            state: state.clone(),
            bridge: services.bridge.clone(),
        };

        (core, services, discord_activated)
    }
}

/// Activates the quake-bar slice over the kernel's registries.
///
/// The slice crate is kernel-free, so composition assembles the
/// `SliceHost` borrows and hands them over.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
/// Activates the status-bar slice: its state cell only (the element
/// itself is display chrome registered into the UI registry by the
/// TUI composition). No routes, no actors.
/// Activates the scope-focus slice: its state cell only. No routes,
/// no actors, no view.
fn jinn_scope_focus_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_scope_focus::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("scope-focus slice finalize failed: {error}");
    }
}

/// Activates the chat-log-view slice: its state cell only. No routes,
/// no actors, no view. Also attaches the registry handle on the session
/// map so every session's view facade resolves the cell.
fn jinn_chat_log_view_activate(services: &mut Services, state: &jinn_domain::State) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_chat_log_view::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("chat-log-view slice finalize failed: {error}");
    }
    state.read().session.attach_slices(services.slices.clone());
}

/// Activates the chat-input slice: its state cell only. No routes, no
/// actors, no view. The session map already carries the attached registry
/// handle, so every session's input facade resolves the cell.
fn jinn_sidebar_activate(services: &mut Services, state: jinn_domain::common::state::State) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_sidebar::activate(&mut host, state);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("sidebar slice finalize failed: {error}");
    }
}

fn jinn_cwd_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_cwd::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("cwd slice finalize failed: {error}");
    }
}

async fn jinn_preferences_activate(
    services: &mut Services,
    state: jinn_domain::common::state::State,
) {
    // The two persistence actors spawn here (with the state handle and
    // their frontend caps) and subscribe synchronously — this must
    // complete before the env-init tail publishes `EnvironmentLoaded`,
    // which triggers publishes of `UpdateAppState`/`UpdatePreferences`
    // on first boot.
    let system = services.trouper_system.clone();
    let services_handle = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_preferences::activate(&mut host, &system, services_handle, state);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("preferences slice finalize failed: {error}");
    }
}

fn jinn_chat_input_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_chat_input::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("chat-input slice finalize failed: {error}");
    }
}

/// Activates the theme slice: scans the theme directories once and mints
/// the theme-entries cell. No routes, no actors, no view — the readers
/// are the theme picker's open hook and the app-state actor's resolution.
fn jinn_token_count_activate(
    services: &mut Services,
    state: jinn_domain::common::state::State,
) -> jinn_token_count_msg::HistoryWorkerChatEntryTokenCache {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let cache = jinn_token_count::activate(&mut host, state);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("token-count slice finalize failed: {error}");
    }
    cache
}

/// Activates the context-curation slice: builds the config-gated prune
/// strategy list, spawns the two curation troupers (prune + compaction),
/// stages their crossing routes, and drains them.
///
/// Strategy enablement is a construction-time gate (the wiring shape the
/// kameo spawns used) — a disabled strategy never reaches the prune
/// actor. The regex strategy additionally skips when its rule list is
/// empty or any rule fails to compile (warn-and-skip, never a launch
/// failure).
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn jinn_context_curation_activate(
    services: &mut Services,
    state: jinn_domain::common::state::State,
    user_preferences_storage: &UserPreferencesStorageService,
    handle: tokio::runtime::Handle,
    compaction_prompt: String,
) {
    use jinn_context_curation::strategies::{
        AnchoredAssistantAutoPruneWorker, BrokenEditAutoPruneWorker,
        ConsecutiveReadsAutoPruneWorker, DoubleEditAutoPruneWorker, EditReadAutoPruneWorker,
        ReadEditAutoPruneWorker, RegexAutoPruneWorker, TodoAutoPruneWorker,
        ToolAgeWindowAutoPruneWorker, TrivialAssistantAutoPruneWorker,
    };
    use jinn_context_curation::worker::HistoryWorker;
    use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;

    let prefs = user_preferences_storage.read();
    let auto_prune = prefs.auto_prune.clone();
    let entry_token_cache = HistoryWorkerChatEntryTokenCache::default();
    let counter =
        jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter::o200k_base();

    let mut workers: Vec<Box<dyn HistoryWorker>> = Vec::new();

    // Auto-prune worker: read→edit context pruning.
    let config = auto_prune.read_edit.clone();
    if config.enabled {
        workers.push(Box::new(ReadEditAutoPruneWorker { config }));
    }

    // Auto-prune worker: edit→read context pruning.
    let config = auto_prune.edit_read.clone();
    if config.enabled {
        workers.push(Box::new(EditReadAutoPruneWorker { config }));
    }

    // Auto-prune worker: regex-based tool call pruning.
    let regex_config = auto_prune.regex.clone();
    if regex_config.enabled && !regex_config.rules.is_empty() {
        match RegexAutoPruneWorker::from_config(&regex_config) {
            Ok(worker) => workers.push(Box::new(worker)),
            Err(e) => {
                tracing::warn!(err=?e, "invalid regex in auto_prune config, skipping");
            }
        }
    } else {
        tracing::debug!(
            enabled = regex_config.enabled,
            rules = regex_config.rules.len(),
            "regex auto-prune skipped",
        );
    }

    // Auto-prune worker: todo tool call pruning.
    let config = auto_prune.todo.clone();
    if config.enabled {
        workers.push(Box::new(TodoAutoPruneWorker { config }));
    }

    // Auto-prune worker: broken-edit context pruning.
    let config = auto_prune.broken_edit.clone();
    if config.enabled {
        workers.push(Box::new(BrokenEditAutoPruneWorker { config }));
    }

    // Auto-prune worker: double-edit context pruning.
    let config = auto_prune.double_edit.clone();
    if config.enabled {
        workers.push(Box::new(DoubleEditAutoPruneWorker { config }));
    }

    // Auto-prune worker: consecutive-reads per-file pruning.
    let config = auto_prune.consecutive_reads.clone();
    if config.enabled {
        workers.push(Box::new(ConsecutiveReadsAutoPruneWorker { config }));
    }

    // Auto-prune worker: tool-age-window context pruning.
    let config = auto_prune.tool_age_window.clone();
    if config.enabled {
        workers.push(Box::new(ToolAgeWindowAutoPruneWorker { config }));
    }

    // Auto-prune worker: trivial-assistant context pruning.
    let trivial_config = auto_prune.trivial_assistant.clone();
    if trivial_config.enabled {
        workers.push(Box::new(TrivialAssistantAutoPruneWorker {
            config: trivial_config,
            token_cache: entry_token_cache.clone(),
            counter: counter.clone(),
        }));
    }

    // Auto-prune worker: anchored-assistant context pruning (its
    // candidate floor derives from the trivial-assistant max-tokens).
    let anchored_config = auto_prune.anchored_assistant.clone();
    let trivial_max_tokens = auto_prune.trivial_assistant.max_tokens as u32;
    if anchored_config.enabled {
        workers.push(Box::new(AnchoredAssistantAutoPruneWorker {
            radius: anchored_config.radius,
            config: anchored_config,
            min_candidate_tokens: trivial_max_tokens + 1,
            token_cache: entry_token_cache.clone(),
            counter,
        }));
    }
    drop(prefs);

    let compaction_deps = jinn_context_curation::compaction_actor::CompactionActorDeps {
        services: services.clone(),
        state: state.clone(),
        handle,
        compaction_prompt,
    };

    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_context_curation::activate(&mut host, workers, compaction_deps);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("context-curation slice finalize failed: {error}");
    }
}

fn jinn_persona_activate(services: &mut Services) -> jinn_persona_msg::Personas {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let scanned = jinn_persona::activate(&mut host, &services.paths.personas_dir());
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("persona slice finalize failed: {error}");
    }
    scanned
}

/// Activates the turn-dispatch slice: spawns the queue actor (trouper
/// ServiceActor) and stages its crossing routes. The queue actor holds a
/// `Services` clone for its bus publishes and the assembly ask.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn jinn_turn_dispatch_activate(services: &mut Services, state: jinn_domain::common::state::State) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_turn_dispatch::activate(&mut host, state, services_snapshot);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("turn-dispatch slice finalize failed: {error}");
    }
}

/// Activates the inference slice: spawns the inference actor (trouper
/// ServiceActor) and stages its crossing routes. The actor holds a
/// `Services` clone for its bus publishes and factory resolution.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn jinn_inference_activate(services: &mut Services) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_inference::activate(&mut host, services_snapshot);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("inference slice finalize failed: {error}");
    }
}

fn jinn_theme_activate(services: &mut Services) {
    let (themes_dir, system_themes_dir) = {
        (
            services.paths.themes_dir(),
            services.paths.system_themes_dir(),
        )
    };
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_theme_slice::activate(&mut host, &themes_dir, &system_themes_dir);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("theme slice finalize failed: {error}");
    }
}

fn jinn_status_bar_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_status_bar::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("status-bar slice finalize failed: {error}");
    }
}

fn jinn_quake_bar_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_quake_bar::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("quake-bar slice finalize failed: {error}");
    }
}

/// Drains the quake-bar slice's staged forward routes into per-route
/// relays. Kernel-side: the relays are kameo actors.
async fn jinn_quake_bar_drain(services: &Services) {
    jinn_domain::common::trouper_bridge::spawn_one::<jinn_quake_bar::SubmitQuakeBarCommand>(
        services,
        &jinn_slices::host::RouteEntry {
            schema_id:
                <jinn_quake_bar::SubmitQuakeBarCommand as trouper::schema::Schema>::schema_id(),
            name: "quake-bar",
            topic: jinn_quake_bar::command::quake_bar_topic(),
            direction: jinn_slices::host::Direction::Forward,
        },
    )
    .await;
}

/// Drains the discord slice's staged forward routes into per-route
/// relays on the shared `jinn.session` topic.
async fn jinn_discord_drain(services: &Services) {
    use jinn_discord_msg::{
        CreateThreadForSession, DiscordThreadCreateFailed, DiscordThreadCreated,
    };
    use jinn_session_msg::{
        SessionArchived, SessionPhaseChanged, SessionSetupCompleted, SessionTeardownFinished,
        session_topic,
    };

    let topic = session_topic();
    let route = |schema_id| jinn_slices::host::RouteEntry {
        schema_id,
        name: "discord",
        topic: topic.clone(),
        direction: jinn_slices::host::Direction::Forward,
    };
    jinn_domain::common::trouper_bridge::spawn_one::<SessionPhaseChanged>(
        services,
        &route(<SessionPhaseChanged as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<SessionSetupCompleted>(
        services,
        &route(<SessionSetupCompleted as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<SessionTeardownFinished>(
        services,
        &route(<SessionTeardownFinished as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<SessionArchived>(
        services,
        &route(<SessionArchived as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<CreateThreadForSession>(
        services,
        &route(<CreateThreadForSession as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<DiscordThreadCreated>(
        services,
        &route(<DiscordThreadCreated as trouper::schema::Schema>::schema_id()),
    )
    .await;
    jinn_domain::common::trouper_bridge::spawn_one::<DiscordThreadCreateFailed>(
        services,
        &route(<DiscordThreadCreateFailed as trouper::schema::Schema>::schema_id()),
    )
    .await;
}

/// Activates the discord slice over the kernel's registries.
///
/// Composition assembles the `SliceHost` borrows plus the services the
/// slice's kameo-side bridge actor needs; the slice returns the parked
/// gateway channels and its validated config for the frontend spawn.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
async fn jinn_discord_activate(
    services: &mut Services,
    state: jinn_domain::common::state::State,
) -> jinn_discord::ActivatedDiscord {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    // Config-section resolution sink: reads the user-preferences
    // document's raw tables (slice-owned sections survive there). Built
    // before activation — the slice applies its sections during
    // `activate`, before reading its `[discord]` value.
    let prefs = services.user_preferences_storage.clone();
    let sink = move |key: &str| prefs.raw_section(key);
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let activated = jinn_discord::activate(&mut host, &services_snapshot, state, &sink)
        .await
        .unwrap_or_else(|error| panic!("discord slice activation failed: {error}"));
    if let Err(error) = host.finalize(&sink) {
        panic!("discord slice finalize failed: {error}");
    }
    activated
}

/// Activates the session-init slice over the kernel's registries.
///
/// The slice installs the discovery partition set, spawns its trouper
/// actors, and stages the crossing routes; `finalize` collects the
/// staged set so the drain's relays match. Slice integration is
/// exactly this call plus `bridge::drain_routes`.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn jinn_session_init_activate(services: &mut Services, state: jinn_domain::common::state::State) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    if let Err(error) = jinn_session_init::activate(&mut host, &services_snapshot, state) {
        panic!("session-init slice activation failed: {error}");
    }
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("session-init slice finalize failed: {error}");
    }
}

/// The `TermHandle` implementation over the coordinator's actor ref.
///
/// Lives with the term slice's wiring; the actor type stays private to
/// the slice once the feature tree moves.
#[derive(Debug, Clone)]
pub struct ActorTermHandle {
    coordinator: kameo::actor::ActorRef<jinn_term::interactive_term_actor::InteractiveTermActor>,
}

impl ActorTermHandle {
    pub fn new(
        coordinator: kameo::actor::ActorRef<
            jinn_term::interactive_term_actor::InteractiveTermActor,
        >,
    ) -> Self {
        Self { coordinator }
    }
}

#[async_trait::async_trait]
impl jinn_term_msg::TermHandle for ActorTermHandle {
    async fn spawn_term(
        &self,
        chat_session_id: jinn_domain::protocol::SessionId,
        command: String,
        cwd: std::path::PathBuf,
        size: (u16, u16),
        max_wait: std::time::Duration,
    ) -> Result<jinn_term_msg::SpawnTermOutcome, jinn_term_msg::TermAskError> {
        let msg = jinn_term_msg::SpawnTerm {
            chat_session_id,
            command,
            cwd,
            size,
            max_wait,
        };
        self.coordinator
            .ask(msg)
            .await
            .map_err(|_| jinn_term_msg::TermAskError)
    }

    async fn send_input(
        &self,
        chat_session_id: jinn_domain::protocol::SessionId,
        text: Option<String>,
        keys: Vec<String>,
        enter: bool,
        max_wait: std::time::Duration,
    ) -> Result<jinn_term_msg::SendTermOutcome, jinn_term_msg::TermAskError> {
        let msg = jinn_term_msg::SendTermInput {
            chat_session_id,
            text,
            keys,
            enter,
            max_wait,
        };
        self.coordinator
            .ask(msg)
            .await
            .map_err(|_| jinn_term_msg::TermAskError)
    }

    async fn kill_term(
        &self,
        chat_session_id: jinn_domain::protocol::SessionId,
    ) -> Result<jinn_term_msg::KillTermOutcome, jinn_term_msg::TermAskError> {
        self.coordinator
            .ask(jinn_term_msg::KillTerm { chat_session_id })
            .await
            .map_err(|_| jinn_term_msg::TermAskError)
    }

    fn name(&self) -> &'static str {
        "term-coordinator"
    }
}
