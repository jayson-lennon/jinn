//! The composition root: builds the actor system at launch.
//!
//! This file is the *sequence* — construct shared state, build the bus and
//! bridge, hand off to the boot list, run the startup tail, wait for
//! readiness. The slice activations themselves live in
//! [`crate::bootstrap::slices`], in one ordered list, so the complete boot
//! wiring is readable in one place rather than interleaved with setup here.
//!
//! # Spawn order
//!
//! 1. Every slice activation, in [`crate::bootstrap::slices`].
//! 2. The startup tail, once every subscription above exists.
//!
//! EnvInitActor is spawned first within the boot trio so dependent actors
//! can pull config from it via `ask()` on its path.

use jinn_kernel::ApiKeysService;
use jinn_kernel::AppState;
use jinn_kernel::ConfigStorageService;
use jinn_kernel::LlmServiceFactoryService;
use jinn_kernel::ProviderRegistryService;
use jinn_kernel::Services;
use jinn_session_state::SessionStoreService;

use jinn_kernel::{AppCore, State};

use crate::bootstrap::slices::ActivateError;
use crate::bootstrap::slices::Activated;
use marked_path::{Absolute, MarkedPathBuf};

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
    /// The configuration layer: the live `jinn.toml` every consumer reads.
    pub config: jinn_config::ConfigLayer,
    /// App state storage service.
    pub app_state_storage: jinn_preferences_config::AppStateStorageService,
    /// The shells jinn runs commands through, resolved at launch.
    pub shells: jinn_common::shell::Shells,
    /// Application paths.
    pub paths: jinn_kernel::AppPaths,
    /// Dump directory for provider request debugging. `None` disables.
    pub dump_requests: Option<MarkedPathBuf<Absolute>>,
    /// The compaction system prompt loaded from the prompts directory at
    /// startup (the compaction worker consumes it on each compaction).
    pub compaction_prompt: String,
}

/// Builds the actor system: spawns all actors on trouper.
///
/// Construct with [`ActorSystemBuilder::new`], then call
/// [`ActorSystemBuilder::build`]. After spawning all actors, `build` blocks
/// the calling thread until the actor system signals readiness (3s timeout).
///
/// # Errors
///
/// Returns an error if a slice activation fails. A slice that cannot
/// register its cell is a broken wiring, and aborting launch is the only
/// honest outcome.
pub struct ActorSystemBuilder {
    args: ActorSystemBuilderArgs,
}

impl ActorSystemBuilder {
    #[must_use]
    pub fn new(args: ActorSystemBuilderArgs) -> Self {
        Self { args }
    }

    /// Spawns every actor, builds the bus and bridge, and waits for readiness.
    ///
    /// This is the composition sequence and nothing else. The slice
    /// activations it performs live in [`crate::bootstrap::slices`], in one
    /// ordered list, so the wiring is readable in one place.
    ///
    /// # Errors
    ///
    /// Returns an error if a slice activation fails.
    pub async fn build(
        self,
    ) -> Result<(AppCore, Services, jinn_discord::ActivatedDiscord), ActivateError> {
        let ActorSystemBuilderArgs {
            handle,
            llm_service,
            provider_registry,
            api_keys,
            config_storage,
            session_store,
            config,
            app_state_storage,
            shells,
            paths,
            dump_requests,
            compaction_prompt,
        } = self.args;

        // Shared State FIRST — injected into multiple actors.
        let state = State::new(AppState::default());

        // The in-flight subagent registry: one handle shared by the tool
        // orchestrator (writer) and the app state the intent handler reads
        // (the confirmed-cancel cascade). Stamped before any actor spawns,
        // so a `spawn_subagent` call racing an early cancel cannot miss the pair.
        let task_spawns = jinn_tools_msg::TaskSpawnRegistry::default();
        state.write().task_spawns = task_spawns.clone();

        {
            let app_state = app_state_storage.read();
            let mut guard = state.write();
            guard.frontend.app_state.last_model = app_state.last_model.clone();
            guard.frontend.app_state.theme_name = app_state.theme_name.clone();
            guard.frontend.app_state.persona_name = app_state.persona_name.clone();
            guard.frontend.app_state.sidebar_width = app_state.sidebar_width;
        }

        // Default CWD for sessions (inherited from the shell).
        let (initial_session_id, initial_cwd) = {
            let cwd = {
                let raw = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
                marked_path::MarkedPathBuf::<marked_path::Absolute>::new(raw)
                    .expect("cwd and / are absolute")
            };
            let mut guard = state.write();
            guard.session.set_default_cwd(cwd.clone());
            guard.active_session_mut().set_cwd(cwd.clone());
            (guard.active_session().session_id().clone(), cwd)
        };

        // The message fabric: the trouper actor system plus the bridge.
        let (bus, trouper_system) = {
            let system =
                trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
            (
                jinn_kernel::common::services::bus_service::BusService::new_trouper(system.clone()),
                system,
            )
        };
        let bridge = jinn_kernel::common::bridge::Bridge::with_system(&bus, &handle);

        let mut services = Services {
            paths: paths.clone(),
            handle: handle.clone(),
            llm_service: llm_service.clone(),
            provider_registry: provider_registry.clone(),
            api_keys: api_keys.clone(),
            config_storage: config_storage.clone(),
            session_store: session_store.clone(),
            config,
            app_state_storage: app_state_storage.clone(),
            shells: shells.clone(),
            tempdir: None,
            bus,
            bridge: bridge.clone(),
            trouper_system,
            mcp_coordinator: std::sync::Arc::new(std::sync::OnceLock::new()),
            canvas_server: std::sync::Arc::new(std::sync::OnceLock::new()),
            interactive_term: std::sync::Arc::new(std::sync::OnceLock::new()),
            request_dump: jinn_kernel::common::request_dump::RequestDumpService::new(dump_requests),
            task_spawns,
            slices: jinn_slices::Slices::new(),
            key_routes: jinn_slices::route::KeyRoutes::new(),
            viewport: jinn_slices::view::Viewport::new(),
            overlay_views: jinn_slices::OverlayViews::new(),
            project_picker: None,
            turn_ledger: jinn_kernel::common::turn_ledger::TurnLedger::default(),
        };

        // The shell was resolved at launch, in `bootstrap::config`, and
        // rides on the services container from here: the environment is a
        // global namespace and is not consulted outside of program
        // startup. Lifecycle scripts promise bash semantics, not the
        // user's login shell — see `jinn_common::shell`.
        let mut ctx = crate::bootstrap::Ctx::new(&state, &mut services, compaction_prompt);
        let Activated { boot, discord } = crate::bootstrap::slices::activate_all(&mut ctx).await?;

        // The frontend facade resolves the scope-focus and session-map
        // handles through the attached registry.
        state
            .write()
            .frontend
            .attach_slices(services.slices.clone());
        state.read().session.attach_slices(services.slices.clone());

        // ── Startup tail ────────────────────────────────────────────
        // Runs only after every activation above: each actor's
        // subscription now exists, so none of these publishes can race
        // the wiring it is meant to trigger.
        {
            let bus = services.bus.clone();

            // Personas: the persona slice scanned at activation; publish
            // now that every actor (the session actor subscribes to
            // `PersonasLoaded`) is spawned.
            if let Some(personas) = services
                .slices
                .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
                && !personas.read().entries.is_empty()
            {
                // Clone the entries out before the await: the read guard
                // must not be held across it.
                let entries = personas.read().entries.clone();
                bus.publish(jinn_persona_msg::PersonasLoaded {
                    personas: entries,
                    error: None,
                })
                .await;
            }

            // Every subscription above is live, so this signal certifies
            // the actor system rather than racing it.
            bus.publish(jinn_boot_msg::AllActorsSpawned).await;

            // Ask EnvInitActor for config and publish EnvironmentLoaded to
            // trigger the init chain. The trouper ask has a MANDATORY
            // timeout; this is the one real startup ask path.
            use jinn_boot_msg::GetEnvironmentConfig;
            const ENV_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
            match services
                .trouper_system
                .ask(
                    boot.env_init_path.clone(),
                    GetEnvironmentConfig,
                    ENV_ASK_TIMEOUT,
                )
                .await
            {
                Ok(value) => match value
                    .decode::<jinn_boot_msg::EnvironmentConfigReply>()
                    .expect("env reply decodes")
                    .config
                {
                    Some(config) => {
                        bus.publish(jinn_boot_msg::EnvironmentLoaded { config })
                            .await;
                    }
                    None => {
                        tracing::warn!("no provider config found — skipping EnvironmentLoaded");
                    }
                },
                Err(e) => {
                    tracing::error!(err = ?e, "failed to get environment config from EnvInitActor");
                }
            }

            // The boot session's cwd is known here; the session-init
            // supervisor routes from payloads, not shared state. This
            // publish triggers the initial session's discovery through the
            // same payload path as every other session.
            bus.publish(jinn_session_lifecycle_msg::SessionCwdChanged {
                session_id: initial_session_id.clone(),
                cwd: initial_cwd,
            })
            .await;
        }

        // Block until SystemReadyActor confirms readiness.
        let _ = boot.ready_rx.to_async().recv().await;

        let core = AppCore {
            state: state.clone(),
            bridge: services.bridge.clone(),
        };

        Ok((core, services, discord))
    }
}
/// The `TermHandle` implementation over the coordinator's trouper path.
///
/// Asks route through the system at the coordinator's static path with the
/// mandatory trouper timeout: the settle wait inside the actor already
/// bounds by `max_wait`, so the outer timeout adds a margin for the round
/// trip (`ask_timeout + SPAWN_ASK_MARGIN` pattern).
#[derive(Debug, Clone)]
pub struct ActorTermHandle {
    system: trouper::system::ActorSystem,
    path: trouper::actor::ActorPath,
}

impl ActorTermHandle {
    pub fn new(system: trouper::system::ActorSystem, path: trouper::actor::ActorPath) -> Self {
        Self { system, path }
    }
}

#[async_trait::async_trait]
impl jinn_term_msg::TermHandle for ActorTermHandle {
    async fn spawn_term(
        &self,
        chat_session_id: jinn_core_types::SessionId,
        command: String,
        cwd: MarkedPathBuf<Absolute>,
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
        let reply = self
            .system
            .ask(self.path.clone(), msg, TERM_ASK_TIMEOUT)
            .await;
        decode_reply::<jinn_term_msg::SpawnTermOutcome>(reply)
    }

    async fn send_input(
        &self,
        chat_session_id: jinn_core_types::SessionId,
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
        let reply = self
            .system
            .ask(self.path.clone(), msg, TERM_ASK_TIMEOUT)
            .await;
        decode_reply::<jinn_term_msg::SendTermOutcome>(reply)
    }

    async fn kill_term(
        &self,
        chat_session_id: jinn_core_types::SessionId,
    ) -> Result<jinn_term_msg::KillTermOutcome, jinn_term_msg::TermAskError> {
        let reply = self
            .system
            .ask(
                self.path.clone(),
                jinn_term_msg::KillTerm { chat_session_id },
                TERM_ASK_TIMEOUT,
            )
            .await;
        decode_reply::<jinn_term_msg::KillTermOutcome>(reply)
    }

    fn name(&self) -> &'static str {
        "term-coordinator"
    }
}

/// Outer bound for term asks: the settle wait inside the actor bounds by
/// the message's `max_wait`, so this covers the whole round trip with a
/// margin (the `ask_timeout + SPAWN_ASK_MARGIN` pattern).
const TERM_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// Decodes a trouper ask reply into the typed outcome, mapping transport
/// failure to the domain-level [`jinn_term_msg::TermAskError`].
fn decode_reply<T: serde::de::DeserializeOwned>(
    reply: Result<trouper::json::Json, error_stack::Report<trouper::context::AskError>>,
) -> Result<T, jinn_term_msg::TermAskError> {
    reply
        .ok()
        .and_then(|value| value.decode::<T>().ok())
        .ok_or(jinn_term_msg::TermAskError)
}
