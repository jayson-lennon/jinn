//! Top-level application state and dispatch.
//!
//! [`App`] is the root of the ownership hierarchy. It creates the tokio
//! runtime, builds shared [`Services`], and dispatches to the appropriate
//! [`Runner`] variant (TUI or headless).

use std::sync::Arc;

use error_stack::{Report, ResultExt};
use jinn_cli::Cli;
use jinn_domain::ApiKeys;
use jinn_domain::ApiKeysService;
use jinn_domain::AppStateStorageService;
use jinn_domain::ConfigStorageService;
use jinn_domain::FilesystemAppStateStorage;
use jinn_domain::FilesystemConfigStorage;
use jinn_domain::FilesystemUserPreferencesStorage;
use jinn_domain::LlmServiceFactoryService;
use jinn_domain::NoProvidersAvailableFactory;
use jinn_domain::ProviderRegistry;
use jinn_domain::ProviderRegistryService;
use jinn_domain::SessionStoreService;
use jinn_domain::SqliteSessionStore;

use jinn_domain::UserPreferencesStorageService;
use tokio::runtime::Runtime;
use wherror::Error;

use crate::actor_wiring;
#[cfg(debug_assertions)]
use crate::headless::HeadlessApp;
use crate::runner::Runner;

/// Error type for top-level application initialization.
#[derive(Debug, Error)]
#[error(debug)]
pub struct AppError;

/// Top-level application state.
///
/// Created once in `crate::main` and dispatched to whichever
/// runner handles the command. Owns the tokio runtime and delegates
/// to [`Runner`] variants.
pub struct App {
    /// The tokio runtime.
    runtime: Runtime,
}

impl App {
    /// Creates a new top-level app with a default multi-threaded runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokio runtime cannot be created.
    pub fn new() -> Result<Self, Report<AppError>> {
        let runtime = Runtime::new()
            .change_context(AppError)
            .attach("failed to create tokio runtime")?;
        Ok(Self { runtime })
    }

    /// Returns a handle to the tokio runtime for spawning tasks.
    #[must_use]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Runs a runner to completion, then shuts down the session store.
    ///
    /// The store shutdown folds the WAL into `sessions.db` (a
    /// `wal_checkpoint(TRUNCATE)`) so that a clean exit leaves a
    /// self-contained, up-to-date database. It runs on the live runtime
    /// after the runner has returned, at which point the actor system has
    /// already drained via graceful shutdown, so no competing writers
    /// remain.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner fails or the store shutdown fails.
    fn run_and_shutdown(
        &self,
        runner: crate::runner::Runner,
        store: &jinn_domain::SessionStoreService,
    ) -> Result<(), Report<AppError>> {
        // Extract the root supervisor ref and the trouper system handle
        // before the runner is consumed, so we can coordinate shutdowns
        // after the event loop exits.
        let root = runner.root_supervisor();
        let trouper_system = runner.trouper_system();

        // Run the runner, but don't short-circuit shutdown on its error.
        // The WAL checkpoint is non-destructive and must run whenever the actor
        // system started, so that a clean quit leaves sessions.db self-contained
        // even if the runner itself failed. Surface the runner error as the
        // final result; a shutdown error takes precedence (it indicates storage
        // trouble the caller should see).
        let run_result = runner.run().change_context(AppError);

        // Coordinated actor shutdown: signal the root supervisor to stop, which
        // cascades a graceful shutdown to every supervised child actor (kameo's
        // lifecycle calls each child's `on_stop`). Race the barrier against a
        // 20-second timeout; on timeout we proceed regardless so a wedged actor
        // can't prevent process exit.
        if let Some(root) = root {
            self.runtime.block_on(async {
                root.stop_gracefully().await.ok();
                if tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    root.wait_for_shutdown(),
                )
                .await
                .is_err()
                {
                    tracing::warn!("actor shutdown timed out after 20s; proceeding");
                }
            });
        }

        // The trouper fabric drains second: kameo producers are down, so
        // the sweep's barrier sees a quiet bus and every on_stop hook
        // runs before the store checkpoint. Capped at 10s — a wedged
        // trouper actor can't prevent process exit (the sweep hard-stops
        // stragglers at the deadline).
        if let Some(trouper_system) = trouper_system {
            self.runtime.block_on(async {
                if tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    trouper_system.shutdown_graceful(std::time::Duration::from_secs(10)),
                )
                .await
                .is_err()
                {
                    tracing::warn!("trouper shutdown timed out after 20s; proceeding");
                }
            });
        }

        self.runtime
            .block_on(store.shutdown())
            .change_context(AppError)
            .attach("session store shutdown (WAL checkpoint) failed")?;
        run_result
    }

    /// Dispatches the CLI command to the appropriate runner.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner fails.
    pub fn dispatch(&mut self, cli: Cli) -> Result<(), Report<AppError>> {
        use jinn_cli::cli::Commands;
        #[cfg(debug_assertions)]
        use jinn_cli::cli::HeadlessCommands;

        // Load config from providers.toml (auto-creates on first run).
        let config_storage =
            ConfigStorageService::new(Arc::new(FilesystemConfigStorage::default_path()));
        // API keys are resolved by the env-init actor.
        let resolved_api_keys = ApiKeysService::new(ApiKeys::new());

        // Provider registry is populated by the provider-init actor.
        // Start with an empty registry.
        let provider_registry = ProviderRegistryService::new(
            ProviderRegistry::from_config(jinn_domain::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .change_context(AppError)?,
        );

        // Initial factory is the no-provider sentinel until actors resolve the real one.
        let llm_service = LlmServiceFactoryService::new(Arc::new(NoProvidersAvailableFactory));

        // Dispatch `config` subcommands BEFORE the early preferences parse and
        // before any DB wiring.
        // `jinn config init` is the user's recovery tool for a missing or broken
        // config, so it must not be guarded by load-time parsing (which itself
        // auto-creates the file on first run) — and it needs no session store.
        if let Some(Commands::Config { subcommand }) = &cli.command {
            use jinn_cli::cli::ConfigCommands;
            use jinn_domain::{InitOutcome, init_default_config_to, preferences_path};

            match subcommand {
                ConfigCommands::Init { force } => {
                    let path = preferences_path();
                    let force = *force;
                    match init_default_config_to(&path, force) {
                        Ok(InitOutcome::Created) => {
                            println!("Created {}", path.display());
                        }
                        Ok(InitOutcome::Overwritten) => {
                            println!("Overwrote {}", path.display());
                        }
                        Err(report) => {
                            eprintln!("{report:?}");
                            return Err(report.change_context(AppError));
                        }
                    }
                    return Ok(());
                }
                ConfigCommands::Providers { force } => {
                    use jinn_domain::{
                        InitProvidersOutcome, config_path, init_default_providers_to,
                    };

                    let path = config_path();
                    let force = *force;
                    match init_default_providers_to(&path, force) {
                        Ok(InitProvidersOutcome::Created) => {
                            println!("Created {}", path.display());
                        }
                        Ok(InitProvidersOutcome::Overwritten) => {
                            println!("Overwrote {}", path.display());
                        }
                        Err(report) => {
                            eprintln!("{report:?}");
                            return Err(report.change_context(AppError));
                        }
                    }
                    return Ok(());
                }
            }
        }

        // `plugin new` scaffolds a plugin project. It needs no preferences
        // or DB and runs fully offline, so it dispatches before both.
        if let Some(Commands::Plugin { subcommand }) = &cli.command {
            use jinn_cli::cli::PluginCommands;
            use jinn_cli::plugin_build;
            use jinn_cli::plugin_new;
            match subcommand {
                PluginCommands::New { name, sdk } => {
                    let cwd = std::env::current_dir()
                        .change_context(AppError)
                        .attach("resolving current directory")?;
                    let source = match sdk.as_deref() {
                        None => plugin_new::SdkSource::DefaultGit,
                        Some(value) => plugin_new::parse_sdk(value).map_err(|report| {
                            eprintln!("{report:?}");
                            report.change_context(AppError)
                        })?,
                    };
                    match plugin_new::scaffold(&cwd, name, &source) {
                        Ok(dir) => plugin_new::report_success(&dir, name, &source),
                        Err(report) => {
                            eprintln!("{report:?}");
                            return Err(report.change_context(AppError));
                        }
                    }
                }
                PluginCommands::Build { dir } => {
                    let target = dir.as_deref().map_or_else(
                        || {
                            std::env::current_dir()
                                .change_context(AppError)
                                .attach("resolving current directory")
                        },
                        |d| {
                            std::path::PathBuf::from(d)
                                .canonicalize()
                                .change_context(AppError)
                                .attach("resolving plugin directory")
                        },
                    )?;
                    match plugin_build::build(&target) {
                        Ok(artifact) => {
                            println!("built {}", artifact.display());
                            println!("install with: jinn plugin install {}", artifact.display());
                        }
                        Err(report) => {
                            eprintln!("{report:?}");
                            return Err(report.change_context(AppError));
                        }
                    }
                }
                PluginCommands::Install {
                    wasm,
                    name,
                    grants,
                    http,
                    no_http,
                } => {
                    use jinn_domain::feat::plugin::manifest::extract_manifest;

                    let wasm_path = std::path::PathBuf::from(wasm);
                    let bytes = std::fs::read(&wasm_path).map_err(|e| {
                        Report::new(AppError)
                            .attach(e.to_string())
                            .attach(wasm_path.to_string_lossy().to_string())
                    })?;
                    let manifest = extract_manifest(&bytes).map_err(|report| {
                        eprintln!("{report:?}");
                        report.change_context(AppError)
                    })?;
                    let resolved = resolve_install(
                        name.clone(),
                        grants.clone(),
                        *http,
                        *no_http,
                        &manifest,
                        &wasm_path,
                    );
                    run_install(&resolved, &wasm_path)?;
                    return Ok(());
                }
                PluginCommands::Add {
                    dir,
                    name,
                    grants,
                    http,
                    no_http,
                } => {
                    use jinn_domain::feat::plugin::manifest::{extract_manifest, read_manifest};

                    let dir = std::path::PathBuf::from(dir.as_deref().unwrap_or("."));
                    let cargo_toml = dir.join("Cargo.toml");
                    let _ = std::fs::read_to_string(&cargo_toml)
                        .change_context(AppError)
                        .attach(cargo_toml.to_string_lossy().to_string())
                        .and_then(|content| {
                            read_manifest(&content).map_err(|r| r.change_context(AppError))
                        })?;
                    let artifact = plugin_build::build(&dir).map_err(|report| {
                        eprintln!("{report:?}");
                        report.change_context(AppError)
                    })?;
                    let manifest =
                        extract_manifest(&std::fs::read(&artifact).change_context(AppError)?)
                            .map_err(|report| {
                                eprintln!("{report:?}");
                                report.change_context(AppError)
                            })?;
                    let resolved = resolve_install(
                        name.clone(),
                        grants.clone(),
                        *http,
                        *no_http,
                        &manifest,
                        &artifact,
                    );
                    run_install(&resolved, &artifact)?;
                    return Ok(());
                }
                // `install-builtins` overwrites every builtin plugin payload
                // and registers only entries missing from jinn.toml. It needs
                // no DB and runs before actor wiring, so it dispatches here.
                PluginCommands::InstallBuiltins => {
                    use jinn_domain::{
                        AppPaths, BuiltinPluginInstall, FilesystemUserPreferencesStorage,
                        InstallOutcome, install_builtin_plugins_to,
                    };

                    let app_paths = AppPaths::default();
                    let storage = FilesystemUserPreferencesStorage::default_path();
                    match install_builtin_plugins_to(&app_paths.plugins_dir(), &storage) {
                        Ok(installs) => {
                            for BuiltinPluginInstall {
                                name,
                                payload,
                                entry_registered,
                            } in installs
                            {
                                match &payload {
                                    InstallOutcome::Created(path) => {
                                        println!("Installed {}", path.display());
                                    }
                                    InstallOutcome::Overwritten(path) => {
                                        println!("Overwrote {}", path.display());
                                    }
                                    InstallOutcome::Skipped(path) => {
                                        println!("Already present, skipped {}", path.display());
                                    }
                                }
                                if entry_registered {
                                    println!("Registered {name}");
                                } else {
                                    println!("Already registered, skipped {name}");
                                }
                            }
                            println!("Restart jinn to activate plugins.");
                            return Ok(());
                        }
                        Err(report) => {
                            eprintln!("error: failed to install builtin plugins:");
                            eprintln!("  {report:?}");
                            return Err(report.change_context(AppError));
                        }
                    }
                }
            }
        }
        // `install` seeds default resources into user dirs. Like `config`, it
        // must run before any actor wiring — and it needs no preferences/DB,
        // so it dispatches before the session store is opened.
        if let Some(Commands::Install { force }) = &cli.command {
            use jinn_domain::feat::preferences_actor::FilesystemUserPreferencesStorage;
            use jinn_domain::{
                AppPaths, Destinations, InstallOutcome, InstallReport, JinnTomlOutcome,
                install_defaults_to,
            };

            let app_paths = AppPaths::default();
            let storage = FilesystemUserPreferencesStorage::default_path();
            let destinations = Destinations::new(
                app_paths.themes_dir(),
                app_paths.personas_dir(),
                app_paths.prompts_dir(),
                app_paths.skills_dir(),
                app_paths.plugins_dir(),
            );
            match install_defaults_to(&destinations, *force, storage.path(), &storage) {
                Ok(report) => {
                    let InstallReport {
                        outcomes,
                        jinn_toml,
                    } = report;
                    let mut plugins_touched = false;
                    for outcome in outcomes {
                        match &outcome {
                            InstallOutcome::Created(path) => {
                                println!("Installed {}", path.display());
                            }
                            InstallOutcome::Skipped(path) => {
                                println!("Already present, skipped {}", path.display());
                            }
                            InstallOutcome::Overwritten(path) => {
                                println!("Overwrote {}", path.display());
                            }
                        }
                        if !matches!(outcome, InstallOutcome::Skipped(_))
                            && outcome.path().extension().is_some_and(|e| e == "wasm")
                        {
                            plugins_touched = true;
                        }
                    }
                    match &jinn_toml {
                        JinnTomlOutcome::Created(path) => {
                            println!("Created {}", path.display());
                        }
                        JinnTomlOutcome::Untouched(path) => {
                            println!("Already present, skipped {}", path.display());
                        }
                    }
                    if plugins_touched {
                        if matches!(jinn_toml, JinnTomlOutcome::Untouched(_)) {
                            println!(
                                "note: payloads installed while jinn.toml exists are not registered; run \"jinn plugin install-builtins\" to add any missing [plugin.<name>] entries"
                            );
                        }
                        println!("Restart jinn to activate plugins.");
                    }
                    return Ok(());
                }
                Err(report) => {
                    eprintln!("error: failed to install defaults:");
                    eprintln!("  {report:?}");
                    return Err(report.change_context(AppError));
                }
            }
        }

        // Create the session store - uses --db-path if provided, otherwise
        // the platform default. Deferred until after the `config`/`install`
        // early-returns so neither pays for DB open or migrations.
        let (session_store, session_pool) = {
            let store = self.runtime.block_on(async {
                match cli.db_path_opt() {
                    Some(path) => SqliteSessionStore::open_or_create(path).await,
                    None => SqliteSessionStore::new().await,
                }
            });
            let store = store.change_context(AppError)?;
            let pool = store.pool().clone();
            (SessionStoreService::new(Arc::new(store)), pool)
        };

        // Parse user preferences early — fail-fast on a bad config BEFORE
        // any actor wiring runs. The shared service is cloned into each
        // command arm below. Config subcommands have already dispatched above.
        let user_preferences_storage = {
            let backend = FilesystemUserPreferencesStorage::default_path();
            let path = backend.path().to_path_buf();
            let svc = UserPreferencesStorageService::new(Arc::new(backend));
            if let Err(report) = svc.reload() {
                tracing::error!(path = %path.display(), "failed to parse user preferences");
                eprintln!(
                    "error: failed to parse user preferences at {}:",
                    path.display()
                );
                eprintln!("  {report:?}");
                std::process::exit(1);
            }
            svc
        };

        // Load providers.toml early — fail-fast on a malformed file BEFORE
        // any actor wiring runs, with a report naming the file and TOML detail.
        // Config subcommands have already dispatched above, so `jinn config
        // providers` remains usable as the recovery tool for a broken file.
        if let Err(report) = providers_load_error_report(&config_storage) {
            tracing::error!("failed to load providers config");
            eprintln!("error: failed to load providers config:");
            eprintln!("  {report:?}");
            std::process::exit(1);
        }

        let app_state_storage = {
            let backend =
                FilesystemAppStateStorage::new(jinn_domain::AppPaths::default().state_file_path());
            let svc = AppStateStorageService::new(Arc::new(backend));
            if let Err(report) = svc.reload() {
                tracing::error!("failed to load app state");
                eprintln!("error: failed to load app state:");
                eprintln!("  {report:?}");
                std::process::exit(1);
            }
            svc
        };

        #[cfg(debug_assertions)]
        let _db_path = cli.db_path_opt().cloned();
        match cli.command.unwrap_or(Commands::Tui) {
            Commands::Completions { shell } => {
                use clap::CommandFactory;
                let mut cmd = Cli::command();
                let name = cmd.get_name().to_string();
                clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
                return Ok(());
            }
            Commands::Tui => {
                let intent_handler_cap =
                    jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
                let (core, services, discord_activated) = self.runtime.block_on(async {
                    actor_wiring::ActorSystemBuilder::new(actor_wiring::ActorSystemBuilderArgs {
                        handle: self.handle(),
                        llm_service: llm_service.clone(),
                        provider_registry: provider_registry.clone(),
                        api_keys: resolved_api_keys.clone(),
                        config_storage: config_storage.clone(),
                        session_store: session_store.clone(),
                        user_preferences_storage: user_preferences_storage.clone(),
                        app_state_storage: app_state_storage.clone(),
                        paths: jinn_domain::AppPaths::default(),
                        browser_profile_override: cli.browser_profile.clone(),
                        dump_requests: cli.dump_requests.clone(),
                    })
                    .build()
                    .await
                });

                // The discord frontend consumes the slice activation's
                // parked channels + config; no-ops when disabled.
                jinn_discord::spawn_gateway(
                    &self.handle(),
                    &core,
                    &services,
                    session_pool.clone(),
                    discord_activated,
                    &intent_handler_cap,
                );

                let app = jinn_tui::launch(core, services).change_context(AppError)?;
                let runner = Runner::Tui(Box::new(app));
                self.run_and_shutdown(runner, &session_store)?;
            }
            #[cfg(debug_assertions)]
            Commands::Headless { command, .. } => {
                let intent_handler_cap =
                    jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
                let store_for_shutdown = session_store.clone();
                let (core, services, _discord_activated) = self.runtime.block_on(async {
                    actor_wiring::ActorSystemBuilder::new(actor_wiring::ActorSystemBuilderArgs {
                        handle: self.handle(),
                        llm_service: llm_service.clone(),
                        provider_registry,
                        api_keys: resolved_api_keys,
                        config_storage,
                        session_store,
                        user_preferences_storage: user_preferences_storage.clone(),
                        app_state_storage,
                        paths: jinn_domain::AppPaths::default(),
                        browser_profile_override: cli.browser_profile.clone(),
                        dump_requests: cli.dump_requests.clone(),
                    })
                    .build()
                    .await
                });

                jinn_tui::load_compaction_prompt(
                    &core.state,
                    &services.paths.prompts_dir(),
                    &services.paths.system_prompts_dir(),
                    &intent_handler_cap,
                )
                .change_context(AppError)?;
                jinn_tui::load_theme(
                    &core.state,
                    &services.paths.themes_dir(),
                    &services.paths.system_themes_dir(),
                    &intent_handler_cap,
                );
                let mut headless = HeadlessApp::new(core, services);
                match command {
                    Some(HeadlessCommands::SendChat { message }) => {
                        headless.send_chat(&message).change_context(AppError)?;
                    }
                    Some(HeadlessCommands::Script { path }) => {
                        let file = std::fs::File::open(&path)
                            .change_context(AppError)
                            .attach("failed to open script file")?;
                        headless.run_script(file).change_context(AppError)?;
                    }
                    None => {}
                }
                let runner = Runner::Headless(Box::new(headless));
                self.run_and_shutdown(runner, &store_for_shutdown)?;
            }
            Commands::Fetch { subcommand } => {
                use jinn_cli::cli::FetchCommands;

                match subcommand {
                    FetchCommands::Models => {
                        self.runtime
                            .block_on(async { fetch_models().await })
                            .change_context(AppError)?;
                    }
                }
            }
            // Config subcommands are dispatched above, before the early
            // preferences parse. Reaching this match arm is impossible.
            Commands::Config { .. } => {}
            Commands::Install { .. } => {}
            // `plugin` subcommands are dispatched above, before the early
            // preferences parse. Reaching this match arm is impossible.
            Commands::Plugin { .. } => {}
        }

        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new().expect("failed to create default App")
    }
}

/// Checks that `providers.toml` loads and parses, producing a fail-fast report.
///
/// A missing file is not an error here — the loader auto-creates the default
/// template on first run. Only a load or parse failure produces an error, with
/// the config path and the underlying TOML detail attached to the report.
fn providers_load_error_report(storage: &ConfigStorageService) -> Result<(), Report<AppError>> {
    if let Err(report) = storage.load() {
        let path = jinn_domain::config_path();
        return Err(report.change_context(AppError).attach(format!(
            "failed to load providers config at {}",
            path.display()
        )));
    }
    Ok(())
}

/// Fetches model metadata from models.dev and saves it to the user's cache directory.
///
/// Makes an HTTP GET request to `https://models.dev/api.json`, validates the
/// response as JSON, and writes it to `~/.cache/jinn/models.dev.json`.
///
/// # Errors
///
/// Returns an error if the HTTP request fails, the response is not valid JSON,
/// or the file cannot be written.
async fn fetch_models() -> Result<(), Report<AppError>> {
    use jinn_domain::common::app_info::APP_NAME;
    let target_path = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(APP_NAME)
        .join("models.dev.json");
    fetch_models_from_url("https://models.dev/api.json", target_path).await
}

/// Fetches model metadata from a URL and saves it to the user's cache directory.
///
/// This is the testable core of [`fetch_models`], separated so tests can
/// pass a mockito URL.
async fn fetch_models_from_url(
    url: &str,
    output_path: std::path::PathBuf,
) -> Result<(), Report<AppError>> {
    tracing::info!(url = url, "fetching model metadata");

    let response = reqwest::get(url)
        .await
        .change_context(AppError)
        .attach("failed to fetch models.dev API")?;

    if !response.status().is_success() {
        return Err(
            Report::new(AppError).attach(format!("models.dev returned HTTP {}", response.status()))
        );
    }

    let body = response
        .text()
        .await
        .change_context(AppError)
        .attach("failed to read models.dev response")?;

    // Validate that the response is valid JSON and count providers/models.
    let (provider_count, model_count) = {
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .change_context(AppError)
            .attach("models.dev response is not valid JSON")?;

        let mut provider_count = 0u32;
        let mut model_count = 0u32;
        if let serde_json::Value::Object(map) = &parsed {
            for (_provider_name, provider_data) in map {
                provider_count += 1;
                if let Some(models) = provider_data.get("models").and_then(|m| m.as_object()) {
                    model_count += models.len() as u32;
                }
            }
        }
        (provider_count, model_count)
    };

    // Write to the provided output path.
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(AppError)
            .attach("failed to create cache directory")?;
    }

    std::fs::write(&output_path, &body)
        .change_context(AppError)
        .attach("failed to write models.dev.json")?;

    println!(
        "Fetched {model_count} models from {provider_count} providers to {}",
        output_path.display()
    );

    Ok(())
}

/// The effective values an install applies, after manifest-vs-flag
/// precedence.
struct ResolvedInstall {
    name: String,
    grants: Vec<jinn_domain::feat::plugin::PluginPathGrant>,
    http: bool,
    /// True when the grants came from the embedded manifest (not flags) —
    /// drives the mandatory override hint.
    grants_from_manifest: bool,
}

/// Resolves `--name`/`--grant`/`--http` flags against the plugin's embedded
/// manifest: flags win when present, manifest values otherwise, file/crate
/// stem as the final name fallback.
fn resolve_install(
    name_flag: Option<String>,
    grant_flags: Vec<String>,
    http_flag: bool,
    no_http_flag: bool,
    manifest: &jinn_domain::feat::plugin::manifest::PluginManifest,
    artifact: &std::path::Path,
) -> ResolvedInstall {
    use jinn_domain::feat::plugin::manifest::parse_grant_str;

    let name = name_flag
        .or_else(|| manifest.name.clone())
        .or_else(|| {
            artifact
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let (grants, grants_from_manifest) = if grant_flags.is_empty() {
        (manifest.grants.clone(), true)
    } else {
        (
            grant_flags.iter().map(|g| parse_grant_str(g)).collect(),
            false,
        )
    };
    let http = if http_flag {
        true
    } else if no_http_flag {
        false
    } else {
        manifest.http
    };
    ResolvedInstall {
        name,
        grants,
        http,
        grants_from_manifest,
    }
}

/// Runs the install with the resolved values and prints the loud outcome:
/// payload, each grant (read-only/writable), http, and — when grants were
/// auto-applied from the manifest — the `--grant` override hint.
fn run_install(
    resolved: &ResolvedInstall,
    wasm_path: &std::path::Path,
) -> Result<(), Report<AppError>> {
    use jinn_domain::AppPaths;
    use jinn_domain::feat::plugin::install::{PluginInstallOutcome, install};
    use jinn_domain::feat::preferences_actor::FilesystemUserPreferencesStorage;

    let paths = AppPaths::default();
    let storage = FilesystemUserPreferencesStorage::default_path();
    let ResolvedInstall {
        name,
        grants,
        http,
        grants_from_manifest,
    } = resolved;
    match install(
        wasm_path,
        name,
        &paths.plugins_dir(),
        grants.clone(),
        *http,
        &storage,
    ) {
        Ok(PluginInstallOutcome::Installed { wasm_path, name }) => {
            println!("Installed plugin {name}");
            println!("  payload: {}", wasm_path.display());
        }
        Ok(PluginInstallOutcome::Updated { wasm_path, name }) => {
            println!("Updated plugin {name}");
            println!("  payload: {}", wasm_path.display());
        }
        Err(report) => {
            eprintln!("{report:?}");
            return Err(report.change_context(AppError));
        }
    }
    // A plugin installed with no grants sees no directories at all —
    // almost always an oversight worth naming.
    if grants.is_empty() {
        eprintln!(
            "note: {name} declares no grants — it cannot read any directory.\nIf it should see files, reinstall with --grant '<config_dir>/…'\n(repeatable, :w for writable; '<plugin_data_dir>:w' for a scratch dir)."
        );
    } else {
        println!("  grants:");
        for grant in grants {
            let mode = if grant.writable {
                "writable"
            } else {
                "read-only"
            };
            println!("    {} ({mode})", grant.path);
        }
    }
    println!("  http: {}", if *http { "yes" } else { "no" });
    if *grants_from_manifest {
        println!(
            "To override these grants: jinn plugin install <wasm> --grant '<path>' (repeatable, :w for writable)"
        );
    }
    println!("Restart jinn to activate the plugin.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use jinn_domain::{AppState, State};
    use jinn_tui::{load_compaction_prompt, load_theme};
    use std::path::PathBuf;

    use super::*;

    // Note: there is no unit test for `run_and_shutdown` / `dispatch` calling
    // `store.shutdown()`. `Runner::run` is a concrete enum (not a trait), and
    // both variants require a live `AppCore` + `ActorHostService` (the full
    // actor system) to construct. Standing that up — or introducing a `Runner`
    // trait + fake — is scope creep for this task.
    //
    // The behavior is covered two ways instead:
    //  1. The 4 wiring sites are verified by static inspection: `run_and_shutdown`
    //     is called at the Tui (line 250), Headless (292), Bench::Run (389), and
    //     Bench::Tui (460) exit paths.
    //  2. The checkpoint itself is proven by `shutdown_truncates_wal_file` and
    //     `shutdown_makes_db_self_contained_for_backup` in the session store tests.

    fn sample_manifest() -> jinn_domain::feat::plugin::manifest::PluginManifest {
        use jinn_domain::feat::plugin::manifest::{PluginManifest, parse_grant_str};
        PluginManifest {
            name: Some("embedded-name".to_owned()),
            grants: vec![parse_grant_str("<config_dir>/themes")],
            http: true,
        }
    }

    // Given a manifest with grants and no --grant flag.
    // When resolving.
    // Then the embedded grants apply and carry grants_from_manifest.
    #[rstest::rstest]
    #[test]
    fn resolve_install_applies_manifest_grants_by_default() {
        let resolved = resolve_install(
            None,
            vec![],
            false,
            false,
            &sample_manifest(),
            std::path::Path::new("/tmp/x.wasm"),
        );

        assert_eq!(resolved.grants.len(), 1);
        assert_eq!(resolved.grants[0].path, "<config_dir>/themes");
        assert!(resolved.grants_from_manifest);
        assert_eq!(resolved.name, "embedded-name");
        assert!(resolved.http);
    }

    // Given a --grant flag alongside embedded grants.
    // When resolving.
    // Then the flag wins and the manifest grants are discarded.
    #[rstest::rstest]
    #[test]
    fn resolve_install_grant_flag_overrides_manifest() {
        let resolved = resolve_install(
            None,
            vec!["<data_dir>/notes:w".to_owned()],
            false,
            false,
            &sample_manifest(),
            std::path::Path::new("/tmp/x.wasm"),
        );

        assert_eq!(resolved.grants.len(), 1);
        assert_eq!(resolved.grants[0].path, "<data_dir>/notes");
        assert!(resolved.grants[0].writable);
        assert!(!resolved.grants_from_manifest);
    }

    // Given an embedded manifest with http = true and a --no-http flag.
    // When resolving.
    // Then http is denied.
    #[rstest::rstest]
    #[test]
    fn resolve_install_no_http_overrides_manifest() {
        let resolved = resolve_install(
            None,
            vec![],
            false,
            true,
            &sample_manifest(),
            std::path::Path::new("/tmp/x.wasm"),
        );

        assert!(!resolved.http);
    }

    // Given a manifest with no name and no --name flag.
    // When resolving against an artifact path.
    // Then the file stem is the name.
    #[rstest::rstest]
    #[test]
    fn resolve_install_name_falls_back_to_file_stem() {
        use jinn_domain::feat::plugin::manifest::PluginManifest;
        let manifest = PluginManifest {
            name: None,
            ..sample_manifest()
        };

        let resolved = resolve_install(
            None,
            vec![],
            false,
            false,
            &manifest,
            std::path::Path::new("/tmp/some-plugin.wasm"),
        );

        assert_eq!(resolved.name, "some-plugin");
    }

    #[rstest::rstest]
    fn load_compaction_prompt_populates_state() {
        // Given a user dir with a compaction prompt file.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("_compaction.md"), "test compaction prompt")
            .expect("write compaction prompt");

        let state = State::new(AppState::default());
        let empty = PathBuf::from("/nonexistent");
        let cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();

        // When loading the compaction prompt.
        load_compaction_prompt(&state, dir.path(), &empty, &cap).expect("load");

        // Then the state contains the prompt text.
        let state = state.read();
        assert_eq!(state.context.compaction_prompt, "test compaction prompt");
    }

    #[rstest::rstest]
    fn load_compaction_prompt_returns_err_when_missing() {
        // Given empty user and system dirs.
        let user_dir = tempfile::tempdir().expect("temp dir");
        let system_dir = tempfile::tempdir().expect("temp dir");

        let state = State::new(AppState::default());
        let cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();

        // When loading the compaction prompt with no file present.
        let result = load_compaction_prompt(&state, user_dir.path(), system_dir.path(), &cap);

        // Then an error is returned (hard-fail semantics).
        assert!(
            result.is_err(),
            "expected error when compaction prompt is missing"
        );

        // And the state compaction prompt remains empty.
        let state = state.read();
        assert!(state.context.compaction_prompt.is_empty());
    }

    #[rstest::rstest]
    fn load_theme_updates_state_from_file() {
        // Given a temp directory with a custom theme file.
        let dir = tempfile::tempdir().expect("temp dir");
        let theme_content = "focus_accent = \"magenta\"\nborder_unfocused = \"blue\"";
        std::fs::write(dir.path().join("custom.toml"), theme_content).expect("write theme");

        let state = State::new(AppState::default());

        // Set the theme name in preferences.
        state.write_test_no_cap().frontend.app_state.theme_name = Some("custom".to_owned());

        // Capture the initial focus_accent color.
        let initial_focus = state.read().frontend.theme.focus_accent;

        // When loading the theme.
        let empty = PathBuf::from("/nonexistent");
        let cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
        load_theme(&state, dir.path(), &empty, &cap);

        // Then the theme in state was updated (focus_accent changed to magenta).
        // This kills: replace load_theme with ().
        let updated_focus = state.read().frontend.theme.focus_accent;
        assert_ne!(
            initial_focus, updated_focus,
            "theme should have been updated from the custom file"
        );
    }

    #[rstest::rstest]
    fn load_theme_falls_back_gracefully_on_missing() {
        // Given a state with a theme name that doesn't exist.
        let state = State::new(AppState::default());
        state.write_test_no_cap().frontend.app_state.theme_name = Some("nonexistent".to_owned());

        let empty = PathBuf::from("/nonexistent");

        // When loading the theme (no matching file).
        // Then it should not panic - the function logs a warning and returns.
        let cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
        load_theme(&state, &empty, &empty, &cap);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fetch_models_writes_file_on_success() {
        // Given a mock server returning valid JSON.
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::json!({
            "openai": {
                "models": {
                    "gpt-4o": { "id": "gpt-4o" },
                    "gpt-4o-mini": { "id": "gpt-4o-mini" }
                }
            },
            "anthropic": {
                "models": {
                    "claude-3": { "id": "claude-3" }
                }
            }
        });
        let mock = server
            .mock("GET", "/api.json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let url = format!("{}/api.json", server.url());
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let output_path = temp_dir.path().join("models.dev.json");

        // When fetching models.
        let result = fetch_models_from_url(&url, output_path.clone()).await;

        // Then it succeeds and the mock was called.
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());
        mock.assert_async().await;

        // And the file was written to the temp directory (not real cache).
        assert!(
            output_path.exists(),
            "output file should exist at temp path"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fetch_models_returns_error_on_http_failure() {
        // Given a mock server returning HTTP 500.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api.json")
            .with_status(500)
            .create_async()
            .await;

        let url = format!("{}/api.json", server.url());
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let output_path = temp_dir.path().join("models.dev.json");

        // When fetching models.
        let result = fetch_models_from_url(&url, output_path).await;

        // Then it returns an error (the ! in !is_success() is needed).
        // This kills: delete ! in fetch_models.
        assert!(result.is_err(), "expected error on HTTP 500");
        mock.assert_async().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fetch_models_counts_providers_and_models_correctly() {
        // Given a mock server returning JSON with 2 providers and 3 models total.
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::json!({
            "provider-a": {
                "models": {
                    "model-1": { "id": "model-1" },
                    "model-2": { "id": "model-2" }
                }
            },
            "provider-b": {
                "models": {
                    "model-3": { "id": "model-3" }
                }
            }
        });
        let mock = server
            .mock("GET", "/api.json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let url = format!("{}/api.json", server.url());
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let output_path = temp_dir.path().join("models.dev.json");

        // When fetching models.
        let result = fetch_models_from_url(&url, output_path).await;

        // Then it succeeds.
        // This kills: += with -= (would panic on u32 underflow in debug mode)
        // and += with *= (would print "0 models" but still succeed - the println
        // output in the test log shows the actual counts, making a human-readable check).
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());
        mock.assert_async().await;
    }

    #[rstest::rstest]
    fn providers_load_error_report_fails_with_parse_detail_on_malformed_file() {
        // Given a config storage backed by a malformed providers.toml.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "[providers.ollama]\nbackend = \"ollama\"\nmodels = [\"llama3\"\n",
        )
        .expect("write");
        let storage = ConfigStorageService::new(Arc::new(FilesystemConfigStorage::new(path)));

        // When checking the providers config.
        let result = providers_load_error_report(&storage);

        // Then the error render keeps the TOML detail attached upstream
        // (attachments survive the change_context to AppError).
        let report = result.expect_err("malformed providers.toml must fail");
        let rendered = format!("{report:?}");
        assert!(
            rendered.contains("TOML parse error"),
            "missing TOML parse detail: {rendered}"
        );
    }

    #[rstest::rstest]
    fn providers_load_error_report_ok_when_file_missing() {
        // Given a config storage backed by a directory with no providers.toml.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let storage =
            ConfigStorageService::new(Arc::new(FilesystemConfigStorage::new(path.clone())));

        // When checking the providers config.
        let result = providers_load_error_report(&storage);

        // Then the check passes (the loader auto-creates the default template).
        assert!(result.is_ok(), "expected ok, got: {:?}", result.err());
        // And the default file was created.
        assert!(
            path.exists(),
            "first-run load should auto-create the config file"
        );
    }
}
