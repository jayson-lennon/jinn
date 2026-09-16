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
use jinn_domain::UserPreferencesStorageService;
use jinn_quake_bar;
use jinn_slices;

use jinn_domain::common::actor_deps::ActorDeps;
use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;

use jinn_domain::init::env_init_actor::{EnvInitActor, EnvInitActorDeps};
use jinn_domain::init::provider_init_actor::{ProviderInitActor, ProviderInitActorDeps};
use jinn_domain::init::system_ready_actor::{SystemReadyActor, SystemReadyActorDeps};

use jinn_domain::feat::browser_binary_scan::{
    BrowserBinaryScanActor, BrowserBinaryScanActorDeps, SystemBinaryLocator, resolve_browser_binary,
};
use jinn_domain::feat::preferences_actor::user_preferences::WebFetchBackend;
use jinn_domain::feat::web_fetch_actor::{WebFetchActor, WebFetchActorDeps};
use jinn_domain::feat::web_search_actor::{WebSearchActor, WebSearchActorDeps};
#[cfg(feature = "headless-chrome")]
use jinn_web_fetch::stealth::StealthSettings;
use jinn_web_fetch::{CleanMarkdownExtractor, HttpFetcher, MarkdownExtractor, OutputFormat};
use jinn_web_search::DdgSearcher;

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
    pub app_state_storage: jinn_domain::feat::preferences_actor::AppStateStorageService,
    /// Application paths.
    pub paths: jinn_domain::AppPaths,
    /// Override for the persistent browser profile base directory.
    /// When `Some`, per-mode profiles live under `<dir>/headless` and `<dir>/headed`.
    /// When `None`, defaults to `AppPaths::browser_profile_base_dir()`.
    pub browser_profile_override: Option<std::path::PathBuf>,
    /// Dump directory for provider request debugging. `None` disables.
    pub dump_requests: Option<std::path::PathBuf>,
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
            browser_profile_override,
            dump_requests,
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

        // Create the kameo message bus and closure bridge.
        let bus = {
            let bus_actor = kameo_actors::message_bus::MessageBus::new(
                kameo_actors::DeliveryStrategy::BestEffort,
            );
            let bus_ref = kameo_actors::message_bus::MessageBus::spawn(bus_actor);
            jinn_domain::common::services::bus_service::BusService::new(bus_ref)
        };
        let bridge = jinn_domain::common::bridge::Bridge::new(bus.actor_ref().clone());

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
            root_supervisor: root.clone(),
            mcp_coordinator: std::sync::Arc::new(std::sync::OnceLock::new()),
            interactive_term: std::sync::Arc::new(std::sync::OnceLock::new()),
            request_dump: jinn_domain::common::request_dump::RequestDumpService::new(dump_requests),
            task_spawns: jinn_domain::feat::tools_actor::task_registry::TaskSpawnRegistry::default(
            ),
            slices: jinn_domain::common::slices::Slices::new(),
            key_routes: jinn_domain::common::slices::key_routes::KeyRoutes::new(),
            viewport: jinn_domain::common::slices::view::Viewport::new(),
            overlay_views: jinn_domain::common::overlay_views::OverlayViews::new(),
            trouper_system: trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ),
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
        jinn_sidebar_activate(&mut services);
        jinn_theme_activate(&mut services);

        // Persona slice: activation scans the persona directories and
        // mints the personas cell; the returned set is published as
        // `PersonasLoaded` below, after the session actor (its sole
        // subscriber) has spawned — the plugin's push-once contract.
        let persona_entries = jinn_persona_activate(&mut services);

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

        // Preferences: loads and persists user preferences.
        let _preferences =
            spawn_tracked!(&services.bus, "preferences", "PreferencesActor",
jinn_domain::feat::preferences_actor::preferences_actor::PreferencesActor::supervise(
                    &root,
                    jinn_domain::feat::preferences_actor::preferences_actor::PreferencesActorDeps {
                        deps: actor_deps.clone(),
                        state: state.clone(),
                        cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
        );

        // App state actor: persists state changes to state.toml.
        let _app_state = spawn_tracked!(
            &services.bus,
            "app-state",
            "AppStateActor",
            jinn_domain::feat::preferences_actor::app_state_actor::AppStateActor::supervise(
                &root,
                jinn_domain::feat::preferences_actor::app_state_actor::AppStateActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    context_cap: jinn_domain::common::tcaps::mint::mint_context_cap(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // ── Domain actors ──────────────────────────────────────────────────

        // LLM streaming actor.
        let _llm = spawn_tracked!(
            &services.bus,
            "llm",
            "LlmActor",
            jinn_domain::feat::llm_actor::LlmActor::supervise(
                &root,
                jinn_domain::feat::llm_actor::LlmActorDeps {
                    factory: llm_service.clone(),
                    deps: actor_deps.clone(),
                    state: state.clone(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

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
        //
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
        // Shared entry-token cache for history workers — created here so the
        // session actor's accumulation gate can read it too.
        let entry_token_cache =
            jinn_domain::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache::new();
        let _session =
            jinn_domain::feat::session::session_actor::SessionPersistenceActor::supervise(
                &root,
                jinn_domain::feat::session::session_actor::SessionPersistenceActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    context_cap: jinn_domain::common::tcaps::mint::mint_context_cap(),
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

        // Tool orchestrator actor.
        let _tools = spawn_tracked!(
            &services.bus,
            "tool-orchestrator",
            "ToolOrchestratorActor",
            jinn_domain::feat::tools_actor::ToolOrchestratorActor::supervise(
                &root,
                jinn_domain::feat::tools_actor::ToolOrchestratorActorDeps {
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
            jinn_domain::feat::mcp_coordinator_actor::McpCoordinatorActor::supervise(
                &root,
                jinn_domain::feat::mcp_coordinator_actor::McpCoordinatorActorDeps {
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
        // Expose the coordinator ref to the tool layer (restart_mcp_server).
        // `OnceLock::set` returns Err if already set; ignore (e.g. test re-seed).
        let _ = services.mcp_coordinator.set(_mcp_coordinator.clone());

        // Interactive-term coordinator: owns PTY sessions across tool calls
        // (the `interactive_term*` tools ask it directly). Spawned with the
        // same lifecycle shape as the MCP coordinator; the per-session
        // control registry goes to the terminal tab (takeover UI) wiring.
        let term_controls =
            jinn_domain::feat::interactive_term::interactive_term_actor::TermControls::default();
        let (term_coordinator, _controls) =
            jinn_domain::feat::interactive_term::interactive_term_actor::spawn_interactive_term_actor(
                jinn_domain::feat::interactive_term::interactive_term_actor::InteractiveTermActorDeps {
                    bus: services.bus.clone(),
                    controls: term_controls.clone(),
                    state: state.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    settle_quiet: std::time::Duration::from_millis(
                        state.read().frontend.preferences.interactive_term.settle_quiet_ms,
                    ),
                    settle_cap: std::time::Duration::from_millis(
                        state.read().frontend.preferences.interactive_term.settle_max_wait_ms,
                    ),
                },
                &root,
            )
            .await;
        let _ = services.interactive_term.set(term_coordinator);
        // Install the shared registry for the IntentHandler's takeover
        // intents (synchronous flips that in-flight tool calls observe
        // mid-drain).
        let _ =
            jinn_domain::feat::interactive_term::takeover_intent::TERM_CONTROLS.set(term_controls);

        // Plugin lifecycle actor: reads `[[plugin]]` entries from jinn.toml and spawns one in-process
        // WASM guest per entry. Guests are hosted directly by jinn via the
        // shared wasmtime engine — no child processes. Spawned after MCP so
        // contributions land once the bus is fully populated.
        let _plugin_coordinator = spawn_tracked!(
            &services.bus,
            "plugin-coordinator",
            "PluginCoordinatorActor",
            jinn_domain::feat::plugin_coordinator_actor::PluginCoordinatorActor::supervise(
                &root,
                jinn_domain::feat::plugin_coordinator_actor::PluginCoordinatorActorDeps {
                    deps: actor_deps.clone(),
                    root: root.clone(),
                    state: state.clone(),
                    cap: jinn_domain::common::tcaps::mint::mint_plugins_cap(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    dirs: jinn_domain::feat::plugin_coordinator_actor::PluginDirs {
                        config_dir: services.paths.app_config_dir(),
                        data_dir: services.paths.app_data_dir(),
                        engine: std::sync::Arc::new(
                            jinn_plugin::PluginEngine::new()
                                .expect("wasmtime engine construction cannot fail"),
                        ),
                    },
                    tick_override: None,
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );
        _plugin_coordinator.wait_for_startup().await;

        // Web fetch + web search actors.
        //
        // Browser-backed tools share one process per MODE (headless/headed): if
        // both tools select the same mode they attach to the same SharedBrowser
        // and warm one profile together. Each mode gets its own persistent profile
        // dir so a warmed headed profile survives restarts.
        let web_fetch_config = user_preferences_storage.read().web_fetch.clone();
        let web_search_config = user_preferences_storage.read().web_search.clone();
        let browser_config = user_preferences_storage.read().browser.clone();
        let web_fetch_backend = web_fetch_config.backend;
        let web_search_backend = web_search_config.backend;
        // Browser-backed backends require the `headless-chrome` feature. When
        // the binary is built without it, fall back to HTTP and say so — a
        // browser preference must not be silently ignored.
        #[cfg(not(feature = "headless-chrome"))]
        let (web_fetch_backend, web_search_backend) = {
            let fallback = |backend, tool| match backend {
                WebFetchBackend::Http => backend,
                other @ (WebFetchBackend::HeadlessChrome | WebFetchBackend::HeadedChrome) => {
                    tracing::warn!(
                        ?other,
                        tool,
                        "backend requires the headless-chrome feature (not compiled in); using http"
                    );
                    WebFetchBackend::Http
                }
            };
            (
                fallback(web_fetch_backend, "web-fetch"),
                fallback(web_search_backend, "web-search"),
            )
        };
        tracing::info!(
            ?web_fetch_backend,
            ?web_search_backend,
            "constructing web tools"
        );

        let extractors = {
            let markdown: std::sync::Arc<dyn jinn_web_fetch::Extractor> =
                std::sync::Arc::new(MarkdownExtractor);
            let clean: std::sync::Arc<dyn jinn_web_fetch::Extractor> =
                std::sync::Arc::new(CleanMarkdownExtractor);
            std::collections::HashMap::from([
                (OutputFormat::MarkdownClean, clean),
                (OutputFormat::Markdown, markdown),
            ])
        };

        // Resolve the browser binary + UA once. Both modes (headless/headed) and
        // the http search path all use this same UA so a browser-blocked site and
        // a plain-http request look like the same client.
        let resolved = resolve_browser_binary(browser_config.binary, &SystemBinaryLocator);
        tracing::info!(
            family = ?resolved.family,
            path = ?resolved.path,
            version = ?resolved.version_major,
            note = ?resolved.fallback_note,
            "web tools: browser binary resolved"
        );
        let resolved_user_agent = browser_config.user_agent.clone().unwrap_or_else(|| {
            let major = resolved
                .version_major
                .as_deref()
                .unwrap_or(jinn_web_fetch::stealth::CHROME_MAJOR);
            jinn_web_fetch::stealth::build_user_agent(major)
        });

        // Resolve the profile base dir: explicit CLI override wins, else AppPaths.
        #[cfg(feature = "headless-chrome")]
        let profile_base: std::path::PathBuf =
            browser_profile_override.unwrap_or_else(|| paths.browser_profile_base_dir());
        // No browser profiles exist without the headless-chrome feature.
        #[cfg(not(feature = "headless-chrome"))]
        let _ = browser_profile_override;

        // Build one SharedBrowser per active mode. A mode is active when either tool
        // selects it. The handle is cached so both tools sharing a mode reuse it.
        #[cfg(feature = "headless-chrome")]
        {
            use jinn_domain::BrowserProfileMode;
            use std::collections::HashMap;
            let build_shared =
                |mode: BrowserProfileMode| -> std::sync::Arc<jinn_web_fetch::SharedBrowser> {
                    let headed = matches!(mode, BrowserProfileMode::Headed);
                    let mut stealth = StealthSettings::from(&browser_config);
                    stealth.headed = headed;
                    stealth.binary_path = resolved.path.clone();
                    stealth.user_agent = resolved_user_agent.clone();
                    stealth.profile_dir = Some(profile_base.join(mode.as_str()));
                    tracing::info!(?stealth, "web tools: building shared browser for mode");
                    std::sync::Arc::new(jinn_web_fetch::SharedBrowser::new(stealth))
                };
            let shared_for = |backend: WebFetchBackend| -> Option<BrowserProfileMode> {
                match backend {
                    WebFetchBackend::Http => None,
                    WebFetchBackend::HeadlessChrome => Some(BrowserProfileMode::Headless),
                    WebFetchBackend::HeadedChrome => Some(BrowserProfileMode::Headed),
                }
            };
            let fetch_mode = shared_for(web_fetch_backend);
            let search_mode = shared_for(web_search_backend);
            let mut shared_by_mode: HashMap<
                BrowserProfileMode,
                std::sync::Arc<jinn_web_fetch::SharedBrowser>,
            > = HashMap::new();
            for mode in [fetch_mode, search_mode].into_iter().flatten() {
                shared_by_mode
                    .entry(mode)
                    .or_insert_with(|| build_shared(mode));
            }

            // Spawn one detached keepalive heartbeat per active SharedBrowser. Each
            // loop polls [`SharedBrowser::probe`] inside a blocking task bounded by
            // [`PROBE_TIMEOUT`]; a timeout or panic force-evicts so the next request
            // lazily relaunches a fresh browser rather than hanging on a dead
            // WebSocket. The task is detached (handle dropped) — it lives for the
            // process lifetime and is independent of any actor lifecycle.
            for shared in shared_by_mode.values() {
                let shared = shared.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(jinn_web_fetch::HEARTBEAT_INTERVAL).await;
                        let probe = tokio::task::spawn_blocking({
                            let shared = shared.clone();
                            move || shared.probe()
                        });
                        match tokio::time::timeout(jinn_web_fetch::PROBE_TIMEOUT, probe).await {
                            Ok(Ok(())) => {}
                            // probe() returns () — it evicts internally on failure,
                            // so an Ok is no-op here. The remaining arms cover the
                            // wedge cases probe() can't observe on its own.
                            Ok(Err(join_err)) => {
                                tracing::warn!(
                                    ?join_err,
                                    "keepalive: probe task panicked, force-evicting"
                                );
                                shared.force_evict();
                            }
                            Err(_elapsed) => {
                                tracing::warn!("keepalive: probe timed out, force-evicting");
                                shared.force_evict();
                            }
                        }
                    }
                });
            }

            // Construct the fetcher.
            let web_fetcher: std::sync::Arc<dyn jinn_web_fetch::WebFetcher> = match fetch_mode {
                None => {
                    tracing::debug!("web-fetch: using HttpFetcher backend");
                    std::sync::Arc::new(HttpFetcher::new(extractors.clone()))
                }
                Some(mode) => {
                    tracing::debug!(?mode, "web-fetch: using shared browser backend");
                    let shared = shared_by_mode
                        .get(&mode)
                        .expect("shared browser built for active mode")
                        .clone();
                    std::sync::Arc::new(jinn_web_fetch::HeadlessChromeFetcher::with_shared(
                        shared,
                        extractors.clone(),
                    ))
                }
            };
            let _web_fetch = spawn_tracked!(
                &services.bus,
                "web-fetch",
                "WebFetchActor",
                WebFetchActor::supervise(
                    &root,
                    WebFetchActorDeps {
                        deps: actor_deps.clone(),
                        web_fetcher,
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );

            // Construct the searcher.
            let web_searcher: std::sync::Arc<dyn jinn_web_search::WebSearcher> = match search_mode {
                None => {
                    tracing::debug!("web-search: using reqwest DdgSearcher backend");
                    std::sync::Arc::new(DdgSearcher::with_endpoint_and_user_agent(
                        "https://html.duckduckgo.com/html".to_owned(),
                        resolved_user_agent.as_str(),
                    ))
                }
                Some(mode) => {
                    tracing::debug!(?mode, "web-search: using browser-backed DdgSearcher");
                    let shared = shared_by_mode
                        .get(&mode)
                        .expect("shared browser built for active mode")
                        .clone();
                    std::sync::Arc::new(jinn_web_search::BrowserDdgSearcher::new(shared))
                }
            };
            let _web_search = spawn_tracked!(
                &services.bus,
                "web-search",
                "WebSearchActor",
                WebSearchActor::supervise(
                    &root,
                    WebSearchActorDeps {
                        deps: actor_deps.clone(),
                        web_searcher,
                        config: web_search_config,
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

        // Without the headless-chrome feature both tools run on plain HTTP.
        #[cfg(not(feature = "headless-chrome"))]
        {
            let web_fetcher: std::sync::Arc<dyn jinn_web_fetch::WebFetcher> =
                std::sync::Arc::new(HttpFetcher::new(extractors.clone()));
            let _web_fetch = spawn_tracked!(
                &services.bus,
                "web-fetch",
                "WebFetchActor",
                WebFetchActor::supervise(
                    &root,
                    WebFetchActorDeps {
                        deps: actor_deps.clone(),
                        web_fetcher,
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );

            let web_searcher: std::sync::Arc<dyn jinn_web_search::WebSearcher> =
                std::sync::Arc::new(DdgSearcher::with_endpoint_and_user_agent(
                    "https://html.duckduckgo.com/html".to_owned(),
                    resolved_user_agent.as_str(),
                ));
            let _web_search = spawn_tracked!(
                &services.bus,
                "web-search",
                "WebSearchActor",
                WebSearchActor::supervise(
                    &root,
                    WebSearchActorDeps {
                        deps: actor_deps.clone(),
                        web_searcher,
                        config: web_search_config,
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

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

        let _token_count = spawn_tracked!(
            &services.bus,
            "token-count",
            "TokenCountActor",
            jinn_domain::feat::token_count_actor::TokenCountActor::supervise(
                &root,
                jinn_domain::feat::token_count_actor::TokenCountActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
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

        // Queue actor.
        let _queue = spawn_tracked!(
            &services.bus,
            "queue",
            "QueueActor",
            jinn_domain::feat::queue_actor::QueueActor::supervise(
                &root,
                jinn_domain::feat::queue_actor::QueueActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    counter: token_counter,
                    cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Context size actor.
        let _context_size = spawn_tracked!(
            &services.bus,
            "context-size",
            "ContextSizeActor",
            jinn_domain::feat::context::context_size_actor::ContextSizeActor::supervise(
                &root,
                jinn_domain::feat::context::context_size_actor::ContextSizeActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    counter: token_counter,
                    session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // ── History mutation workers ───────────────────��──────────────────────
        //
        // To add a new history mutation worker:
        //
        //   1. Implement `HistoryWorker` for your heuristic type
        //      (see `crates/jinn-domain/src/feat/history_worker/worker_trait.rs`).
        //   2. Add a spawn call here following the pattern below.

        // History snapshot actor.
        {
            use jinn_domain::feat::history_worker::snapshot_actor::{
                HistorySnapshotActor, HistorySnapshotActorDeps,
            };

            let _snapshot = spawn_tracked!(
                &services.bus,
                "history-snapshot",
                "HistorySnapshotActor",
                HistorySnapshotActor::supervise(
                    &root,
                    HistorySnapshotActorDeps {
                        deps: actor_deps.clone(),
                        state: state.clone(),
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

        // Compaction worker.
        {
            use jinn_domain::feat::compaction_worker::CompactionWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let _compaction = spawn_tracked!(
                &services.bus,
                "history-compaction",
                "HistoryWorker<CompactionWorker>",
                HistoryWorkerActor::<CompactionWorker>::supervise(
                    &root,
                    HistoryWorkerActorDeps {
                        deps: actor_deps.clone(),
                        worker: CompactionWorker::new(
                            services.clone(),
                            handle.clone(),
                            state.clone(),
                            jinn_domain::common::tcaps::mint::mint_session_cap(),
                        ),
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

        // Compaction trigger actor.
        {
            use jinn_domain::feat::compaction_worker::{
                CompactionTriggerActor, CompactionTriggerActorDeps, CompactionWorker,
            };

            let _trigger = spawn_tracked!(
                &services.bus,
                "compaction-trigger",
                "CompactionTriggerActor",
                CompactionTriggerActor::supervise(
                    &root,
                    CompactionTriggerActorDeps {
                        deps: actor_deps.clone(),
                        worker: CompactionWorker::new(
                            services.clone(),
                            handle.clone(),
                            state.clone(),
                            jinn_domain::common::tcaps::mint::mint_session_cap(),
                        ),
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

        // Auto-prune worker: read→edit context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::ReadEditAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.read_edit;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-read-edit",
                    "HistoryWorker<ReadEditAutoPruneWorker>",
                    HistoryWorkerActor::<ReadEditAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: ReadEditAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: edit→read context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::EditReadAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.edit_read;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-edit-read",
                    "HistoryWorker<EditReadAutoPruneWorker>",
                    HistoryWorkerActor::<EditReadAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: EditReadAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: regex-based tool call pruning.
        {
            use jinn_domain::feat::auto_prune_worker::RegexAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };
            let regex_config = user_preferences_storage.read().auto_prune.regex.clone();

            if regex_config.enabled && !regex_config.rules.is_empty() {
                match RegexAutoPruneWorker::from_config(&regex_config) {
                    Ok(worker) => {
                        let _worker = spawn_tracked!(
                            &services.bus,
                            "history-regex",
                            "HistoryWorker<RegexAutoPruneWorker>",
                            HistoryWorkerActor::<RegexAutoPruneWorker>::supervise(
                                &root,
                                HistoryWorkerActorDeps {
                                    deps: actor_deps.clone(),
                                    worker,
                                },
                            )
                            .restart_policy(kameo::supervision::RestartPolicy::Never)
                            .spawn()
                            .await
                        );
                    }
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
        }

        // Auto-prune worker: todo tool call pruning.
        {
            use jinn_domain::feat::auto_prune_worker::TodoAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.todo;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-todo",
                    "HistoryWorker<TodoAutoPruneWorker>",
                    HistoryWorkerActor::<TodoAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: TodoAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: broken-edit context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::BrokenEditAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.broken_edit;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-broken-edit",
                    "HistoryWorker<BrokenEditAutoPruneWorker>",
                    HistoryWorkerActor::<BrokenEditAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: BrokenEditAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: double-edit context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::DoubleEditAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.double_edit;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-double-edit",
                    "HistoryWorker<DoubleEditAutoPruneWorker>",
                    HistoryWorkerActor::<DoubleEditAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: DoubleEditAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: consecutive-reads per-file pruning.
        {
            use jinn_domain::feat::auto_prune_worker::ConsecutiveReadsAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.consecutive_reads;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-consecutive-reads",
                    "HistoryWorker<ConsecutiveReadsAutoPruneWorker>",
                    HistoryWorkerActor::<ConsecutiveReadsAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: ConsecutiveReadsAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // HistoryWorkerChatEntryTokenCache eviction actor.

        // HistoryWorkerChatEntryTokenCache eviction actor.
        {
            use jinn_domain::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCacheEvictionActor;
            use jinn_domain::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCacheEvictionActorDeps;

            let _eviction = spawn_tracked!(
                &services.bus,
                "history-worker-chat-entry-token-cache-eviction",
                "HistoryWorkerChatEntryTokenCacheEvictionActor",
                HistoryWorkerChatEntryTokenCacheEvictionActor::supervise(
                    &root,
                    HistoryWorkerChatEntryTokenCacheEvictionActorDeps {
                        deps: actor_deps.clone(),
                        cache: entry_token_cache.clone(),
                    },
                )
                .restart_policy(kameo::supervision::RestartPolicy::Never)
                .spawn()
                .await
            );
        }

        // Auto-prune worker: tool-age-window context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::ToolAgeWindowAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.tool_age_window;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-tool-age-window",
                    "HistoryWorker<ToolAgeWindowAutoPruneWorker>",
                    HistoryWorkerActor::<ToolAgeWindowAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: ToolAgeWindowAutoPruneWorker { config },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: trivial-assistant context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::TrivialAssistantAutoPruneWorker;
            use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let config = user_preferences_storage.read().auto_prune.trivial_assistant;

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-trivial-assistant",
                    "HistoryWorker<TrivialAssistantAutoPruneWorker>",
                    HistoryWorkerActor::<TrivialAssistantAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: TrivialAssistantAutoPruneWorker {
                                config,
                                token_cache: entry_token_cache.clone(),
                                counter: TiktokenCounter::o200k_base(),
                            },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: anchor shield.
        {
            use jinn_domain::feat::auto_prune_worker::AnchorShieldAutoPruneWorker;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let shield_config = {
                let prefs = user_preferences_storage.read();
                prefs.auto_prune.anchor_shield
            };

            if shield_config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-anchor-shield",
                    "HistoryWorker<AnchorShieldAutoPruneWorker>",
                    HistoryWorkerActor::<AnchorShieldAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: AnchorShieldAutoPruneWorker {
                                config: shield_config,
                            },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Auto-prune worker: anchored-assistant context pruning.
        {
            use jinn_domain::feat::auto_prune_worker::AnchoredAssistantAutoPruneWorker;
            use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;
            use jinn_domain::feat::history_worker::actor::{
                HistoryWorkerActor, HistoryWorkerActorDeps,
            };

            let (config, shield_radius, trivial_max_tokens) = {
                let prefs = user_preferences_storage.read();
                let cfg = prefs.auto_prune.anchored_assistant.clone();
                let radius = prefs.auto_prune.anchor_shield.radius;
                let max_tokens = prefs.auto_prune.trivial_assistant.max_tokens as u32;
                (cfg, radius, max_tokens)
            };

            if config.enabled {
                let _worker = spawn_tracked!(
                    &services.bus,
                    "history-anchored-assistant",
                    "HistoryWorker<AnchoredAssistantAutoPruneWorker>",
                    HistoryWorkerActor::<AnchoredAssistantAutoPruneWorker>::supervise(
                        &root,
                        HistoryWorkerActorDeps {
                            deps: actor_deps.clone(),
                            worker: AnchoredAssistantAutoPruneWorker {
                                config,
                                radius: shield_radius,
                                min_candidate_tokens: trivial_max_tokens + 1,
                                token_cache: entry_token_cache.clone(),
                                counter: TiktokenCounter::o200k_base(),
                            },
                        },
                    )
                    .restart_policy(kameo::supervision::RestartPolicy::Never)
                    .spawn()
                    .await
                );
            }
        }

        // Sidebar state actor.
        let _sidebar = spawn_tracked!(
            &services.bus,
            "sidebar-state",
            "SidebarStateActor",
            jinn_sidebar::sections::sidebar_state_actor::SidebarStateActor::supervise(
                &root,
                jinn_sidebar::sections::sidebar_state_actor::SidebarStateActorDeps {
                    deps: actor_deps.clone(),
                    state: state.clone(),
                    frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
                    session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                },
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
        );

        // Browser binary scan: verifies the configured browser binary once at
        // startup (subscribes to EnvironmentLoaded). Not a session-scoped scan.
        let _browser_binary_scan = spawn_tracked!(
            &services.bus,
            "browser-binary-scan",
            "BrowserBinaryScanActor",
            BrowserBinaryScanActor::supervise(
                &root,
                BrowserBinaryScanActorDeps::new(actor_deps.clone(), browser_config.binary,),
            )
            .restart_policy(kameo::supervision::RestartPolicy::Never)
            .spawn()
            .await
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
fn jinn_sidebar_activate(services: &mut Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_sidebar::activate(&mut host);
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
fn jinn_persona_activate(services: &mut Services) -> jinn_slices::Personas {
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
